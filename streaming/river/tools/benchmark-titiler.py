"""Real TiTiler HTTP API vs River; identical immutable JP2 via recorded ranges."""
import argparse,importlib.util,io,json,pathlib,socket,threading,time,urllib.parse,urllib.request,urllib.error,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import numpy as np
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--titiler',default='http://127.0.0.1:18897');p.add_argument('--river',default='http://127.0.0.1:18894');p.add_argument('--runs',type=int,default=1);p.add_argument('--latencies',type=float,nargs='+',default=[0,10]);p.add_argument('--cases',nargs='+',default=['tile','preview']);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[1];file=root/'corpus/cache/sentinel-date-mosaics/2016-09-12.jp2';stat=file.stat();phase='idle';latency=0;events=[];results=[]
spec=importlib.util.spec_from_file_location('bench_source',root/'tools/benchmark-s3-corpus.py');module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
refs={'tile':Image.open(root/'reports/gdal-random-m143/full-detail-core-timed.gdal.png').convert('RGBA'),'preview':module.load_preview_reference(file,6,1,root/'reports/grok-large-reference-m54.ppm')}
class Origin(ThreadingHTTPServer):request_queue_size=256
class Source(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
 def log_message(self,*_):pass
 def do_HEAD(self):self.reply(False)
 def do_GET(self):self.reply(True)
 def reply(self,body):
  started=time.perf_counter();label=phase;lo,hi=0,stat.st_size-1
  if body:
   raw=self.headers.get('Range')
   if not raw:self.send_error(400,'Bounded Range required');return
   x,y=raw.removeprefix('bytes=').split('-');lo=int(x) if x else max(0,stat.st_size-int(y));hi=min(int(y),stat.st_size-1) if x and y else stat.st_size-1
   assert 0<=lo<=hi<stat.st_size and hi-lo+1<=4*1024*1024,(lo,hi)
  time.sleep(latency/1000)
  assert file.stat().st_mtime_ns==stat.st_mtime_ns
  payload=b''
  if body:
   with file.open('rb') as f:f.seek(lo);payload=f.read(hi-lo+1)
  self.send_response(206 if body else 200);self.send_header('Content-Length',str(len(payload) if body else stat.st_size));self.send_header('Content-Type','image/jp2');self.send_header('ETag','"stable"');self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT');self.send_header('Accept-Ranges','bytes')
  if body:self.send_header('Content-Range',f'bytes {lo}-{hi}/{stat.st_size}')
  self.end_headers()
  try:
   if body:self.wfile.write(payload)
  except OSError:pass
  events.append(dict(phase=label,method=self.command,start=lo,end=hi,bytes=len(payload),ms=(time.perf_counter()-started)*1000))
server=Origin(('0.0.0.0',0),Source);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(api,path,body=None,method=None):
 req=urllib.request.Request(api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'})
 with urllib.request.urlopen(req,timeout=180) as r:return r.read()
a.output.parent.mkdir(parents=True,exist_ok=True)
try:
 for latency in a.latencies:
  for case in a.cases:
   for trial in range(a.runs):
    for version in (['titiler','river'] if trial%2==0 else ['river','titiler']):
     nonce=uuid.uuid4().hex;sid='titiler-'+nonce[:12];key=nonce+'.jp2'
     if version=='river':request(a.river,'/api/s3/sources',dict(id=sid,name=sid,bucket=sid,region='us-east-1',endpoint=f'http://host.docker.internal:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True))
     remote=f'http://host.docker.internal:{server.server_port}/fixture/{key}'
     if version=='titiler':
      q=[('url',remote),('bidx',1),('bidx',2),('bidx',3),('rescale','0,65535'),('return_mask','false')]
      if case=='tile':path='/cog/bbox/681560,4128320,686680,4133440/512x512.png';q.extend([('coord_crs','EPSG:32610'),('dst_crs','EPSG:32610')])
      else:path='/cog/preview/641x172.png'
      api=a.titiler;path+='?'+urllib.parse.urlencode(q)
     else:
      q=dict(image=f's3://{sid}/{key}',edge='crop',driver='openjpeg')
      if case=='tile':path='/streamer/v1/6/55/13/tile.png'
      else:path='/streamer/v1/0/0/0/tile.png';q.update(tileSize=1024,maxZoom=6)
      api=a.river;path+='?'+urllib.parse.urlencode(q)
     try:
      for state in ['cold','repeat']:
       phase=f'{version}-{case}-{latency}-{trial}-{state}';begin=time.perf_counter();data=request(api,path);ms=(time.perf_counter()-begin)*1000
       im=Image.open(io.BytesIO(data)).convert('RGBA');ref=refs[case];assert im.size==ref.size,(im.size,ref.size)
       delta=np.abs(np.asarray(im).astype(np.int16)-np.asarray(ref).astype(np.int16));rgb=delta[:,:,:3];traffic=[e for e in events if e['phase']==phase]
       row=dict(version=version,case=case,latencyMs=latency,trial=trial,state=state,ms=ms,bytes=sum(e['bytes'] for e in traffic),gets=sum(e['method']=='GET' for e in traffic),heads=sum(e['method']=='HEAD' for e in traffic),outputSize=im.size,maxRgbError=int(rgb.max()),meanRgbError=float(rgb.mean()),exactPixels=not bool(delta.any()))
       results.append(row);print(json.dumps(row),flush=True);(a.output.parent/(phase+'.png')).write_bytes(data)
       if version=='river':assert row['exactPixels'],'River independent pixels differ'
       elif case=='tile':assert row['maxRgbError']<=1,'TiTiler tile differs beyond 8-bit rescale rounding'
       a.output.write_text(json.dumps(dict(scope='Real TiTiler 2.4.0 HTTP endpoints with bundled GDAL 3.12.4 JP2OpenJPEG; fresh object URL for cold states, warm processes and OS cache. Same source pixels/extent/output dimensions, 16-bit to 8-bit rescaling may differ. River first request includes lazy header open. No Kakadu binary or 11 GB source measurement.',sourceBytes=stat.st_size,sourceImagesUnchanged=True,results=results,events=events),indent=2))
     finally:
      if version=='river':request(a.river,'/api/s3/sources/'+sid,method='DELETE')
finally:server.shutdown()
