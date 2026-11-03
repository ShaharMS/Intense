"""API persistence, hot reader switching, physical retry counts and retry limits."""
import argparse,hashlib,importlib.util,io,json,pathlib,socket,subprocess,threading,time,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.parse import urlencode,urlsplit
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--container',required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();assert not a.output.exists();assert a.container.startswith('river-goal-')
spec=importlib.util.spec_from_file_location('h',pathlib.Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
root=pathlib.Path('reports/fixed-gate-v2');m=json.loads((root/'manifest.json').read_text());c=next(c for c in m['cases'] if c['name']=='mb50-deep');file=(root/c['file']).resolve()
with file.open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==c['sha256']
gold=Image.open(root/c['reference']).convert('RGB');sid='native-settings-'+uuid.uuid4().hex;events=[];lock=threading.Lock();rows=[]
class Server(ThreadingHTTPServer):daemon_threads=True;request_queue_size=256
class Origin(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1';wbufsize=262144
 def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
 def log_message(self,*args):pass
 def do_GET(self):
  key=urlsplit(self.path).path.split('/')[-1];native=self.headers.get('User-Agent')=='River/native-curl';start,end=map(int,self.headers['Range'][6:].split('-'));end=min(end,c['sourceBytes']-1)
  with lock:
   number=sum(e['key']==key and e['native'] for e in events)+1 if native else 0
   failure=native and ((key=='once.jp2' and number==1) or key in ['always.jp2','retry-after.jp2'])
   event=dict(key=key,native=native,status=503 if failure else 206,start=start,end=end,at=time.monotonic());events.append(event)
  if failure:
   self.send_response(503);self.send_header('Retry-After','1' if key=='retry-after.jp2' else '0');self.send_header('Content-Length','0');self.end_headers();return
  self.send_response(206);self.send_header('ETag','"'+c['sha256']+'"');self.send_header('Content-Range',f'bytes {start}-{end}/{c["sourceBytes"]}');self.send_header('Content-Length',str(end-start+1));self.end_headers()
  try:
   with file.open('rb') as f:
    f.seek(start);left=end-start+1
    while left:
     block=f.read(min(65536,left));self.wfile.write(block);left-=len(block)
   self.wfile.flush()
  except (BrokenPipeError,ConnectionResetError):pass
server=Server(('0.0.0.0',18998),Origin);threading.Thread(target=server.serve_forever,daemon=True).start();endpoint='http://host.docker.internal:18998/'+sid
report=dict(passed=False,rows=rows,scope='Dedicated trial container, frozen50MB file for API/retry behavior. Full three-size latency comparisons are separate. Source unchanged. Fixture credentials only.')
def policy():return json.loads(h.http(a.api+'/api/settings/read-policy'))
def select(mode):
 settings=policy()['settings'];settings['serialTransport']=mode;result=json.loads(h.http(a.api+'/api/settings/read-policy',settings,'PUT'));assert result['serialTransport']==mode
def image(key):
 q=dict(image=endpoint+'/fixture/'+key,driver=c['driver'],edge='crop',tileSize=c['tileSize'],minZoom=0,maxZoom=c['maxZoom'],tilePixelRatio=1)
 return a.api+f"/streamer/v1/{c['z']}/{c['x']}/{c['y']}/tile.png?"+urlencode(q)
try:
 assert policy()['settings']['serialTransport']=='sdk'
 h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
 h.http(a.api+'/api/s3/sources/'+sid+'/assets',dict(key='once.jp2'),'POST')
 select('curl');body=h.http(image('once.jp2'));actual=Image.open(io.BytesIO(body)).convert('RGB');assert actual.size==gold.size and actual.tobytes()==gold.tobytes()
 failures=[e for e in events if e['key']=='once.jp2' and e['status']==503];assert len(failures)==1
 native=[e for e in events if e['native']];observed=policy()['sources'][sid]['nativeSerialRequests'];assert observed==len(native) and observed>=2
 rows.append(dict(case='hot-switch-and-transient-retry',pixelsExact=True,failedRequests=1,nativeRequests=observed))
 exported=json.loads(h.http(a.api+'/api/settings/configuration'));assert exported['readPolicy']['serialTransport']=='curl'
 subprocess.run(['docker','restart',a.container],check=True,stdout=subprocess.DEVNULL)
 for _ in range(100):
  try:policy();break
  except (OSError,RuntimeError):time.sleep(.1)
 assert policy()['settings']['serialTransport']=='curl';assert any(s['id']==sid for s in json.loads(h.http(a.api+'/api/s3/sources')))
 rows.append(dict(case='restart-and-export',transportPersisted=True,sourcePersisted=True))
 for key in ['always.jp2','retry-after.jp2']:
  t=time.monotonic()
  try:h.http(image(key));raise AssertionError('Expected transient source failure')
  except RuntimeError as error:assert '503' in str(error) and 'throttled' in str(error)
  attempts=[e for e in events if e['key']==key and e['native']];assert len(attempts)==3,(key,attempts)
  if key=='retry-after.jp2':assert all(b['at']-x['at']>=.9 for x,b in zip(attempts,attempts[1:])),attempts
  rows.append(dict(case=key,attempts=3,elapsedMs=(time.monotonic()-t)*1000,retryAfterHonored=key=='retry-after.jp2'))
 select('sdk');before=len([e for e in events if e['native']]);body=h.http(image('sdk-again.jp2'));assert len([e for e in events if e['native']])==before
 actual=Image.open(io.BytesIO(body)).convert('RGB');assert actual.size==gold.size and actual.tobytes()==gold.tobytes()
 h.http(a.api+'/api/settings/configuration',exported,'PUT');assert policy()['settings']['serialTransport']=='curl';select('sdk')
 rows.append(dict(case='sdk-switch-and-config-import',pixelsExact=True,sdkUsesNoNativeRequests=True,importRestoresTransport=True))
 report['passed']=True
finally:
 report['events']=events;server.shutdown();a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
