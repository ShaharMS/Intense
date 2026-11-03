"""Alternate previous/new builds on the same unchanged large source."""
import argparse,json,statistics,subprocess,sys
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--old-api',required=True);p.add_argument('--new-api',required=True);p.add_argument('--runs',type=int,default=3);p.add_argument('--report',required=True);a=p.parse_args()
root=Path(__file__).resolve().parents[1];out=root/'reports'/a.report;out.mkdir(exist_ok=True)
records={}
cases={'preview-local':[], 'preview-rtt10':['--range-latency-ms','10'], 'complete-local':['--complete'],'complete-rtt10':['--complete','--range-latency-ms','10']}
for case,flags in cases.items():
    for run in range(a.runs):
        for version in (['old','new'] if run%2==0 else ['new','old']):
            path=out/f'{case}-{version}-{run}.json'
            command=[sys.executable,str(root/'tools/benchmark-s3-corpus.py'),'--api',getattr(a,version+'_api'),'--driver','jp2grok','--objects','large','--progressive-patches','--repeat-runs','2','--output',str(path),*flags]
            if 'preview' in case:command+=['--verify-preview-pixels','--preview-reference',str(root/'reports/grok-large-reference-m54.ppm')]
            result=subprocess.run(command,cwd=root,capture_output=True,text=True)
            if result.returncode:raise RuntimeError(result.stdout+result.stderr)
            rows=json.loads(path.read_text())['measurements'];assert all('error' not in row for row in rows)
            records[(case,version,run)]=rows;print(case,version,run,'passed',flush=True)
summary=[]
for case in cases:
    for state in ['cold','repeat']:
        for metric in ['firstNonBlackMs' if 'preview' in case else 'firstMs','previewMs']+(['finalMs'] if 'complete' in case else []):
            medians={version:round(statistics.median(row[metric] for (c,v,r),rows in records.items() if c==case and v==version for row in rows if row['state']==state),3) for version in ['old','new']}
            summary.append(dict(case=case,state=state,metric=metric,**medians,improvementPercent=round((1-medians['new']/medians['old'])*100,1)))
traffic={version:{metric:statistics.median(rows[0][metric] for (c,v,r),rows in records.items() if c=='preview-local' and v==version) for metric in ['sourceReadBytes','rangeRequests']} for version in ['old','new']}
report=dict(scope='Three alternating trials per case on one Docker host; CPU JP2Grok; fresh S3 identity and compressed/encoded/index caches; OS cache warm. PNG previews checked against independent original-source pixels. Protocol times exclude manifest open/browser paint. No source image changes.',runs=a.runs,traffic=traffic,measurements=summary)
(out/'summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
