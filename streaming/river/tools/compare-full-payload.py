"""Alternate fresh-S3-identity requests; retain every result and pixel check."""
import argparse,json,pathlib,statistics,subprocess,sys
p=argparse.ArgumentParser();p.add_argument('--old-api',required=True);p.add_argument('--new-api',required=True);p.add_argument('--runs',type=int,default=3);a=p.parse_args();root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/full-payload-m98';rows=[]
cases=[('commented',['--objects','tiny','--tiny-source',str(out/'commented-full.jp2')]),('tiny',['--objects','tiny','--preview-reference',str(root/'reports/index-checkpoint-cost-m97/tiny-grok-reference.ppm'),'--reference-reduction','0']),('small',['--objects','small']),('large',['--objects','large','--progressive-patches','--preview-reference',str(root/'reports/cold-range-probes-m96/grok-reference.ppm')])]
for run in range(a.runs):
 for case,flags in cases:
  for version in (['old','new'] if run%2==0 else ['new','old']):
   path=out/f'{case}-{version}-{run}.json';command=[sys.executable,str(root/'tools/benchmark-s3-corpus.py'),'--api',getattr(a,version+'_api'),'--driver','jp2grok','--repeat-runs','1','--range-latency-ms','10','--verify-preview-pixels','--record-range-events','--origin-http11','--output',str(path),*flags]
   result=subprocess.run(command,capture_output=True,text=True)
   if result.returncode:raise RuntimeError(result.stdout+result.stderr)
   for row in json.loads(path.read_text())['measurements']:
    rows.append(dict(case=case,version=version,trial=run,**{k:row[k] for k in ['state','firstNonBlackMs','previewMs','sourceReadBytes','rangeRequests','previewPixels']}))
   (out/'results.json').write_text(json.dumps(rows,indent=2));print(case,version,run,'exact pixels',flush=True)
summary=[]
for case,_ in cases:
 for state in ['cold','repeat']:
  summary.append(dict(case=case,state=state,**{version:{key:statistics.median(r[key] for r in rows if r['case']==case and r['state']==state and r['version']==version) for key in ['firstNonBlackMs','previewMs','sourceReadBytes','rangeRequests']} for version in ['old','new']}))
report={'scope':'Three alternating trials on warm server processes, fresh S3 identities and all technical/encoded cache keys; HTTP/1.1, 10 ms range latency, OS cache warm; protocol excludes manifest open and browser paint. Original source images unchanged. Commented full-resolution image is an additional synthetic lossless fixture.','baseline':'checkpoint 95','candidate':'full-payload trial 98','measurements':summary,'pixels':'Every preview exact against independent full-source OpenJPEG or JP2Grok references.'};(out/'summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
