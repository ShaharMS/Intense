"""Alternating cross-source fairness and same-source uncached-ROI checks."""
import argparse,json,pathlib,statistics,subprocess,sys
p=argparse.ArgumentParser();p.add_argument('--old-api',required=True);p.add_argument('--new-api',required=True);a=p.parse_args();root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/source-fairness-m106';rows=[]
for protocol,runs in [('wmts',3),('websocket',1)]:
 for run in range(runs):
  for version in (['old','new'] if run%2==0 else ['new','old']):
   label=f'{version}-{protocol}-{run}';cmd=[sys.executable,str(root/'tools/benchmark-source-fairness.py'),'--api',getattr(a,version+'_api'),'--label',label,'--protocol',protocol]
   result=subprocess.run(cmd,capture_output=True,text=True)
   if result.returncode:raise RuntimeError(result.stdout+result.stderr)
   row=json.loads((out/(label+'.json')).read_text())['measurements'][0];row.update(version=version,trial=run);rows.append(row);(out/'paired-results.json').write_text(json.dumps(rows,indent=2));print(json.dumps(row),flush=True)
summary=[]
for protocol in ['wmts','websocket']:
 summary.append(dict(protocol=protocol,**{version:{'fastMs':statistics.median(r['fastMs'] for r in rows if r['protocol']==protocol and r['version']==version),'slowestHeavyMs':statistics.median(max(r['heavyMs']) for r in rows if r['protocol']==protocol and r['version']==version),'slowestWarmUncachedMs':statistics.median(max(r['warmUncachedMs']) for r in rows if r['protocol']==protocol and r['version']==version),'sourceBytes':statistics.median(r['sourceBytes'] for r in rows if r['protocol']==protocol and r['version']==version),'requests':statistics.median(r['rangeRequests'] for r in rows if r['protocol']==protocol and r['version']==version)} for version in ['old','new']}))
report={'baseline':'checkpoint 104','candidate':'checkpoint 108','scope':'WMTS: three alternating trials; WebSocket: one paired protocol regression. Two global source IO slots, unrestricted CPU, synthetic 10 ms range delay, new S3 identities. Source header opens excluded. Additional lossless two-layer fixture is padded to 64 MiB plus 4 KiB with auxiliary free-box data to exercise the large-object admission path; original corpus unchanged.','measurements':summary,'pixels':'exact for small response, both competing heavy ROIs and subsequent uncached adjacent ROIs'};(out/'summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
