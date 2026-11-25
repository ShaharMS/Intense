"""Native bridge fault/TLS/signature tests. Run inside a task-owned Linux lab."""
import argparse,ctypes as C,hashlib,hmac,json,pathlib,re,socket,ssl,subprocess,tempfile,threading,time
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.parse import urlsplit,parse_qsl,quote,urlencode
p=argparse.ArgumentParser();p.add_argument('--library',required=True);a=p.parse_args();lib=C.CDLL(a.library)
class Request(C.Structure):
 _fields_=[(k,C.c_char_p) for k in ['url','signing','userpwd','token','etag','version','ca_info']]+[(k,C.c_uint64) for k in ['start','end','total','timeout_ms']]
class Response(C.Structure):_fields_=[('written',C.c_uint64),('status',C.c_int64),('curl_code',C.c_int32),('violation',C.c_int32),('requests',C.c_uint64),('retry_after_ms',C.c_uint64)]
Cancel=C.CFUNCTYPE(C.c_int,C.c_void_p)
lib.river_curl_create.restype=C.c_void_p;lib.river_curl_destroy.argtypes=[C.c_void_p]
lib.river_curl_fetch.argtypes=[C.c_void_p,C.POINTER(Request),C.c_void_p,C.c_size_t,Cancel,C.c_void_p,C.POINTER(Response)]
assert lib.river_curl_abi()==3 and lib.river_curl_request_size()==C.sizeof(Request) and lib.river_curl_response_size()==C.sizeof(Response)
data=bytes((i*17)%251 for i in range(65536));etag='"fixture-v1"';events=[];auth_errors=[];rows=[]
def signature(handler):
 auth=handler.headers.get('Authorization','');prefix='AWS4-HMAC-SHA256 '
 if not auth.startswith(prefix):return False
 fields=dict(v.strip().split('=',1) for v in auth[len(prefix):].split(','));scope=fields['Credential'].split('/');names=fields['SignedHeaders'].split(';')
 if scope[0]!='fixture' or scope[2:]!=['us-east-1','s3','aws4_request'] or names!=sorted(names):return False
 date=handler.headers['x-amz-date'];assert date[:8]==scope[1]
 if handler.headers.get('x-amz-security-token') and 'x-amz-security-token' not in names:return False
 parsed=urlsplit(handler.path);query='&'.join(k+'='+v for k,v in sorted((quote(k,safe='-_.~'),quote(v,safe='-_.~')) for k,v in parse_qsl(parsed.query,keep_blank_values=True)))
 headers=''.join(k+':'+re.sub(r'\s+',' ',handler.headers.get(k,'').strip())+'\n' for k in names)
 payload=handler.headers.get('x-amz-content-sha256',hashlib.sha256(b'').hexdigest())
 canonical='\n'.join([handler.command,parsed.path,query,headers,fields['SignedHeaders'],payload])
 text='\n'.join(['AWS4-HMAC-SHA256',date,'/'.join(scope[1:]),hashlib.sha256(canonical.encode()).hexdigest()])
 key=b'AWS4fixture'
 for value in scope[1:]:key=hmac.new(key,value.encode(),hashlib.sha256).digest()
 return hmac.compare_digest(hmac.new(key,text.encode(),hashlib.sha256).hexdigest(),fields['Signature'])
class Server(ThreadingHTTPServer):daemon_threads=True
class Handler(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
 def handle(self):
  try:super().handle()
  except (ConnectionResetError,BrokenPipeError):pass
 def log_message(self,*args):pass
 def do_GET(self):
  path=urlsplit(self.path);name=path.path.split('/')[-1];events.append(name)
  try:valid=signature(self)
  except Exception as e:auth_errors.append(str(e));valid=False
  if not valid:
   auth_errors.append(name+': signature mismatch');self.send_response(403);self.send_header('Content-Length','0');self.end_headers();return
  if name=='session' and self.headers.get('x-amz-security-token')!='session-fixture':
   self.send_response(403);self.send_header('Content-Length','0');self.end_headers();return
  if name=='precondition' or self.headers.get('If-Match')!=etag:
   self.send_response(412);self.send_header('Content-Length','0');self.end_headers();return
  if name=='throttled':
   self.send_response(503);self.send_header('Retry-After','1');self.send_header('Content-Length','0');self.end_headers();return
  start,end=map(int,self.headers['Range'][6:].split('-'));payload=data[start:end+1];version=dict(parse_qsl(path.query)).get('versionId')
  try:
   if name in ['slow-headers','timeout']:time.sleep(.7)
   if name=='redirect':
    self.send_response(307);self.send_header('Location','http://127.0.0.1:'+str(self.server.server_port)+'/fixture/leak');self.send_header('Content-Length','0');self.end_headers();return
   self.send_response(200 if name=='ignore-range' else 206)
   self.send_header('ETag','"wrong"' if name=='bad-etag' else etag)
   self.send_header('Content-Range',f'bytes {start+int(name=="bad-range")}-{end}/{len(data)}')
   if version and name!='missing-version':self.send_header('x-amz-version-id','wrong-version' if name=='bad-version' else version)
   if name=='oversized-chunked':
    self.send_header('Transfer-Encoding','chunked');self.end_headers();self.wfile.write(b'80\r\n'+data[:128]+b'\r\n0\r\n\r\n');self.wfile.flush();return
   self.send_header('Content-Length',str(len(payload)+(1 if name=='oversized' else 0)));self.end_headers()
   if name=='slow-body':self.wfile.write(payload[:1]);self.wfile.flush();time.sleep(.7);payload=payload[1:]
   if name=='truncated':payload=payload[:len(payload)//2];self.close_connection=True
   self.wfile.write(payload);self.wfile.flush()
  except (BrokenPipeError,ConnectionResetError,ssl.SSLError):pass
def run(handle,base,name,expected,*,version=None,token=None,cancel_after=None,timeout=2000,ca=None,capacity=64):
 url=base+'/fixture/'+name+(('?'+urlencode({'versionId':version})) if version else '')
 req=Request(url.encode(),b'aws:amz:us-east-1:s3',b'fixture:fixture',token.encode() if token else None,etag.encode(),version.encode() if version else None,ca.encode() if ca else None,0,63,len(data),timeout)
 out=C.create_string_buffer(64);response=Response();began=time.monotonic();cb=Cancel(lambda _:int(cancel_after is not None and time.monotonic()-began>=cancel_after))
 code=lib.river_curl_fetch(handle,C.byref(req),out,capacity,cb,None,C.byref(response));elapsed=(time.monotonic()-began)*1000
 row=dict(case=name,code=code,status=response.status,curlCode=response.curl_code,violation=response.violation,written=response.written,requests=response.requests,retryAfterMs=response.retry_after_ms,elapsedMs=elapsed);rows.append(row)
 assert code==expected,(row,auth_errors)
 expected_violation={'bad-range':1,'bad-etag':2,'missing-version':3,'bad-version':3,'oversized':4,'oversized-chunked':5,'truncated':6}.get(name)
 if expected_violation is not None:assert response.violation==expected_violation,row
 if code==0:assert out.raw==data[:64]
 if code==0:assert response.requests==1,row
 if cancel_after is not None or name=='timeout':assert elapsed<200,row
 return row
report={'passed':False,'rows':rows,'scope':'Independent SigV4 HMAC verification, bounded body/range/version checks, cancellation before headers/during body, deadlines, reuse after faults, and TLS trust/hostname validation. Fixture credentials only.'};handle=lib.river_curl_create();assert handle
server=Server(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start();base='http://127.0.0.1:'+str(server.server_port)
try:
 run(handle,base,'good',0)
 run(handle,base,'key%20with%20space%23%25.jp2',0)
 run(handle,base,'version',0,version='v1+/%')
 run(handle,base,'session',0,token='session-fixture')
 run(handle,base,'session',3,token='wrong-session')
 for name,expected in [('ignore-range',3),('bad-range',4),('bad-etag',4),('oversized',4),('oversized-chunked',4),('truncated',4),('precondition',3),('redirect',3)]:run(handle,base,name,expected)
 run(handle,base,'missing-version',4,version='v1')
 run(handle,base,'bad-version',4,version='v1')
 retry=run(handle,base,'throttled',3);assert retry['retryAfterMs']==1000 and retry['requests']==1
 before=len(events);run(handle,base,'already-cancelled',1,cancel_after=0);assert len(events)==before
 run(handle,base,'slow-headers',1,cancel_after=.04);run(handle,base,'slow-body',1,cancel_after=.04)
 run(handle,base,'timeout',2,timeout=40);run(handle,base,'good-after-faults',0)
 before=len(events);run(handle,base,'invalid-capacity',6,capacity=8);assert len(events)==before
 assert 'leak' not in events
 with tempfile.TemporaryDirectory(prefix='river-curl-tls-') as folder:
  cert=pathlib.Path(folder)/'cert.pem';key=pathlib.Path(folder)/'key.pem'
  subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-keyout',str(key),'-out',str(cert),'-days','1','-subj','/CN=localhost','-addext','subjectAltName=DNS:localhost'],check=True,capture_output=True)
  tls=Server(('127.0.0.1',0),Handler);ctx=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);ctx.load_cert_chain(cert,key);tls.socket=ctx.wrap_socket(tls.socket,server_side=True);threading.Thread(target=tls.serve_forever,daemon=True).start()
  try:
   url='https://localhost:'+str(tls.server_port);run(handle,url,'untrusted-tls',5)
   run(handle,url,'trusted-tls',0,ca=str(cert));run(handle,url,'untrusted-after-trusted',5);run(handle,'https://127.0.0.1:'+str(tls.server_port),'wrong-hostname',5,ca=str(cert))
  finally:tls.shutdown();tls.server_close()
 report['passed']=True
finally:
 lib.river_curl_destroy(handle);server.shutdown();server.server_close();report['authErrors']=auth_errors;print(json.dumps(report))
