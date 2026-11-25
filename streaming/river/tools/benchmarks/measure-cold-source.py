"""Run in the sidecar; all timings include complete HTTP body receipt."""
import concurrent.futures,json,os,time,urllib.parse,urllib.request
api=os.environ.get('RIVER_COLD_API','http://river:8082')
driver=os.environ.get('RIVER_COLD_DRIVER','jp2grok')
asset='034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299'
other='135b2692882ccd32fc71db61259b1be7f494648adeea6856e9f0a4c9cf7311a4'
samples=[]
def request(label,selected=asset,matrix=0,col=0):
 q=dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=selected,STYLE='default',FORMAT='image/webp',DRIVER=driver,TILEMATRIXSET='RiverPixel-'+selected,TILEMATRIX=matrix,TILEROW=0,TILECOL=col)
 start=time.perf_counter();s=dict(label=label,asset=selected,matrix=matrix,col=col)
 try:
  with urllib.request.urlopen(api+'/wmts?'+urllib.parse.urlencode(q),timeout=120) as r:
   b=r.read();s.update(status=r.status,e2eMs=(time.perf_counter()-start)*1000,bytes=len(b),traceId=r.headers.get('x-river-trace-id'),cache=r.headers.get('x-river-source-cache'))
   for name in ['queue','decode','encode','server','source-cache']:s[name+'Ms']=float(r.headers.get('x-river-'+name+'-ms','0'))
 except Exception as e:s.update(error=str(e),e2eMs=(time.perf_counter()-start)*1000)
 samples.append(s);return s
request('cold-overview')
with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:list(pool.map(lambda i:request('early-burst',matrix=6,col=i*7%81),range(8)))
# Wait for a deferred verified copy, without turning its completion into part
# of the foreground request latency. Older versions return no cache header.
wait_start=time.perf_counter()
if samples[0].get('cache')=='warming':
 while time.perf_counter()-wait_start<60:
  s=request('cache-ready-probe',matrix=6)
  if s.get('cache')=='ready':break
  time.sleep(.5)
with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
 cold=pool.submit(request,'other-cold-overview',other)
 time.sleep(.05)
 ready=list(pool.map(lambda i:request('ready-during-other-fill',matrix=6,col=i*7%81),range(7)))
 cold.result()
print(json.dumps(dict(at=time.time(),api=api,driver=driver,backgroundWaitSeconds=time.perf_counter()-wait_start,samples=samples),indent=2))
if any('error' in s for s in samples):raise SystemExit(1)
