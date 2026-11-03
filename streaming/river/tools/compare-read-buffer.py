"""Compare reader-local buffering with matched effective shared-cache budgets."""
import argparse,json,pathlib,statistics,subprocess,sys
p=argparse.ArgumentParser()
for flag in ['old-off-api','new-off-api','old-normal-api','new-normal-api']:p.add_argument('--'+flag,required=True)
p.add_argument('--report',default='read-buffer-m115');p.add_argument('--candidate-label',default='checkpoint 115');a=p.parse_args();root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports'/a.report;out.mkdir(exist_ok=True);rows=json.loads((out/'results.json').read_text()) if (out/'results.json').exists() else []
for mode,runs in [('off',3),('normal',3)]:
 for run in range(runs):
  for case in ['tiny','small','large']:
   if mode=='normal' and run>0 and case!='large':continue
   for version in (['old','new'] if run%2==0 else ['new','old']):
    if {r['state'] for r in rows if r['mode']==mode and r['case']==case and r['trial']==run and r['version']==version}=={'cold','repeat'}:continue
    path=out/f'{mode}-{case}-{version}-{run}.json';api=getattr(a,version+'_'+mode+'_api');command=[sys.executable,str(root/'tools/benchmark-s3-corpus.py'),'--api',api,'--driver','jp2grok','--objects',case,'--progressive-patches','--repeat-runs','1','--range-latency-ms','10','--verify-preview-pixels','--origin-http11','--output',str(path)]
    if case=='tiny':command+=['--preview-reference',str(root/'reports/index-checkpoint-cost-m97/tiny-grok-reference.ppm'),'--reference-reduction','0']
    if case=='large':command+=['--preview-reference',str(root/'reports/cold-range-probes-m96/grok-reference.ppm')]
    result=subprocess.run(command,capture_output=True,text=True)
    if result.returncode:raise RuntimeError(result.stdout+result.stderr)
    for item in json.loads(path.read_text())['measurements']:
     row=dict(mode=mode,case=case,trial=run,version=version,**{key:item[key] for key in ['state','openMs','firstNonBlackMs','previewMs','sourceReadBytes','rangeRequests','previewPixels']});row['openToFirstMs']=row['openMs']+row['firstNonBlackMs'];row['openToCoverageMs']=row['openMs']+row['previewMs'];rows.append(row)
    (out/'results.json').write_text(json.dumps(rows,indent=2));print(mode,case,version,run,'exact pixels',flush=True)
summary=[]
for mode in ['off','normal']:
 for case in ['tiny','small','large']:
  for state in ['cold','repeat']:
   summary.append(dict(mode=mode,case=case,state=state,**{version:{key:statistics.median(r[key] for r in rows if r['mode']==mode and r['case']==case and r['state']==state and r['version']==version) for key in ['openMs','firstNonBlackMs','previewMs','openToFirstMs','openToCoverageMs','sourceReadBytes','rangeRequests']} for version in ['old','new']}))
report={'baseline':'checkpoint 110','candidate':a.candidate_label,'scope':'Off: three alternating trials with a one-byte shared block cache on both versions (neither can retain a block). Normal: three paired 64 MiB large-image controls and one pair for the smaller fixtures. New S3 identities, both rendered caches disabled, 10 ms HTTP/1.1 range delay, warm OS cache. Open-to-first/coverage sums header-open and tile-request timings, excluding browser paint and client startup.','measurements':summary,'memory':'Reader locality adds at most 16 KiB per preparation; existing 8 MiB required-packet lease budget is unchanged.','pixels':'every preview pixel independently verified'};(out/'summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
