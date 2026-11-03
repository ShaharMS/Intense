"""Isolate technical-index persistence cost; runtime stays unchanged."""
import json,pathlib,subprocess,sys,time,urllib.request,uuid
root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/index-checkpoint-cost-m97';out.mkdir(exist_ok=True);uid=uuid.uuid4().hex[:8];rows=[]
for run in range(3):
 for disk in ([True,False] if run%2==0 else [False,True]):
  label='disk' if disk else 'memory';name=f'river-goal-index-cost-{uid}-{run}-{label}';api='http://127.0.0.1:18590'
  command=['docker','run','-d','--name',name,'-p','127.0.0.1:18590:8080','-e','RIVER_ENABLE_NVIDIA=false','-e','RIVER_OTEL_ENABLED=false','-e','RIVER_ADMIN_TOKEN=fixture-admin','-e',f'RIVER_S3_INDEX_CACHE_BYTES={33554432 if disk else 0}','river:2026.10.15-gpu-waves-combo-m95']
  subprocess.run(command,check=True,capture_output=True)
  try:
   deadline=time.monotonic()+30
   while True:
    try:json.load(urllib.request.urlopen(api+'/api/drivers',timeout=1));break
    except OSError:
     if time.monotonic()>deadline:raise
     time.sleep(.05)
   for case,objects,latency in [('tiny',['tiny'],0),('small',['small'],0),('large',['large'],10)]:
    path=out/f'{run}-{label}-{case}.json'
    cmd=[sys.executable,str(root/'tools/benchmark-s3-corpus.py'),'--api',api,'--driver','jp2grok','--objects',*objects,'--progressive-patches','--repeat-runs','1','--range-latency-ms',str(latency),'--verify-preview-pixels','--origin-http11','--output',str(path)]
    if case=='tiny':cmd+=['--preview-reference',str(out/'tiny-grok-reference.ppm'),'--reference-reduction','0']
    if case=='large':cmd+=['--preview-reference',str(root/'reports/cold-range-probes-m96/grok-reference.ppm')]
    completed=subprocess.run(cmd,capture_output=True,text=True)
    if completed.returncode:raise RuntimeError(completed.stdout+completed.stderr)
    for result in json.loads(path.read_text())['measurements']:
     row=dict(run=run,checkpointing=label,**{k:result[k] for k in ['object','state','firstNonBlackMs','previewMs','sourceReadBytes','rangeRequests','previewPixels']});rows.append(row);print(json.dumps(row),flush=True)
  finally:subprocess.run(['docker','stop',name],check=True,capture_output=True)
  (out/'results.json').write_text(json.dumps(rows,indent=2))
