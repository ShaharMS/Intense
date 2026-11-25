"""An early non-retryable range failure must abandon held sibling reads."""
import argparse, io, json, pathlib, socket, threading, time, urllib.error, urllib.parse, urllib.request, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from PIL import Image

p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args()
image=Image.frombytes('RGB',(1536,1024),bytes((x*17+y*31+c*79)%256 for y in range(1024) for x in range(1536) for c in range(3)))
stream=io.BytesIO();image.save(stream,format='JPEG2000',irreversible=False,num_resolutions=5,tile_size=(256,256),precinct_size=(32,32),codeblock_size=(32,32),progression='PCRL',plt=True,mct=0);data=stream.getvalue()
box=0
while data[box+4:box+8]!=b'jp2c':box+=int.from_bytes(data[box:box+4],'big')
cursor=box+10
while data[cursor:cursor+2]!=b'\xff\x90':cursor+=2+int.from_bytes(data[cursor+2:cursor+4],'big')
payloads=[]
while data[cursor:cursor+2]==b'\xff\x90':
    length=int.from_bytes(data[cursor+6:cursor+10],'big');start=cursor+12
    while data[start:start+2]!=b'\xff\x93':start+=2+int.from_bytes(data[start+2:start+4],'big')
    payloads.append((start+2,cursor+length));cursor+=length
state={'enabled':True,'failedAt':None,'events':[]};lock=threading.Lock();sibling=threading.Event();delay=1.2
class Origin(ThreadingHTTPServer):request_queue_size=128
class S3(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def log_message(self,*_):pass
    def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
    def handle(self):
        try:super().handle()
        except (BrokenPipeError,ConnectionResetError):pass
    def do_HEAD(self):self.reply(False)
    def do_GET(self):self.reply(True)
    def reply(self,body):
        if not body:
            self.send_response(200);self.send_header('Content-Length',str(len(data)));self.send_header('ETag','"fault-v1"');self.send_header('Last-Modified','Wed, 14 Oct 2026 00:00:00 GMT');self.end_headers();return
        lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));hi=min(hi,len(data)-1)
        assert self.headers.get('If-Match')=='"fault-v1"' or (lo,hi)==(0,1023)
        entropy=any(start+512<=lo and hi<end for start,end in payloads)
        with lock:
            fault=state['enabled'] and entropy and state['failedAt'] is None
            if fault:state['failedAt']=-1
            hold=state['enabled'] and entropy and not fault and state['failedAt'] is not None
            event={'lo':lo,'hi':hi,'fault':fault,'held':hold};state['events'].append(event)
        if hold:sibling.set();time.sleep(delay)
        if fault:
            assert sibling.wait(3),'Fixture did not reach parallel entropy requests'
            with lock:state['failedAt']=time.perf_counter()
            self.send_response(412);self.send_header('Content-Length','0');self.end_headers();return
        self.send_response(206);self.send_header('Content-Range',f'bytes {lo}-{hi}/{len(data)}');self.send_header('Content-Length',str(hi-lo+1));self.send_header('ETag','"fault-v1"');self.send_header('Last-Modified','Wed, 14 Oct 2026 00:00:00 GMT');self.end_headers();self.wfile.write(data[lo:hi+1])
server=Origin(('127.0.0.1',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start();source='failure-'+uuid.uuid4().hex[:12];auth={'Authorization':'Bearer fixture-admin','Content-Type':'application/json'}
def request(path,value=None,method=None):
    return urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if value is None else json.dumps(value).encode(),method=method,headers=auth),timeout=30)
created=False
try:
    with request('/api/s3/sources',dict(id=source,name=source,bucket='fixture',endpoint=f'http://host.docker.internal:{server.server_port}',region='us-east-1',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True)):created=True
    with request('/api/s3/sources/'+source+'/assets',{'key':'image.jp2'}) as r:asset=json.load(r)['assetId']
    path='/streamer/v1/0/0/0/tile.png?'+urllib.parse.urlencode(dict(image=asset,driver='openhtj2k',edge='crop'))
    try:
        with request(path) as r:raise AssertionError('Failed range produced a raster')
    except urllib.error.HTTPError as e:
        error=json.loads(e.read());status=e.code;returned=time.perf_counter()
    with lock:
        assert state['failedAt'] and state['failedAt']>0 and sibling.is_set()
        elapsed=returned-state['failedAt'];assert elapsed<delay*.6,elapsed
        count=len(state['events']);state['enabled']=False
    assert status>=400 and error.get('requestId') and ('precondition' in error['message'].lower() or '412' in error['message']),error
    time.sleep(delay+.2)
    with lock:assert len(state['events'])==count,'Abandoned batch continued scheduling reads'
    with request(path) as r:actual=Image.open(io.BytesIO(r.read())).convert('RGBA')
    expected=Image.open(io.BytesIO(data));expected.reduce=4;expected.load();expected=expected.convert('RGBA')
    assert actual.size==expected.size and actual.tobytes()==expected.tobytes(),'Recovery pixels differ'
    report=dict(passed=True,errorStatus=status,errorCode=error['code'],failureReplyMs=elapsed*1000,heldSiblingMs=delay*1000,requestsAtFailure=count,noFurtherRequestsBeforeRetry=True,recoveryPixels='exact',scope='Injected412 while at least one sibling entropy request is held; verify early response, no later scheduling, then successful exact retry on the same asset.')
    a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2),encoding='utf-8');print(json.dumps(report))
finally:
    if created:
        with request('/api/s3/sources/'+source,method='DELETE'):pass
    server.shutdown()
