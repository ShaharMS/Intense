"""Cold source opening and pixel delivery, including HEAD as a real RTT."""
import argparse,json,pathlib,statistics,subprocess,sys
p=argparse.ArgumentParser();p.add_argument('--old-api',required=True);p.add_argument('--new-api',required=True);a=p.parse_args();root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/header-get-m120';out.mkdir(exist_ok=True);rows=[]
for run in range(3):
 for case in ['tiny','small','large']:
  for version in (['old','new'] if run%2==0 else ['new','old']):
   path=out/f'{case}-{version}-{run}.json';cmd=[sys.executable,str(root/'tools/benchmark-s3-corpus.py'),'--api',getattr(a,version+'_api'),'--driver','jp2grok','--objects',case,'--progressive-patches','--repeat-runs','1','--range-latency-ms','20','--head-latency-ms','20','--verify-preview-pixels','--origin-http11','--record-range-events','--output',str(path)]
   if case=='tiny':cmd+=['--preview-reference',str(root/'reports/index-checkpoint-cost-m97/tiny-grok-reference.ppm'),'--reference-reduction','0']
   if case=='large':cmd+=['--preview-reference',str(root/'reports/cold-range-probes-m96/grok-reference.ppm')]
   completed=subprocess.run(cmd,capture_output=True,text=True)
   if completed.returncode:raise RuntimeError(completed.stdout+completed.stderr)
   for sample in json.loads(path.read_text())['measurements']:
    row=dict(case=case,version=version,trial=run,**{key:sample[key] for key in ['state','openMs','firstNonBlackMs','previewMs','sourceReadBytes','rangeRequests','sourceHeadRequests','previewPixels']});row['openToFirstMs']=row['openMs']+row['firstNonBlackMs'];row['openToCoverageMs']=row['openMs']+row['previewMs'];rows.append(row)
   (out/'results.json').write_text(json.dumps(rows,indent=2));print(case,version,run,'exact pixels',flush=True)
summary=[]
for case in ['tiny','small','large']:
 for state in ['cold','repeat']:
  summary.append(dict(case=case,state=state,**{version:{key:statistics.median(row[key] for row in rows if row['case']==case and row['state']==state and row['version']==version) for key in ['openMs','firstNonBlackMs','previewMs','openToFirstMs','openToCoverageMs','sourceReadBytes','rangeRequests','sourceHeadRequests']} for version in ['old','new']}))
report={'baseline':'checkpoint 118','candidate':'checkpoint 120','scope':'Three alternating trials, new S3 identities, same 64 MiB shared cache, rendered caches disabled. Each HEAD and range GET has 20 ms fixture delay; HTTP/1.1, OS cache warm. Header opening and tile-request times are reported separately and summed, excluding browser paint and client startup. Original source images unchanged.','measurements':summary,'pixels':'every preview independently verified'};(out/'summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
