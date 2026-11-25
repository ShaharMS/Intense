"""Verify cross-source connection reuse without sharing signing credentials."""
import argparse,io,json,pathlib,socket,threading,time,urllib.parse,urllib.request,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--connection-delay-ms',type=float,default=50);a=p.parse_args()
objects=[];references=[]
for seed in [5,11]:
    pixels=bytes((x*seed+y*31+c*83)&255 for y in range(128) for x in range(128) for c in range(3));stream=io.BytesIO();Image.frombytes('RGB',(128,128),pixels).save(stream,format='JPEG2000',irreversible=False,num_resolutions=4);objects.append(stream.getvalue());image=Image.open(io.BytesIO(objects[-1]));image.reduce=3;image.load();references.append(image.convert('RGBA'))
events=[];connections=[];violations=[]
class S3(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def setup(self):
        super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1);connections.append(self.client_address[1]);time.sleep(a.connection_delay_ms/1000)
    def log_message(self,*args):pass
    def do_GET(self):
        bucket,key=urllib.parse.urlparse(self.path).path.strip('/').split('/',1);index=int(bucket.removeprefix('bucket-'))
        if f'Credential=access-{index}/' not in self.headers.get('Authorization','') or self.headers.get('X-Amz-Security-Token')!=f'session-{index}':
            violations.append(index);self.send_response(403);self.send_header('Content-Length','0');self.end_headers();return
        data=objects[index%2];lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));hi=min(hi,len(data)-1);etag=f'"object-{index}"'
        assert self.headers.get('If-Match')==etag or (lo,hi)==(0,min(1023,len(data)-1))
        body=data[lo:hi+1];events.append(dict(source=index,bytes=len(body),port=self.client_address[1]));self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{len(data)}');self.send_header('ETag',etag);self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT');self.end_headers();self.wfile.write(body)
server=ThreadingHTTPServer(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start();sources=[];opened=[];prefix='transport-'+uuid.uuid4().hex[:8]
def request(path,body=None,method=None):
    with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=30) as response:return json.load(response) if response.status!=204 else None
try:
    for index in range(8):
        source=f'{prefix}-{index}';request('/api/s3/sources',dict(id=source,name=source,bucket=f'bucket-{index}',region='us-east-1',endpoint=f'http://host.docker.internal:{server.server_port}',accessKeyId=f'access-{index}',secretAccessKey=f'private-{index}',sessionToken=f'session-{index}',allowHttp=True));sources.append(source)
    for source in sources:
        started=time.perf_counter();value=request('/api/s3/sources/'+source+'/assets',dict(key='image.jp2'));opened.append(dict(source=source,asset=value['assetId'],openMs=(time.perf_counter()-started)*1000))
    open_gets=len(events);open_bytes=sum(e['bytes'] for e in events);open_connections=len(connections)
    for index,value in enumerate(opened):
        query=urllib.parse.urlencode(dict(edge='crop',image=value['asset'],driver='openjpeg'))
        with urllib.request.urlopen(a.api+'/streamer/v1/0/0/0/tile.png?'+query,timeout=30) as response:image=Image.open(io.BytesIO(response.read())).convert('RGBA')
        assert image.size==references[index%2].size and image.tobytes()==references[index%2].tobytes(),'cross-source pixels'
        before=len(events);assert request('/api/s3/sources/'+sources[index]+'/assets',dict(key='image.jp2'))['cached'];assert len(events)==before
    assert not violations
    report=dict(scope='Eight distinct configured buckets/credentials/session tokens on one HTTP/1.1 origin; 50 ms delay only when a new TCP connection is accepted. This models connection setup, not a real TLS benchmark. Source images are distinct synthetic lossless RGB fixtures.',connectionDelayMs=a.connection_delay_ms,opens=opened,openConnections=open_connections,openGets=open_gets,openBytes=open_bytes,allConnections=len(connections),requestCredentialsVerified=True,crossSourcePixels='exact',cachedReopenGets=0,passed=True)
    a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
finally:
    for source in sources:request('/api/s3/sources/'+source,method='DELETE')
    server.shutdown()
