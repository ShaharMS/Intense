"""Compare cold-range latency sweeps with independently parsed source SOTs."""
import argparse,json,pathlib,struct
p=argparse.ArgumentParser();p.add_argument('--source',type=pathlib.Path,required=True);p.add_argument('--reports',type=pathlib.Path,required=True);a=p.parse_args()
parts=[]
with a.source.open('rb') as source:
    offset=source.read(65536).find(b'\xff\x90\x00\x0a');assert offset>=0
    while True:
        source.seek(offset);header=source.read(12)
        if header[:2]==b'\xff\xd9':break
        assert header[:4]==b'\xff\x90\x00\x0a',f'Invalid SOT at {offset}'
        tile,length,ordinal,total=struct.unpack('>HIBB',header[4:]);assert length>=14
        parts.append(dict(offset=offset,length=length,tile=tile));offset+=length
rows=[]
for path in sorted(a.reports.glob('rtt*.json')):
    report=json.loads(path.read_text());cold=next(r for r in report['measurements'] if r['state']=='cold');repeat=next(r for r in report['measurements'] if r['state']=='repeat');events=report['rangeEvents']
    assert cold['previewPixels']==repeat['previewPixels']=='exact'
    sot_events=[e for e in events if any(e['start']<=part['offset'] and e['end']>=part['offset']+11 for part in parts)]
    ordered=sorted(sot_events,key=lambda e:e['startedAt'])
    overlaps=sum(right['startedAt']<left['readyAt'] for left,right in zip(ordered,ordered[1:]))
    rows.append(dict(delayMs=report['rangeLatencyMs'],coldCoverageMs=cold['previewMs'],firstNonBlackMs=cold['firstNonBlackMs'],repeatCoverageMs=repeat['previewMs'],sourceBytes=cold['sourceReadBytes'],rangeRequests=cold['rangeRequests'],sotCoveringGets=len(sot_events),overlappingSotGets=overlaps,repeatSourceBytes=repeat['sourceReadBytes']))
result=dict(scope='Checkpoint 122; one pilot per artificial GET delay, fresh S3 identities, 979 MB unchanged original, HTTP/1.1, OS cache warm, encoded caches disabled. Exact independent reference pixels in every cold and repeat view. Timing is diagnostic, not a statistical before/after claim.',sourceBytes=a.source.stat().st_size,tileParts=len(parts),measurements=rows,decision='Serial source tile-part discovery is the full-preview cold critical path. The previous tile-part length identifies the next SOT offset. Driver changes cannot remove these source RTTs. No new image for an unverified runtime change.')
(a.reports/'summary.json').write_text(json.dumps(result,indent=2));print(json.dumps(result,indent=2))
