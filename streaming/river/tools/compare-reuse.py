"""Checkpoint 69 comparison: alternate fresh identities, preserve image data."""
import argparse,json,statistics,subprocess,sys
from pathlib import Path
from urllib.request import urlopen
p=argparse.ArgumentParser();p.add_argument('--old-api',required=True);p.add_argument('--new-api',required=True);p.add_argument('--runs',type=int,default=3);p.add_argument('--report',required=True);a=p.parse_args()
root=Path(__file__).resolve().parents[1];out=root/'reports'/a.report;out.mkdir(exist_ok=True)
for api in [a.old_api,a.new_api]:assert json.load(urlopen(api+'/api/settings/encoding'))['webpMethod']==0,'use matching encoding settings'
cases={
 'small-complete':['--objects','tiny','small','--complete'],
 'large-preview':['--objects','large','--verify-preview-pixels','--preview-reference',str(root/'reports/grok-large-reference-m54.ppm')],
 'large-preview-rtt10':['--objects','large','--verify-preview-pixels','--preview-reference',str(root/'reports/grok-large-reference-m54.ppm'),'--range-latency-ms','10'],
 'large-complete':['--objects','large','--complete'],
 'large-complete-rtt10':['--objects','large','--complete','--range-latency-ms','10'],
 'large-capacity8':['--objects','large','--viewers','8','--distinct','--parallel-open'],
 'large-capacity8-rtt10':['--objects','large','--viewers','8','--distinct','--parallel-open','--range-latency-ms','10'],
}
records={}
for case,flags in cases.items():
    for run in range(a.runs):
        for version in (['old','new'] if run%2==0 else ['new','old']):
            path=out/f'{case}-{version}-{run}.json'
            cmd=[sys.executable,str(root/'tools/benchmark-s3-corpus.py'),'--api',getattr(a,version+'_api'),'--driver','jp2grok','--progressive-patches','--repeat-runs','2','--output',str(path),*flags]
            result=subprocess.run(cmd,cwd=root,capture_output=True,text=True)
            if result.returncode:raise RuntimeError(result.stdout+result.stderr)
            rows=json.loads(path.read_text())['measurements'];assert all('error' not in row for row in rows),rows
            records[(case,version,run)]=rows;print(case,version,run,'passed',flush=True)
summary=[];traffic=[]
for case in cases:
    objects=sorted({row['object'] for (c,v,r),rows in records.items() if c==case for row in rows})
    for obj in objects:
        for state in ['cold','repeat']:
            for metric in ['firstNonBlackMs' if 'preview' in case else 'firstMs','previewMs']+(['finalMs'] if 'complete' in case else []):
                values={}
                for version in ['old','new']:
                    vals=[row[metric] for (c,v,r),rows in records.items() if c==case and v==version for row in rows if row['object']==obj and row['state']==state]
                    values[version]=round(statistics.median(value['p95'] if isinstance(value,dict) else value for value in vals),3)
                summary.append(dict(case=case,object=obj,state=state,metric=metric,**values,improvementPercent=round((1-values['new']/values['old'])*100,1)))
        data={version:{metric:statistics.median(row[metric] for (c,v,r),rows in records.items() if c==case and v==version for row in rows if row['object']==obj and row['state']=='cold') for metric in ['sourceReadBytes','rangeRequests']} for version in ['old','new']}
        assert data['new']==data['old'],(case,'source traffic changed',data)
        traffic.append(dict(case=case,object=obj,**data))
report=dict(scope='Three alternating trials per case on one Docker host against checkpoint 69. Same CPU JP2Grok/native path and WebP method 0. Fresh S3 identities and index/encoded/compressed caches; OS cache warm. PNG preview pixels independently checked. Protocol timings exclude manifest open and browser paint. Capacity metrics are per-run p95, then median across runs. Source images unchanged.',runs=a.runs,traffic=traffic,measurements=summary)
(out/'summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
