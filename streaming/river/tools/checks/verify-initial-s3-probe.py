"""Snapshot selection: GET-only metadata, clipped tiny objects, fallback and races."""
import argparse,hashlib,io,json,struct,threading,urllib.request,urllib.parse,urllib.error,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output',required=True);a=p.parse_args();calls=[]
def picture(size,color):
 stream=io.BytesIO();Image.new('RGB',(size,size),color).save(stream,format='JPEG2000',irreversible=False,num_resolutions=4);return stream.getvalue()
small=picture(64,(17,40,90));replacement=picture(128,(130,20,12));assert len(small)<1024 and len(replacement)<1024
large=bytearray(picture(512,(81,121,163)));pos=0
while large[pos+4:pos+8]!=b'jp2c':pos+=int.from_bytes(large[pos:pos+4],'big')
large[pos:pos]=struct.pack('>I4s',2048,b'free')+bytes(2040);large=bytes(large)
class S3(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def mode(self):return urllib.parse.urlparse(self.path).path.split('/')[-1].removesuffix('.jp2')
 def respond(self,body,etag,status=200,lo=0,hi=None,total=None,write=True):
  self.send_response(status);self.send_header('Content-Length',str(len(body) if total is None else total));self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT')
  if etag:self.send_header('ETag',etag)
  if status==206:self.send_header('Content-Range',f'bytes {lo}-{hi}/{total}')
  self.end_headers()
  if write:self.wfile.write(body)
 def do_HEAD(self):
  mode=self.mode();calls.append({'mode':mode,'method':'HEAD'})
  if mode=='fallback':self.respond(replacement,'"new"',write=False)
  else:self.respond(b'',None,405)
 def do_GET(self):
  mode=self.mode();conditional=self.headers.get('If-Match');lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));calls.append({'mode':mode,'method':'GET','range':[lo,hi],'ifMatch':conditional,'version':urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query).get('versionId',[None])[0]})
  version=urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query).get('versionId',[None])[0]
  if conditional is None and version is None:assert lo==0 and hi==1023
  if mode=='ignored':self.respond(b'', '"stable"',200,total=512*1024*1024,write=False);return
  if mode=='changed' and conditional:self.respond(b'',None,412);return
  if mode=='version':
   assert conditional is None and (version=='saved-version' or (version is None and lo==0 and hi==1023));data=large;hi=min(hi,len(data)-1);body=data[lo:hi+1];self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{len(data)}');self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT');self.send_header('x-amz-version-id','saved-version');self.end_headers();self.wfile.write(body);return
  data=large if mode=='changed' else replacement if mode=='fallback' and conditional else small
  expected='"new"' if mode=='fallback' else '"v1"' if mode=='changed' else '"stable"'
  if conditional:assert conditional==expected
  hi=min(hi,len(data)-1);body=data[lo:hi+1];self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{len(data)}');self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT')
  if mode!='fallback':self.send_header('ETag',expected)
  self.end_headers();self.wfile.write(body)
server=ThreadingHTTPServer(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start();source='initial-'+uuid.uuid4().hex[:12];created=False;rows=[]
def request(path,body=None,method=None):
 with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=30) as response:return response.read()
try:
 request('/api/s3/sources',dict(id=source,name=source,bucket='fixture',region='us-east-1',endpoint=f'http://host.docker.internal:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True));created=True
 for mode in ['tiny','fallback','version','changed','ignored']:
  start=len(calls)
  try:
   opened=json.loads(request('/api/s3/sources/'+source+'/assets',{'key':mode+'.jp2'}))
   assert mode in ['tiny','fallback','version'],mode
   expected_size=64 if mode=='tiny' else 512 if mode=='version' else 128;assert opened['width']==opened['height']==expected_size
   if mode=='tiny':
    identity=[source,f'http://host.docker.internal:{server.server_port}','fixture','tiny.jp2',len(small),'\"stable\"',None,'2026-10-15T00:00:00Z'];expected_id='s3v-'+hashlib.sha256(json.dumps(identity,separators=(',',':')).encode()).hexdigest();assert opened['assetId']==expected_id,'legacy HEAD metadata identity changed'
   url='/streamer/v1/0/0/0/tile.png?'+urllib.parse.urlencode({'image':opened['assetId'],'driver':'jp2grok'});actual=Image.open(io.BytesIO(request(url))).convert('RGBA');expected=Image.new('RGBA',(expected_size//8,expected_size//8),(17,40,90,255) if mode=='tiny' else (81,121,163,255) if mode=='version' else (130,20,12,255));assert actual.size==expected.size and actual.tobytes()==expected.tobytes()
  except urllib.error.HTTPError as error:
   body=json.load(error);assert body['message'] and body['requestId']
   assert (mode,error.code,body['code']) in [('changed',422,'s3_header_failed'),('ignored',502,'s3_probe_failed')],(mode,error.code,body)
  subset=calls[start:];heads=sum(v['method']=='HEAD' for v in subset)
  assert heads==(1 if mode=='fallback' else 0),(mode,subset)
  if mode=='changed':assert any(v.get('ifMatch')=='"v1"' for v in subset)
  if mode=='fallback':assert any(v.get('ifMatch')=='"new"' for v in subset)
  rows.append({'mode':mode,'heads':heads,'calls':subset,'passed':True})
 open(a.output,'w',encoding='utf8').write(json.dumps({'cases':rows,'passed':True},indent=2));print(json.dumps(rows))
finally:
 if created:request('/api/s3/sources/'+source,method='DELETE')
 server.shutdown()
