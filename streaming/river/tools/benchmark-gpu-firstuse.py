"""Isolate the first coarse GPU RPC in fresh, stopped-after-use containers."""
import argparse,json,pathlib,subprocess,time,urllib.request,uuid
p=argparse.ArgumentParser();p.add_argument('--runs',type=int,default=3)
p.add_argument('--experiment',choices=['coarse-prime','buffered-read'],default='coarse-prime');a=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[1];folder=root/'reports/gpu-probe-diagnostics-m85'
image='river:2026.10.15-cached-probe-combo-m85';rows=[]
run_id=uuid.uuid4().hex[:8]
rpc="""import json,struct,time,urllib.request,hashlib
deadline=time.monotonic()+30
while True:
 h=json.load(urllib.request.urlopen('http://127.0.0.1:8081/health'))
 if h['warmup']['status']=='ready':break
 if h['warmup']['status']=='failed' or time.monotonic()>deadline:raise RuntimeError(h)
 time.sleep(.02)
request=dict(sourcePath='/opt/river/corpus/first-test.j2k',sourceWidth=8192,sourceHeight=1024,sourceTile=1024,reduction=6,tiles=[[x,0] for x in range(8)],rpcCodec='lz4')
started=time.perf_counter()
with urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:8081/decode',data=json.dumps(request).encode(),headers={'Content-Type':'application/json'})) as r:body=r.read()
length=struct.unpack_from('<I',body)[0];metadata=json.loads(body[4:4+length])
print(json.dumps(dict(rpcMs=(time.perf_counter()-started)*1000,metadata=metadata,payloadSha256=hashlib.sha256(body[4+length:]).hexdigest(),warmup=h['warmup'])))
"""
for run in range(a.runs):
    configurations=['control',a.experiment] if run%2==0 else [a.experiment,'control']
    for label in configurations:
        name=f'river-goal-gpu-study-{run_id}-{label}-{run}';port=18574
        script=folder/('server-warm-coarse.py' if label=='coarse-prime' else 'server.py')
        command=['docker','run','-d','--gpus','all','--name',name,'-p',f'127.0.0.1:{port}:8080',
            '-e','RIVER_ENABLE_NVIDIA=true','-e','RIVER_ADMIN_TOKEN=fixture-admin','-e','RIVER_OTEL_ENABLED=false',
            '--mount',f'type=bind,source={script},target=/opt/river/nvidia/server.py,readonly',
            '--mount',f'type=bind,source={folder/"new-first.j2k"},target=/opt/river/corpus/first-test.j2k,readonly',image]
        if label=='buffered-read':
            command[-1:-1]=['--mount',f'type=bind,source={root/"nvjpeg2000-sidecar/resolution_view.py"},target=/opt/river/nvidia/resolution_view.py,readonly']
        started=time.monotonic();subprocess.run(command,check=True,capture_output=True)
        try:
            deadline=started+30
            while True:
                try:json.load(urllib.request.urlopen(f'http://127.0.0.1:{port}/api/drivers',timeout=1));break
                except OSError:
                    if time.monotonic()>deadline:raise
                    time.sleep(.05)
            ready_ms=(time.monotonic()-started)*1000
            result=json.loads(subprocess.check_output(['docker','exec',name,'python3','-c',rpc],text=True))
            row=dict(configuration=label,run=run,apiReadyMs=ready_ms,**result);rows.append(row)
            print(json.dumps(dict(configuration=label,run=run,apiReadyMs=ready_ms,rpcMs=result['rpcMs'],timing=result['metadata'].get('timing'),warmup=result['warmup'])),flush=True)
        finally:subprocess.run(['docker','stop',name],check=True,capture_output=True)
(root/f'reports/gpu-fresh-container-study-m86-{a.experiment}.json').write_text(json.dumps(rows,indent=2))
