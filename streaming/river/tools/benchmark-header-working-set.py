"""Isolate retained decoding headers after opening a diverse S3 working set."""
import argparse, concurrent.futures, io, json, pathlib, socket, threading, time, urllib.parse, urllib.request, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from PIL import Image

p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--label',required=True);a=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/header-working-set-m121'
source_file=out/'header-icc.jp2';stat=source_file.stat();events=[]
expected=Image.open(out/'icc-reference.ppm').convert('RGB')
class S3(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
    def log_message(self,*args):pass
    def do_GET(self):
        key=urllib.parse.urlparse(self.path).path
        lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));hi=min(hi,stat.st_size-1)
        assert self.headers.get('If-Match')=='"'+key+'"' or (lo,hi)==(0,1023)
        assert source_file.stat().st_mtime_ns==stat.st_mtime_ns
        with source_file.open('rb') as stream:stream.seek(lo);body=stream.read(hi-lo+1)
        time.sleep(.01);events.append(dict(key=key,offset=lo,bytes=len(body)))
        self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{stat.st_size}');self.send_header('ETag','"'+key+'"');self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT');self.end_headers();self.wfile.write(body)
server=ThreadingHTTPServer(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
source='headers-'+uuid.uuid4().hex[:12]
def request(path,body=None,method=None):
    with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=45) as response:return json.load(response) if response.status!=204 else None
def register(index):return request('/api/s3/sources/'+source+'/assets',{'key':f'image-{index}.jp2'})['assetId']
def view(asset):
    before=len(events);started=time.perf_counter()
    with urllib.request.urlopen(a.api+'/streamer/v1/0/0/0/tile.png?'+urllib.parse.urlencode(dict(edge='crop',image=asset,driver='jp2grok')),timeout=45) as response:image=Image.open(io.BytesIO(response.read())).convert('RGB')
    elapsed=(time.perf_counter()-started)*1000
    assert image.size==expected.size and image.tobytes()==expected.tobytes(),'Independent Grok CLI pixel mismatch'
    return dict(ms=elapsed,rangeRequests=len(events)-before,sourceBytes=sum(e['bytes'] for e in events[before:]),events=events[before:],pixels='exact')
created=False
try:
    request('/api/s3/sources',dict(id=source,name=source,bucket='fixture',region='us-east-1',endpoint=f'http://host.docker.internal:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True));created=True
    target=register(0);cold=view(target)
    started=time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:list(pool.map(register,range(1,513)))
    pressure_ms=(time.perf_counter()-started)*1000;revisit=view(target);repeat=view(target)
    report=dict(label=a.label,images=513,headerBudgetBytes=33554432,sharedBlockCacheBytes=0,renderedCacheBytes=0,rangeDelayMs=10,scope='Same-process revisit after 512 distinct image identities opened. Valid padded sRGB ICC metadata retained; independent CLI oracle. OS cache warm; source bytes unchanged.',cold=cold,pressureMs=pressure_ms,revisit=revisit,repeat=repeat)
    (out/(a.label+'.json')).write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2),flush=True)
finally:
    if created:request('/api/s3/sources/'+source,method='DELETE')
    server.shutdown()
