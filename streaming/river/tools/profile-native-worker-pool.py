import concurrent.futures,json,os,subprocess,time,urllib.parse,urllib.request
asset='034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299'
api='http://river:8082';samples=[]
for workers in [8,16,24,32]:
 env={**os.environ,'RIVER_BIND':'0.0.0.0:8082','RIVER_NVJPEG2000_NATIVE_WORKERS':str(workers),'OTEL_SERVICE_NAME':'river-worker-study'}
 process=subprocess.Popen(['/app/target/release/river-server'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 try:
  for _ in range(200):
   try:urllib.request.urlopen('http://localhost:8082/api/drivers',timeout=1).read();break
   except OSError:time.sleep(.1)
  else:raise RuntimeError('native worker study not ready')
  def request(i,concurrency,warmup=False):
   q=dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/webp',DRIVER='nvjpeg2000',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=6,TILEROW=i%22,TILECOL=i*7%81)
   with urllib.request.urlopen('http://localhost:8082/wmts?'+urllib.parse.urlencode(q),timeout=120) as response:
    body=response.read();return dict(workers=workers,concurrency=concurrency,warmup=warmup,index=i,status=response.status,serverMs=float(response.headers['x-river-server-ms']),decodeMs=float(response.headers['x-river-decode-ms']),encodeMs=float(response.headers['x-river-encode-ms']),route=response.headers['x-river-decoder-route'],bytes=len(body))
  for i in range(32):samples.append(request(i,1,True))
  for c in [8,32]:
   with concurrent.futures.ThreadPoolExecutor(max_workers=c) as pool:samples.extend(pool.map(lambda i:request(i,c),range(32)))
 finally:process.terminate();process.wait(timeout=15)
print(json.dumps(dict(samples=samples),indent=2))
