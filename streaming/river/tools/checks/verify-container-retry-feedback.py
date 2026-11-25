"""A successful SDK retry must remain unmeasured when failed-body bytes are unknown."""
import io,json,threading,urllib.request,urllib.parse,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
from PIL import Image
ROOT=Path(__file__).resolve().parents[2];sid='retry-'+uuid.uuid4().hex
class Proxy(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1';failed=False
 def log_message(self,*args):pass
 def do_GET(self):
  if not Proxy.failed:
   Proxy.failed=True;body=b'<Error><Code>SlowDown</Code><Message>Controlled retry fixture</Message></Error>'
   self.send_response(503);self.send_header('Content-Length',str(len(body)));self.send_header('Retry-After','0');self.end_headers();self.wfile.write(body);return
  req=urllib.request.Request('http://127.0.0.1:18992'+self.path,headers={k:v for k,v in self.headers.items() if k.lower() not in ['host','connection']})
  with urllib.request.urlopen(req,timeout=30) as r:
   body=r.read();self.send_response(r.status)
   for k,v in r.headers.items():
    if k.lower() not in ['server','date','connection','transfer-encoding']:self.send_header(k,v)
   self.end_headers();self.wfile.write(body)
server=ThreadingHTTPServer(('0.0.0.0',18993),Proxy);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
def call(path,body=None,method=None):
 req=urllib.request.Request('http://127.0.0.1:18080'+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers={'Content-Type':'application/json'})
 with urllib.request.urlopen(req,timeout=60) as r:return r.read()
try:
 call('/api/s3/sources',dict(id=sid,name='Retry fixture',bucket='fixture',region='us-east-1',endpoint='http://host.containers.internal:18993/'+sid,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
 locator='s3-'+json.dumps([sid,'synthetic-texture-tlm-plt-d5.jp2'],separators=(',',':')).encode().hex()
 data=call('/streamer/v1/4/0/0/tile.png?'+urllib.parse.urlencode(dict(image=locator,driver='openjpeg')))
 with Image.open(io.BytesIO(data)) as im:actual=im.convert('RGB').tobytes()
 with Image.open(ROOT/'corpus/synthetic-texture-reference.png') as im:assert actual==im.convert('RGB').crop((0,0,256,256)).tobytes()
 policy=json.loads(call('/api/settings/read-policy'))['sources'][sid];assert Proxy.failed;assert policy['metadataOverfetch']['unmeasuredSamples']>=1,policy
 (ROOT/'target/tests/container-retry-feedback.json').write_text(json.dumps(dict(passed=True,exactPixels=True,retryExcluded=True,source=policy),indent=2)+'\n');print('Successful retry excluded from measured feedback; output pixels exact')
finally:
 call('/api/s3/sources/'+sid,method='DELETE');server.shutdown();server.server_close()
