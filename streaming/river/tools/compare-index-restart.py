"""Cold source, persisted-index restart and hot repeat across both protocols."""
import argparse,json,pathlib,statistics,subprocess,sys
p=argparse.ArgumentParser();p.add_argument('--old-api',required=True);p.add_argument('--old-container',required=True);p.add_argument('--new-api',required=True);p.add_argument('--new-container',required=True);p.add_argument('--runs',type=int,default=3);p.add_argument('--report',default='index-prefix-m103');p.add_argument('--candidate-label',default='checkpoint 103');a=p.parse_args();root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports'/a.report;out.mkdir(exist_ok=True);rows=json.loads((out/'results.json').read_text()) if (out/'results.json').exists() else []
for run in range(a.runs):
 for protocol,flags in [('websocket',['--progressive-patches']),('wmts',['--wmts-preview'])]:
  for version in (['old','new'] if run%2==0 else ['new','old']):
   if {r['state'] for r in rows if r['protocol']==protocol and r['version']==version and r['trial']==run}=={'cold','restart','repeat'}:continue
   path=out/f'{protocol}-{version}-{run}.json';command=[sys.executable,str(root/'tools/benchmark-s3-corpus.py'),'--api',getattr(a,version+'_api'),'--restart-container',getattr(a,version+'_container'),'--driver','jp2grok','--objects','large','--repeat-runs','1','--range-latency-ms','10','--verify-preview-pixels','--preview-reference',str(root/'reports/cold-range-probes-m96/grok-reference.ppm'),'--origin-http11','--record-range-events','--output',str(path),*flags]
   result=subprocess.run(command,capture_output=True,text=True)
   if result.returncode:raise RuntimeError(result.stdout+result.stderr)
   for row in json.loads(path.read_text())['measurements']:
    rows.append(dict(protocol=protocol,version=version,trial=run,**{k:row[k] for k in ['state','firstMs','firstNonBlackMs','previewMs','openMs','sourceReadBytes','rangeRequests','previewPixels']}))
   (out/'results.json').write_text(json.dumps(rows,indent=2));print(protocol,version,run,'exact pixels',flush=True)
summary=[]
for protocol in ['websocket','wmts']:
 for state in ['cold','restart','repeat']:
  summary.append(dict(protocol=protocol,state=state,**{version:{key:statistics.median(r[key] for r in rows if r['protocol']==protocol and r['state']==state and r['version']==version) for key in ['firstMs','firstNonBlackMs','previewMs','openMs','sourceReadBytes','rangeRequests']} for version in ['old','new']}))
report={'scope':'Three alternating trials; unchanged 978812130-byte original image; fresh S3 identities per trial, then restart preserving only technical metadata; encoded disk cache disabled, compressed cache cleared by restart; HTTP/1.1 and 10 ms range delay; OS cache warm. Timings exclude manifest open and browser paint.','baseline':'checkpoint 100','candidate':a.candidate_label,'measurements':summary,'pixels':'exact independent full-original JP2Grok reference','distinction':'restart is process-cold with an existing technical index; cold is first source access without an index.'};(out/'summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
