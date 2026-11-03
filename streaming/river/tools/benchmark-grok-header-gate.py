"""Compare color-pipeline fallback without touching original source images."""
import io,json,pathlib,statistics,time,urllib.parse,urllib.request
from PIL import Image
root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/grok-header-gate-m124';catalog=json.loads((out/'catalog.json').read_text())['assets'];expected=Image.open(out/'expected.png').convert('RGBA');rows=[]
for trial in range(9):
    for label,port in ([('old',18896),('new',18897)] if trial%2==0 else [('new',18897),('old',18896)]):
        for entry in catalog:
            query=urllib.parse.urlencode(dict(edge='crop',image=entry['assetId'],driver='jp2grok'))
            started=time.perf_counter()
            with urllib.request.urlopen(f'http://127.0.0.1:{port}/streamer/v1/5/0/0/tile.png?'+query,timeout=60) as response:
                body=response.read();decode=float(response.headers['x-river-decode-ms']);route=response.headers['x-river-decoder-route']
            elapsed=(time.perf_counter()-started)*1000;image=Image.open(io.BytesIO(body)).convert('RGBA');assert image.size==expected.size and image.tobytes()==expected.tobytes()
            rows.append(dict(trial=trial,version=label,case=entry['source'],ms=elapsed,decodeMs=decode,route=route,pixels='exact'))
            print(label,entry['source'],trial,round(elapsed,2),flush=True)
report=dict(baseline='checkpoint 122',candidate='checkpoint 124',scope='Nine alternating requests per case/version, local read-only lossless 2048 RGB noise fixtures with and without valid sRGB ICC. Full-resolution 512 ROI. Encoded caches disabled, OS cache warm. First process decode shown separately; repeated requests measure uncached rendering, not source-cold IO.',measurements=rows,summary=[dict(case=case,**{label:{key:statistics.median(r[key] for r in rows if r['case']==case and r['version']==label and r['trial']>0) for key in ['ms','decodeMs']} for label in ['old','new']}) for case in ['plain.jp2','icc.jp2']],pixels='Every view equals independent lossless source RGB pixels')
(out/'summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report['summary'],indent=2))
