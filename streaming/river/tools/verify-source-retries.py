"""Independent origin counters verify configured S3 retry limits and persistence."""
import argparse,hashlib,importlib.util,io,json,pathlib,socket,subprocess,threading,time,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.parse import urlencode,urlsplit
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--physical-limit',type=int);p.add_argument('--api',required=True);p.add_argument('--container',required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();assert a.container.startswith('river-goal-') and not a.output.exists()
spec=importlib.util.spec_from_file_location('h',pathlib.Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
root=pathlib.Path('reports/fixed-gate-v2');manifest=json.loads((root/'manifest.json').read_bytes());c=next(c for c in manifest['cases'] if c['name']=='mb50-deep');file=(root/c['file']).resolve()
with file.open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==c['sha256']
gold=Image.open(root/c['reference']).convert('RGB');events=[];lock=threading.Lock();rows=[];sources=[]
class Server(ThreadingHTTPServer):daemon_threads=True;request_queue_size=256
class Origin(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1';wbufsize=262144
 def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
 def log_message(self,*args):pass
 def do_GET(self):
  path=urlsplit(self.path).path;key=path.split('/')[-1];native=self.headers.get('User-Agent')=='River/native-curl';start,end=map(int,self.headers['Range'][6:].split('-'));end=min(end,c['sourceBytes']-1)
  with lock:
   failures=sum(e['path']==path and e['status']==503 for e in events)
   failure=(key=='sdk-fail.jp2' or native and key=='native-fail.jp2' or key=='sdk-once.jp2' and failures==0 or native and key=='native-once.jp2' and failures==0)
   events.append(dict(path=path,native=native,status=503 if failure else 206,start=start,end=end))
  self.send_response(503 if failure else 206);self.send_header('Content-Length','0' if failure else str(end-start+1))
  if not failure:self.send_header('ETag','"'+c['sha256']+'"');self.send_header('Content-Range',f'bytes {start}-{end}/{c["sourceBytes"]}')
  self.end_headers()
  if failure:return
  try:
   with file.open('rb') as f:
    f.seek(start);left=end-start+1
    while left:
     block=f.read(min(65536,left));self.wfile.write(block);left-=len(block)
   self.wfile.flush()
  except (BrokenPipeError,ConnectionResetError):pass
server=Server(('0.0.0.0',18998),Origin);threading.Thread(target=server.serve_forever,daemon=True).start()
def policy():return json.loads(h.http(a.api+'/api/settings/read-policy'))['settings']
def select(mode):h.http(a.api+'/api/settings/read-policy',dict(policy(),serialTransport=mode),'PUT')
def image(endpoint,key):
 q=dict(image=endpoint+'/fixture/'+key,driver=c['driver'],edge='crop',tileSize=c['tileSize'],minZoom=0,maxZoom=c['maxZoom'],tilePixelRatio=1)
 return a.api+f"/streamer/v1/{c['z']}/{c['x']}/{c['y']}/tile.png?"+urlencode(q)
report=dict(passed=False,rows=rows,scope='Fault behavior on immutable frozen50MB fixture; not a successful cold-load timing benchmark.')
saved=policy()
try:
 h.http(a.api+'/api/diagnostics/source-reads',dict(enabled=True),'PUT')
 for mode in ['sdk','curl']:
  select(mode)
  for limit in [0,2,4]:
   sid='retry-'+uuid.uuid4().hex;endpoint='http://host.docker.internal:18998/'+sid
   source=dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True,maxRetries=limit)
   before=len(events);h.http(a.api+'/api/s3/sources',source,'POST');sources.append(sid);assert len(events)==before
   assert next(s for s in json.loads(h.http(a.api+'/api/s3/sources')) if s['id']==sid)['maxRetries']==limit
   key='sdk-fail.jp2' if mode=='sdk' else 'native-fail.jp2'
   if mode=='curl':h.http(a.api+'/api/s3/sources/'+sid+'/assets',dict(key=key),'POST')
   h.http(a.api+'/api/diagnostics/source-reads',method='DELETE')
   before=len(events)
   try:h.http(image(endpoint,key));raise AssertionError('expected failure')
   except RuntimeError as error:assert '503' in str(error) or 'physical request attempt limit' in str(error) or a.physical_limit is not None and 'range budget' in str(error),str(error)
   attempts=events[before:];assert len(attempts)==min(limit+1,a.physical_limit or limit+1),(mode,limit,attempts)
   assert all(e['status']==503 and e['native']==(mode=='curl') for e in attempts)
   trace=json.loads(h.http(a.api+'/api/diagnostics/source-reads'));assert trace['evicted']==0
   assert not trace['summary']['partial'];assert trace['summary']['physicalAttempts']==len(attempts);assert sum(g['physicalAttempts'] for g in trace['summary']['groups'])==len(attempts)
   traced=[e for e in trace['events'] if e['kind']=='wireAttempt'] if mode=='sdk' else [e for e in trace['events'] if e['purpose']=='nativeAttempt']
   assert len(traced)==len(attempts),(mode,traced,attempts)
   assert all(e['physicalAttempts']==1 and e['start']==attempts[i]['start'] and e['endExclusive']==attempts[i]['end']+1 for i,e in enumerate(traced))
   if mode=='sdk':assert all(e['httpStatus']==503 for e in traced)
   rows.append(dict(mode=mode,maxRetries=limit,physicalFailedGets=len(attempts),registrationIoZero=True,traceMatchesFailedWire=True,trace=traced))
   if limit==2 and a.physical_limit is None:
    key='sdk-once.jp2' if mode=='sdk' else 'native-once.jp2';body=h.http(image(endpoint,key));actual=Image.open(io.BytesIO(body)).convert('RGB');assert actual.size==gold.size and actual.tobytes()==gold.tobytes()
    failures=[e for e in events if e['path']==f'/{sid}/fixture/{key}' and e['status']==503];assert len(failures)==1
    before=len(events);assert h.http(image(endpoint,key))==body;assert len(events)==before
    rows.append(dict(mode=mode,transientRecoveryPixelsExact=True,repeatIoZero=True))
  if a.physical_limit is not None:
   key='healthy-budget.jp2'
   h.http(a.api+'/api/s3/sources/'+sid+'/assets',dict(key=key),'POST')
   before=len(events)
   try:h.http(image(endpoint,key));raise AssertionError('Expected budget exhaustion')
   except RuntimeError as error:assert 'physical request attempt limit' in str(error) or 'range request limit' in str(error) or 'range budget' in str(error),str(error)
   attempts=events[before:];assert len(attempts)==a.physical_limit,(mode,attempts)
   rows.append(dict(mode=mode,healthySourceStoppedAtBudget=len(attempts),fallbackDidNotBypassLimit=True))
 exported=json.loads(h.http(a.api+'/api/settings/configuration'))
 assert all(next(s for s in exported['sources'] if s['id']==sid)['maxRetries'] in [0,2,4] for sid in sources)
 subprocess.run(['docker','restart',a.container],check=True,stdout=subprocess.DEVNULL)
 for _ in range(100):
  try:policy();break
  except (OSError,RuntimeError):time.sleep(.1)
 restored=json.loads(h.http(a.api+'/api/s3/sources'));assert all(next(s for s in restored if s['id']==sid)['maxRetries']==next(s for s in exported['sources'] if s['id']==sid)['maxRetries'] for sid in sources)
 h.http(a.api+'/api/settings/configuration',exported,'PUT');assert json.loads(h.http(a.api+'/api/settings/configuration'))['sources']==exported['sources']
 report.update(passed=True,restartPreservesLimits=True,configurationRoundtripExact=True)
finally:
 for sid in sources:h.http(a.api+'/api/s3/sources/'+sid,method='DELETE')
 h.http(a.api+'/api/settings/read-policy',saved,'PUT');server.shutdown();report['events']=events;a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps({k:v for k,v in report.items() if k!='events'}))
