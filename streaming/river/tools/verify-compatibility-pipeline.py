"""Compatibility-copy faults, cancellation, and same-image retry recovery."""
import argparse,hashlib,io,json,pathlib,socket,subprocess,threading,time,urllib.parse,urllib.request,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--container',required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();assert a.container.startswith('river-goal-')
root=pathlib.Path(__file__).resolve().parents[1];data=(root/'reports/uncached-prefix-m126/shallow.jp2').read_bytes();started=threading.Event();events=[];all_events=[];mode='';results=[]
reference=Image.open(io.BytesIO(data));reference.reduce=1;reference.load();reference=reference.convert('RGBA').crop((0,0,512,512))
class S3(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
    def handle(self):
        try:super().handle()
        except (ConnectionResetError,ConnectionAbortedError,BrokenPipeError):pass
    def log_message(self,*args):pass
    def do_GET(self):
        lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));hi=min(hi,len(data)-1);full=hi-lo+1==1024*1024;current=mode
        all_events.append((current,lo,hi))
        assert self.headers.get('If-Match')=='"fixed"' or (lo,hi)==(0,1023)
        if full:events.append((current,lo));started.set()
        if full and current=='changed':self.send_response(412);self.send_header('Content-Length','0');self.end_headers();return
        if full and current=='cancel':time.sleep(2)
        body=data[lo:hi+1]
        if full and current=='truncate':body=body[:-1]
        shift=int(full and current=='range')
        self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo+shift}-{hi+shift}/{len(data)}');self.send_header('ETag','"fixed"');self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT');self.end_headers()
        try:self.wfile.write(body)
        except OSError:pass
server=ThreadingHTTPServer(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(path,body=None,method=None):
    with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=30) as response:return json.load(response) if response.status!=204 else None
def files():
    script="from pathlib import Path;import json;print(json.dumps(sorted(p.name for p in Path('/var/lib/river/s3-cache/range-views').glob('river-compatible-*'))))"
    return set(json.loads(subprocess.check_output(['docker','exec',a.container,'python3','-c',script],text=True)))
node=r'''
const cfg=JSON.parse(process.argv[1]);const ws=new WebSocket(cfg.url);const timer=setTimeout(()=>{console.error('timeout');process.exit(1)},15000);
ws.onopen=()=>ws.send(JSON.stringify({type:'viewport',viewport:{assetId:cfg.asset,sequence:1,x:0,y:0,width:2048,height:2048,zoom:1},format:'png',driver:'jp2grok'}));
process.stdin.once('data',()=>ws.close());
ws.onmessage=e=>{if(typeof e.data!=='string'){console.error('Unexpected image from faulty copy');process.exit(1)}const v=JSON.parse(e.data);if(v.type==='error'){if(!v.code||!v.message)process.exit(1);console.log(JSON.stringify(v));ws.close();}};
ws.onclose=()=>{clearTimeout(timer);process.exit(0)};
'''
try:
    for mode in ['range','truncate','changed','cancel']:
        source='pipeline-'+uuid.uuid4().hex[:12];before=files();started.clear();marker=len(events);child=None
        request('/api/s3/sources',dict(id=source,name=source,bucket='fixture',region='us-east-1',endpoint=f'http://host.docker.internal:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True))
        try:
            asset=request('/api/s3/sources/'+source+'/assets',dict(key=mode+'.jp2'))['assetId']
            child=subprocess.Popen(['node','-e',node,json.dumps(dict(url=a.api.replace('http','ws',1)+'/ws',asset=asset))],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            assert started.wait(10),'Compatibility copy did not start'
            if mode=='cancel':child.stdin.write('cancel\n');child.stdin.flush()
            stdout,stderr=child.communicate(timeout=15);assert child.returncode==0,stderr
            if mode!='cancel':assert json.loads(stdout)['type']=='error',stdout
            deadline=time.monotonic()+5
            while files()!=before and time.monotonic()<deadline:time.sleep(.05)
            assert files()==before,'Failed/cancelled copy retained a temporary file'
            count=len(events)-marker
            if mode=='cancel':
                time.sleep(2.2);assert len(events)-marker==count and count<=4,'Cancellation kept scheduling compatibility ranges'
            failure_mode=mode;mode='healthy';retry_started=time.perf_counter()
            tile_url=a.api+'/streamer/v1/0/0/0/tile.png?'+urllib.parse.urlencode(dict(edge='crop',image=asset,driver='jp2grok'))
            with urllib.request.urlopen(tile_url,timeout=15) as response:image=Image.open(io.BytesIO(response.read())).convert('RGBA')
            retry_ms=(time.perf_counter()-retry_started)*1000
            assert image.size==reference.size and image.tobytes()==reference.tobytes(),'Retry pixels differ'
            retained=files()-before;assert len(retained)==1,'Successful retry did not retain exactly one complete copy'
            filename=next(iter(retained));script="from pathlib import Path;import hashlib;p=Path('/var/lib/river/s3-cache/range-views')/"+repr(filename)+";f=p.open('rb');print(hashlib.file_digest(f,'sha256').hexdigest())"
            digest=subprocess.check_output(['docker','exec',a.container,'python3','-c',script],text=True).strip();assert digest==hashlib.sha256(data).hexdigest(),'Retry retained a corrupt or reordered copy'
            reads=len(all_events)
            with urllib.request.urlopen(tile_url,timeout=15) as response:response.read()
            assert len(all_events)==reads,'Repeated retry fetched the source again'
            results.append(dict(mode=failure_mode,rangeRequests=count,temporaryFileRemoved=True,retryMs=retry_ms,retryPixels='exact',retryCopySha256=digest,repeatedRetrySourceRequests=0,passed=True))
        finally:
            if child and child.poll() is None:child.kill();child.wait()
            request('/api/s3/sources/'+source,method='DELETE')
finally:server.shutdown()
a.output.write_text(json.dumps(results,indent=2));print(json.dumps(results,indent=2))
