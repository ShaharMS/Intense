"""Verify retained codewords against original JP2s and bound single-range GET traffic."""
import argparse
import ast
import hashlib
import json
import re
from pathlib import Path


def merge(spans):
    result = []
    for a, b in sorted(spans):
        if result and a <= result[-1][1]:
            result[-1][1] = max(result[-1][1], b)
        else:
            result.append([a, b])
    return result


def uncovered(spans, covered):
    result = []
    for a, b in merge(spans):
        for lo, hi in covered:
            if lo >= b:
                break
            if hi <= a:
                continue
            if a < lo:
                result.append((a, lo))
            a = max(a, hi)
        if a < b:
            result.append((a, b))
    return result


def body_ranges(events):
    spans = []
    for event in events:
        if event['body']:
            a, b = map(int, re.fullmatch(r'bytes=(\d+)-(\d+)', event['range']).groups())
            spans.append((a, b + 1))
    return spans


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('measurements', type=Path)
parser.add_argument('output', type=Path)
parser.add_argument('--corpus', type=Path, default=Path('corpus'))
args = parser.parse_args()
rows = [r for r in json.loads(args.measurements.read_text()) if
        r['build'] == 'after' and r['phase'] == 'firstFull' and '-single' in r['image']]
lines = [line for line in args.measurements.with_suffix('.log').read_text().splitlines()
         if 'message="codeword range audit"' in line]
result = []
for row, line in zip(rows, lines, strict=True):
    want, hits, digest = re.search(r'want="([^"]+)" hits="([^"]+)" sha256="([0-9a-f]+)"', line).groups()
    want, hits = map(ast.literal_eval, (want, hits))
    checksum = hashlib.sha256()
    with (args.corpus / row['image']).open('rb') as image:
        size = image.seek(0, 2)
        assert all(0 <= a < b <= size for a, b in want + hits)
        for a, b in hits:
            image.seek(a)
            checksum.update(image.read(b - a))
    assert checksum.hexdigest() == digest
    prior = merge(body_ranges(row['prior_ranges']))
    actual = body_ranges(row['origin_ranges'])
    fresh = uncovered(want, prior)
    assert not uncovered(fresh, merge(actual))
    fresh_bytes = sum(b-a for a,b in fresh)
    gaps = sorted(b[0] - a[1] for a, b in zip(fresh, fresh[1:]))
    bounds = {k: fresh_bytes + sum(gaps[:max(0,len(fresh)-k)])
              for k in (1, 8, 16, 32, len(actual))}
    result.append(dict(image=row['image'], driver=row['driver'], retained=len(hits),
                       retained_bytes=sum(b-a for a,b in hits), retained_sha256=digest,
                       missing_codeword_bytes=sum(b-a for a,b in want), fresh_bytes=fresh_bytes,
                       fresh_spans=len(fresh), minimum_bytes_by_get_count=bounds,
                       gets=len(actual), actual_bytes=row['bytes']))
args.output.write_text(json.dumps(result, indent=2)+'\n')
