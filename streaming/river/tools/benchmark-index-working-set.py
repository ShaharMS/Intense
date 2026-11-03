"""Measure process-cold large-image revisit after a real 512-image working set."""
import argparse,concurrent.futures,hashlib,importlib.util,io,json,pathlib,socket,subprocess,threading,time,urllib.request,urllib.parse,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--container',required=True);p.add_argument('--label',required=True);p.add_argument('--count',type=int,default=512);a=p.parse_args();assert a.container.startswith('river-goal-');root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/index-working-set-m109';out.mkdir(exist_ok=True)
big=root/'corpus/cache/sentinel-date-mosaics/2016-09-12.jp2';tiny=out/'pressure.jp2'
if not tiny.exists():
 pixels=bytes((x*17+y*31+c*83)&255 for y in range(512) for x in range(512) for c in range(3));Image.frombytes('RGB',(512,512),pixels).save(tiny,format='JPEG2000',irreversible=False,num_resolutions=6)
small_reference=Image.open(tiny);small_reference.reduce=5;small_reference.load();small_reference=small_reference.convert('RGBA')
spec=importlib.util.spec_from_file_location('river_fixture',root/'tools/benchmark-s3-corpus.py');fixture=importlib.util.module_from_spec(spec);spec.loader.exec_module(fixture);large_reference=fixture.load_preview_reference(big,6,1,root/'reports/cold-range-probes-m96/grok-reference.ppm')
metadata={str(path):path.stat() for path in [big,tiny]};events=[]
class S3(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
 def log_message(self,*args):pass
 def object(self):
  key=urllib.parse.unquote(urllib.parse.urlparse(self.path).path).split('/')[-1];assert key=='large.jp2' or key.startswith('pressure-'),key
  path=big if key=='large.jp2' else tiny;return key,path,metadata[str(path)]
 def do_HEAD(self):
  key,path,meta=self.object();self.send_response(200);self.send_header('Content-Length',str(meta.st_size));self.send_header('ETag','"'+key+'"');self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT');self.end_headers()
 def do_GET(self):
  key,path,meta=self.object();assert self.headers['If-Match']=='"'+key+'"';assert path.stat().st_mtime_ns==meta.st_mtime_ns
  lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));assert 0<=lo<=hi<meta.st_size and hi-lo+1<=1024*1024;time.sleep(.01)
  with path.open('rb') as stream:stream.seek(lo);body=stream.read(hi-lo+1)
  events.append({'key':key,'bytes':len(body)});self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{meta.st_size}');self.send_header('ETag','"'+key+'"');self.end_headers();self.wfile.write(body)
class Server(ThreadingHTTPServer):request_queue_size=128
server=Server(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start();source='working-'+uuid.uuid4().hex[:12];config=dict(id=source,name=source,bucket='fixture',region='us-east-1',endpoint=f'http://host.docker.internal:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True)
def request(path,body=None,method=None):
 with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=45) as response:return json.load(response) if response.status!=204 else None
def tile(path,expected):
 started=time.perf_counter()
 with urllib.request.urlopen(a.api+path,timeout=45) as response:body=response.read()
 ms=(time.perf_counter()-started)*1000;image=Image.open(io.BytesIO(body)).convert('RGBA');assert image.size==expected.size and image.tobytes()==expected.tobytes(),'working-set pixels';return ms
def large_tiles(asset):
 before=len(events);started=time.perf_counter();times=[]
 for x in [0,1]:
  query=urllib.parse.urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/png',DRIVER='jp2grok',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=0,TILEROW=0,TILECOL=x))
  times.append(tile('/wmts?'+query,large_reference.crop((x*512,0,min((x+1)*512,large_reference.width),large_reference.height))))
 return {'firstMs':times[0],'coverageMs':(time.perf_counter()-started)*1000,'tileMs':times,'sourceBytes':sum(e['bytes'] for e in events[before:]),'rangeRequests':len(events)-before,'pixels':'exact'}
def snapshot(asset):
 filename=hashlib.sha256(asset.encode()).hexdigest()+'.json'
 script="from pathlib import Path;import json;p=Path('/var/lib/river/s3-cache/technical-index');f=list(p.glob('*.json'));print(json.dumps({'files':len(f),'bytes':sum(v.stat().st_size for v in f),'largeRetained':(p/"+repr(filename)+").exists()}))"
 return json.loads(subprocess.check_output(['docker','exec',a.container,'python3','-c',script],text=True))
created=False
try:
 request('/api/s3/sources',config);created=True;asset=request('/api/s3/sources/'+source+'/assets',{'key':'large.jp2'})['assetId'];cold=large_tiles(asset);before=snapshot(asset);print(json.dumps({'version':a.label,'cold':cold,'initialIndex':before}),flush=True)
 pressure_start=time.perf_counter();event_start=len(events)
 def pressure(index):
  key=f'pressure-{index:04d}.jp2';locator='s3-'+json.dumps([source,key],separators=(',',':')).encode().hex();query=urllib.parse.urlencode({'image':locator,'driver':'jp2grok'});return tile('/streamer/v1/0/0/0/tile.png?'+query,small_reference)
 with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:list(pool.map(pressure,range(a.count)))
 pressure_ms=(time.perf_counter()-pressure_start)*1000;after=snapshot(asset);print(json.dumps({'version':a.label,'pressureMs':pressure_ms,'afterPressure':after}),flush=True)
 subprocess.run(['docker','restart',a.container],check=True,capture_output=True,timeout=45);deadline=time.monotonic()+30
 while True:
  try:
   with urllib.request.urlopen(a.api+'/health',timeout=1) as response:assert response.status==200
   break
  except OSError:
   if time.monotonic()>deadline:raise
   time.sleep(.1)
 request('/api/s3/sources',config);reopened=request('/api/s3/sources/'+source+'/assets',{'key':'large.jp2'})['assetId'];assert reopened==asset
 revisit=large_tiles(asset);report={'version':a.label,'images':a.count+1,'scope':'Actual views of a large original followed by 512 tiny S3 aliases, then process restart. Encoded caches disabled. 10 ms range latency, HTTP/1.1; OS cache warm. Source headers reopened outside revisit timing. Each pixel compared to independent references.','cold':cold,'initialIndex':before,'pressureMs':pressure_ms,'afterPressure':after,'revisit':revisit,'indexBudgetBytes':33554432,'pixels':'exact'};(out/(a.label+'.json')).write_text(json.dumps(report,indent=2));print(json.dumps(report),flush=True)
finally:
 if created:request('/api/s3/sources/'+source,method='DELETE')
 server.shutdown()
