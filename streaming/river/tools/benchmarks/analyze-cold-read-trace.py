"""Audit recorded cold-read traffic without contacting the source.

Counts HTTP send records, not their overlapping logical parent records.
Coverage is requested byte coverage; response payload bytes come from the origin.
"""
import argparse
import collections
import hashlib
import json
from pathlib import Path


def union_size(spans):
    end = -1
    total = 0
    for start, stop in sorted(spans):
        if stop < start:
            raise ValueError('Invalid range')
        total += max(0, stop - max(start, end))
        end = max(end, stop)
    return total


def contiguous_frontier(spans, counts=(5, 10, 16, 32, 64)):
    """Optimistic offline bound: exact future offsets are already known.

    Joining the smallest gaps minimizes bytes for a fixed count of ordinary
    contiguous HTTP ranges. This is not a runnable discovery algorithm.
    """
    merged = []
    for start, stop in sorted(spans):
        if merged and start <= merged[-1][1]:
            merged[-1][1] = max(stop, merged[-1][1])
        else:
            merged.append([start, stop])
    useful = sum(b-a for a, b in merged)
    gaps = sorted(merged[i][0]-merged[i-1][1] for i in range(1, len(merged)))
    return [dict(maxGets=count, minimumRequestedBytes=useful+sum(gaps[:max(0,len(merged)-count)]),
                 requiredRangeBytes=useful) for count in counts]


def audit(row):
    trace = row['sourceReads']
    events = trace['events']
    sends = [e for e in events if e['kind'] == 'wireAttempt' or
             (e['kind'] == 'rangeRead' and e['purpose'] == 'nativeAttempt')]
    spans = [(e['start'], e['endExclusive']) for e in sends]
    requested = sum(b-a for a, b in spans)
    unique = union_size(spans)
    purposes = {}
    for purpose in sorted({e['purpose'] for e in sends}):
        selected = [e for e in sends if e['purpose'] == purpose]
        purposes[purpose] = dict(sends=len(selected),
            requestedBytes=sum(e['endExclusive']-e['start'] for e in selected))
    evicted = trace.get('evicted', 0)
    complete = evicted == 0 and len(sends) == row['wire']['gets']
    return dict(case=row['case'], version=row['version'],
        traceCompleteAgainstOrigin=complete, evictedEvents=evicted,
        origin=row['wire'], recordedSends=len(sends), purposes=purposes,
        requestedBytes=requested, uniqueRequestedBytes=unique,
        overlappingRequestedBytes=requested-unique,
        contiguousRangeLowerBound=contiguous_frontier(spans),
        repeatedExactRanges=sum(n-1 for n in collections.Counter(spans).values()),
        validatedPartRecords=sum(e['kind']=='indexEntry' for e in events),
        sourceMs=row.get('sourceMs'), decodeMs=row.get('decodeMs'))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    assert union_size([(0, 10), (5, 15), (15, 20), (0, 10)]) == 20
    assert contiguous_frontier([(0, 2), (10, 12), (30, 32)], (1, 2, 3)) == [
        dict(maxGets=1, minimumRequestedBytes=32, requiredRangeBytes=6),
        dict(maxGets=2, minimumRequestedBytes=14, requiredRangeBytes=6),
        dict(maxGets=3, minimumRequestedBytes=6, requiredRangeBytes=6)]
    raw = args.input.read_bytes()
    report = json.loads(raw)
    result = dict(inputSha256=hashlib.sha256(raw).hexdigest(),
        scope='Recorded smoke requests only. Requested-range overlap is not necessarily waste: retries and header/payload reuse can overlap. No source contact or new latency measurement.',
        rows=[audit(row) for row in report['smoke']])
    args.output.write_text(json.dumps(result, indent=2)+'\n', encoding='utf-8')
    for row in result['rows']:
        print(row['case'], row['version'], 'GETs', row['recordedSends'],
              'overlapBytes', row['overlappingRequestedBytes'],
              'exactRepeats', row['repeatedExactRanges'],
              'complete', row['traceCompleteAgainstOrigin'])
