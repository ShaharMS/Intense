import concurrent.futures,hashlib,json,os,struct,subprocess,time,urllib.request
from pathlib import Path

asset='034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299'
source=next(Path('/var/lib/river/s3-cache/local-sources').glob('river-source-*-'+asset+'.jp2'))
samples=[];references={}
for workers in [1,2,4,8]:
 env={**os.environ,'PORT':'8082','RIVER_NVJPEG2000_WORKERS':str(workers)}
 process=subprocess.Popen(['python3','/app/server.py'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
 try:
  for _ in range(120):
   try:urllib.request.urlopen('http://localhost:8082/health',timeout=1).read();break
   except Exception:time.sleep(.1)
  else:raise RuntimeError('worker experiment did not become healthy')
  def request(i,reduction,concurrency,warmup=False):
   edge=512*(1<<reduction);cols=(40980+edge-1)//edge;rows=(10980+edge-1)//edge
   body=json.dumps(dict(sourcePath=str(source),sourceWidth=40980,sourceHeight=10980,sourceTile=edge,reduction=reduction,tiles=[[i*7%cols,i%rows]])).encode()
   start=time.perf_counter()
   data=urllib.request.urlopen(urllib.request.Request('http://localhost:8082/decode',body,{'Content-Type':'application/json'}),timeout=120).read()
   elapsed=(time.perf_counter()-start)*1000;length=struct.unpack_from('<I',data)[0];meta=json.loads(data[4:4+length]);pixels=data[4+length:]
   key=(i,reduction);digest=hashlib.sha256(pixels).hexdigest()
   if workers==1:references[key]=digest
   return dict(workers=workers,concurrency=concurrency,reduction=reduction,index=i,warmup=warmup,e2eMs=elapsed,queueMs=meta['queueMs'],decodeMs=meta['decodeMs'],**meta['timing'],equalPixels=references[key]==digest)
  for reduction in [0,6]:
   for i in range(workers):samples.append(request(i,reduction,1,True))
   for concurrency in [8,32]:
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
     samples.extend(pool.map(lambda i:request(i,reduction,concurrency),range(32)))
 finally:
  process.terminate();process.wait(timeout=20)
print(json.dumps(dict(samples=samples),indent=2))
if not all(s['equalPixels'] for s in samples):raise SystemExit(1)
