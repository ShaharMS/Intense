"""Paired fresh-instance working-set retention and process-cold revisit trials."""
import json,pathlib,statistics,subprocess,sys,time,urllib.request,uuid
root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/index-working-set-m109';uid=uuid.uuid4().hex[:8];rows=[]
for run in range(3):
 for version in (['old','new'] if run%2==0 else ['new','old']):
  name=f'river-goal-index-set-{uid}-{run}-{version}';api='http://127.0.0.1:18610';image='river:2026.10.15-source-fairness-combo-m108' if version=='old' else 'river:2026.10.16-index-capacity-combo-m110';label=f'{version}-{run}'
  cmd=['docker','run','-d','--name',name,'-p','127.0.0.1:18610:8080','-e','RIVER_ENABLE_NVIDIA=false','-e','RIVER_OTEL_ENABLED=false','-e','RIVER_ADMIN_TOKEN=fixture-admin','-e','RIVER_RASTER_CACHE_BYTES=0','-e','RIVER_RASTER_DISK_CACHE_BYTES=0','-e','RIVER_S3_INDEX_CACHE_ROOT=/var/lib/river/s3-cache/technical-index','-e','RIVER_S3_INDEX_CACHE_BYTES=33554432','-e','RIVER_S3_INDEX_MEMORY_BYTES=33554432',image]
  subprocess.run(cmd,check=True,capture_output=True)
  try:
   deadline=time.monotonic()+30
   while True:
    try:
     with urllib.request.urlopen(api+'/health',timeout=1) as r:assert r.status==200
     break
    except OSError:
     if time.monotonic()>deadline:raise
     time.sleep(.05)
   result=subprocess.run([sys.executable,str(root/'tools/benchmark-index-working-set.py'),'--api',api,'--container',name,'--label',label],capture_output=True,text=True)
   if result.returncode:raise RuntimeError(result.stdout+result.stderr)
   report=json.loads((out/(label+'.json')).read_text());assert report['afterPressure']['largeRetained']==(version=='new');assert report['afterPressure']['bytes']<=report['indexBudgetBytes'];row=dict(report,trial=run,version=version);rows.append(row);(out/'paired-results.json').write_text(json.dumps(rows,indent=2));print(json.dumps({'trial':run,'version':version,'pressureMs':row['pressureMs'],'indexes':row['afterPressure']['files'],'bytes':row['afterPressure']['bytes'],'revisit':row['revisit']}),flush=True)
  finally:subprocess.run(['docker','stop',name],check=True,capture_output=True)
summary={version:{'coldCoverageMs':statistics.median(r['cold']['coverageMs'] for r in rows if r['version']==version),'pressureMs':statistics.median(r['pressureMs'] for r in rows if r['version']==version),'indexCount':statistics.median(r['afterPressure']['files'] for r in rows if r['version']==version),'indexBytes':statistics.median(r['afterPressure']['bytes'] for r in rows if r['version']==version),'revisit':{k:statistics.median(r['revisit'][k] for r in rows if r['version']==version) for k in ['firstMs','coverageMs','sourceBytes','rangeRequests']}} for version in ['old','new']}
report={'baseline':'checkpoint 108','candidate':'checkpoint 110','scope':'Three alternating fresh-container trials; view the unchanged 979 MB source, then actually render 512 distinct aliases of one tiny lossless fixture via automatic S3 URLs, then restart and revisit the large source. Both encoded caches disabled. Same 32 MiB disk and memory index budgets; HTTP/1.1 and 10 ms per range; OS cache warm. Timings exclude header opens and browser paint.','measurements':summary,'pixels':'every rendered pixel verified against independent references'};(out/'summary.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
