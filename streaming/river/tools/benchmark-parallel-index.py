"""Matched release-image cold/repeat measurements; serial alternating trials."""
import argparse,json,pathlib,statistics,subprocess,sys
p=argparse.ArgumentParser();p.add_argument('--checkpoint',type=int,default=182);p.add_argument('--baseline',type=int,default=178);p.add_argument('--old-api',default='http://127.0.0.1:18894');p.add_argument('--cases',nargs='+',default=['tile-rtt10','ws-rtt10','tile-local','tile-rtt50']);a=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[1];out=root/('reports/index-shards-m'+str(a.checkpoint));out.mkdir(exist_ok=True)
api={'old':a.old_api,'new':'http://127.0.0.1:18896'};results={}
for case,latency,runs in [('tile-rtt10',10,3),('ws-rtt10',10,3),('tile-local',0,3),('tile-rtt50',50,1)]:
 if case not in a.cases:continue
 for run in range(runs):
  for version in (['old','new'] if run%2==0 else ['new','old']):
   path=out/f'{case}-{version}-{run}.json'
   if case.startswith('ws'):
    command=[sys.executable,str(root/'tools/benchmark-s3-corpus.py'),'--api',api[version],'--driver','openjpeg','--objects','large','--origin-http11','--range-latency-ms',str(latency),'--progressive-patches','--repeat-runs','2','--verify-preview-pixels','--preview-reference',str(root/'reports/grok-large-reference-m54.ppm'),'--output',str(path)]
   else:
    command=[sys.executable,str(root/'tools/compare-gdal-random-tile.py'),'--api',api[version],'--source',str(root/'corpus/cache/sentinel-date-mosaics/2016-09-12.jp2'),'--x','55','--y','13','--latency-ms',str(latency),'--repeat-runs','3','--reference',str(root/'reports/gdal-random-m143/full-detail-core-timed.gdal.png'),'--output',str(path)]
   child=subprocess.run(command,capture_output=True,text=True)
   if child.returncode:raise RuntimeError(child.stdout+child.stderr)
   results[(case,version,run)]=json.loads(path.read_text());print(case,version,run,'passed',flush=True)
summary=[]
for case in ['tile-rtt10','ws-rtt10','tile-local','tile-rtt50']:
 if case not in a.cases:continue
 metrics=['coldMs','repeatMs','sourceBytes','sourceRequests']+(['firstNonBlackMs'] if case.startswith('ws') else ['sourceMs','decodeMs','coldIncludingOpenMs','repeatServerMs'])
 for metric in metrics:
  medians={}
  for version in ['old','new']:
   values=[]
   for (c,v,n),d in results.items():
    if c!=case or v!=version:continue
    if case.startswith('ws'):
     cold=d['measurements'][0];repeats=d['measurements'][1:]
     values.extend([r['previewMs'] for r in repeats] if metric=='repeatMs' else [cold[{'coldMs':'previewMs','sourceBytes':'sourceReadBytes','sourceRequests':'rangeRequests','firstNonBlackMs':'firstNonBlackMs'}[metric]]])
    else:
     if metric=='repeatMs':values.extend(r['ms'] for r in d['riverRepeat'])
     elif metric=='repeatServerMs':values.extend(r['serverMs'] for r in d['riverRepeat'])
     elif metric=='coldMs':values.append(d['riverTile']['ms'])
     elif metric=='coldIncludingOpenMs':values.append(d['riverTile']['ms']+d['riverOpen']['ms'])
     elif metric=='sourceBytes':values.append(d['riverTile']['sourceBytes'])
     elif metric=='sourceRequests':values.append(d['riverTile']['rangeRequests'])
     else:values.append(float(next(v for k,v in d['riverHeaders'].items() if k.lower()==('x-river-source-cache-ms' if metric=='sourceMs' else 'x-river-decode-ms'))))
   medians[version]=statistics.median(values)
  summary.append(dict(case=case,metric=metric,**medians,improvementPercent=100*(1-medians['new']/medians['old'])))
report=dict(baseline=a.baseline,candidate=a.checkpoint,sourceBytes=978812130,sourceImagesUnchanged=True,scope='Fresh source identities and technical/encoded/range caches, warm OS filesystem cache, added per-GET latency. OpenJPEG, PNG, same Docker host, serial alternating release-image trials. HTTP measured through full body receipt; WS through full preview coverage; independent original-source pixels. No browser paint or organizational S3 claim.',measurements=summary)
(out/'paired-summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
