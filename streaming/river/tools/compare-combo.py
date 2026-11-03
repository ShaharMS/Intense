"""Alternating old/new fresh-source trials; compare medians on one Docker host."""
import argparse,json,statistics,subprocess,sys
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--old-api',required=True);p.add_argument('--new-api',required=True);p.add_argument('--runs',type=int,default=3);a=p.parse_args()
root=Path(__file__).resolve().parents[1];out=root/'reports/combo-comparison-m63';out.mkdir(exist_ok=True)
cases={
 'tiny-small-complete':['--objects','tiny','small','--complete'],
 'large-preview':['--objects','large','--verify-preview-pixels','--preview-reference',str(root/'reports/grok-large-reference-m54.ppm')],
 'capacity16':['--objects','small','--viewers','16','--distinct','--parallel-open'],
 'large-preview-rtt10':['--objects','large','--range-latency-ms','10'],
}
records={}
for case,flags in cases.items():
    for run in range(a.runs):
        for version in (['old','new'] if run%2==0 else ['new','old']):
            path=out/f'{case}-{version}-{run}.json'
            result=subprocess.run([sys.executable,str(root/'tools/benchmark-s3-corpus.py'),'--api',getattr(a,version+'_api'),
                '--driver','jp2grok','--repeat-runs','2','--output',str(path),*flags],cwd=root,capture_output=True,text=True)
            if result.returncode:raise RuntimeError(result.stdout+result.stderr)
            data=json.loads(path.read_text())['measurements']
            assert all('error' not in row for row in data),data
            records[(case,version,run)]=data
            print(case,version,run,'passed',flush=True)
summary=[]
for case in cases:
    objects=sorted({row['object'] for (c,v,r),rows in records.items() if c==case for row in rows})
    for obj in objects:
        for state in ['cold','repeat']:
            for metric in (['previewMs','finalMs'] if case=='tiny-small-complete' else ['previewMs']):
                values={}
                for version in ['old','new']:
                    rows=[row for (c,v,r),rows in records.items() if c==case and v==version for row in rows if row['object']==obj and row['state']==state]
                    vals=[row[metric]['p95'] if isinstance(row[metric],dict) else row[metric] for row in rows]
                    values[version]=round(statistics.median(vals),3)
                summary.append(dict(case=case,object=obj,state=state,metric=metric,**values,improvementPercent=round((1-values['new']/values['old'])*100,1)))
report=dict(scope='Alternating trials on one Docker host, fresh S3 identity and compressed cache per cold run; OS cache warm. Protocol timing excludes manifest open and browser paint. Capacity uses per-run p95, then median across runs.',runs=a.runs,measurements=summary)
(out/'summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
