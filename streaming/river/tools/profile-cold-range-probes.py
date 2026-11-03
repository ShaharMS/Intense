"""Measure existing cold-prefix tuning against unchanged source and fresh S3 identities."""
import json,pathlib,subprocess,sys,time,urllib.request,uuid
root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/cold-range-probes-m96';out.mkdir(exist_ok=True);uid=uuid.uuid4().hex[:8]
for probe in [2048,64,4096]:
 name=f'river-goal-probes-{uid}-{probe}';api='http://127.0.0.1:18589'
 subprocess.run(['docker','run','-d','--name',name,'-p','127.0.0.1:18589:8080','-e','RIVER_ENABLE_NVIDIA=false','-e','RIVER_OTEL_ENABLED=false','-e','RIVER_ADMIN_TOKEN=fixture-admin','-e','RIVER_PROFILE_SOURCE=true','-e',f'RIVER_S3_COLD_PREFIX_PROBE_BYTES={probe}','river:2026.10.15-gpu-waves-combo-m95'],check=True,capture_output=True)
 try:
  deadline=time.monotonic()+30
  while True:
   try:json.load(urllib.request.urlopen(api+'/api/drivers',timeout=1));break
   except OSError:
    if time.monotonic()>deadline:raise
    time.sleep(.05)
  command=[sys.executable,str(root/'tools/benchmark-s3-corpus.py'),'--api',api,'--driver','jp2grok','--objects','large','--progressive-patches','--repeat-runs','2','--range-latency-ms','10','--verify-preview-pixels','--preview-reference',str(out/'grok-reference.ppm'),'--record-range-events','--output',str(out/f'probe-{probe}.json')]
  subprocess.run(command,check=True)
  rows=json.loads((out/f'probe-{probe}.json').read_text())['measurements'];print(json.dumps({'probeBytes':probe,'measurements':[{k:v for k,v in r.items() if k in ['state','firstMs','firstNonBlackMs','previewMs','sourceReadBytes','rangeRequests','error']} for r in rows]}),flush=True)
 finally:
  (out/f'probe-{probe}.log').write_bytes(subprocess.check_output(['docker','logs',name],stderr=subprocess.STDOUT));subprocess.run(['docker','stop',name],check=True,capture_output=True)
