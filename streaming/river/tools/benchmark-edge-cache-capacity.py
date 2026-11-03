"""Bounded-RAM cross-protocol cache workload with independent pixel and wire checks.

Run against a task-owned instance with a 16 MiB raster cache and disk cache off.
Distinct S3 keys contain the same controlled high-entropy JP2 fixture. This
isolates image identity and cache pressure; it is not an S3 RTT benchmark.
"""
import argparse,hashlib,io,json,pathlib,socket,statistics,threading,time,urllib.parse,urllib.request,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image

p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--runs',type=int,default=1);p.add_argument('--cache-bytes',type=int,default=16*1024*1024);a=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[1];source=root/'reports/dense-indexed-m144/valid-indexed.jp2';stat=source.stat();data=source.read_bytes();reference=Image.open(io.BytesIO(data)).convert('RGBA').crop((0,0,512,512))
events=[];rows=[]
class Origin(ThreadingHTTPServer):request_queue_size=256
class S3(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
    def log_message(self,*_):pass
    def do_GET(self):
        assert source.stat().st_mtime_ns==stat.st_mtime_ns
        key=urllib.parse.urlsplit(self.path).path.rsplit('/',1)[-1];etag='"cache-'+key+'"'
        start,end=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));end=min(end,len(data)-1)
        assert self.headers.get('If-Match')==etag or (start,end)==(0,1023)
        assert end-start+1<=1024*1024
        body=data[start:end+1];events.append(len(body));self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {start}-{end}/{len(data)}');self.send_header('ETag',etag);self.end_headers();self.wfile.write(body)
server=Origin(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(path,body=None,method=None):
    with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=60) as r:return r.read(),r.headers
try:
    for trial in range(a.runs):
        sid='capacity-'+uuid.uuid4().hex[:12];created=False
        try:
            request('/api/s3/sources',dict(id=sid,name=sid,bucket=sid,region='us-east-1',endpoint=f'http://host.docker.internal:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True));created=True
            def load(key):return json.loads(request('/api/s3/sources/'+sid+'/assets',dict(key=key))[0])['assetId']
            probe=load('probe.jp2');meta=json.loads(request('/api/assets/'+probe+'/manifest')[0]);matrix=meta['resolutionLevels']-1
            def tile(asset,phase):
                if phase=='compatibility_black':url='/streamer/v1/'+str(matrix)+'/0/0/tile.png?'+urllib.parse.urlencode(dict(image=asset))
                else:
                    q=dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/png',DRIVER='openjpeg',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=matrix,TILECOL=0,TILEROW=0)
                    if phase=='transparent':q['EDGE']='transparent'
                    url='/wmts?'+urllib.parse.urlencode(q)
                started=time.perf_counter();body,headers=request(url);elapsed=(time.perf_counter()-started)*1000
                return body,dict(ms=elapsed,route=headers['X-River-Decoder-Route'],decodeMs=float(headers['X-River-Decode-Ms']),encodeMs=float(headers['X-River-Encode-Ms']),serverMs=float(headers['X-River-Server-Ms']),sourceBytes=int(headers['X-River-Source-Read-Bytes']))
            probe_body,_=tile(probe,'crop_cold');actual=Image.open(io.BytesIO(probe_body)).convert('RGBA');assert actual.size==reference.size and actual.tobytes()==reference.tobytes()
            digest=hashlib.sha256(probe_body).hexdigest();size=len(probe_body);count=min(24,a.cache_bytes//size-2)
            assert count>=3 and (count+1)*size<a.cache_bytes and 3*count*size>a.cache_bytes,(count,size,a.cache_bytes)
            assets=[load(f'image-{i}.jp2') for i in range(count)]
            for phase in ['crop_cold','compatibility_black','transparent','crop_repeat']:
                before=len(events);samples=[]
                for asset in assets:
                    body,sample=tile(asset,phase);assert hashlib.sha256(body).hexdigest()==digest,'Full-tile pixel or encoding identity changed across edge modes'
                    samples.append(sample)
                row=dict(trial=trial,phase=phase,images=count,encodedTileBytes=size,workingSetBytes=count*size,cacheHits=sum(s['route']=='encoded-raster-cache' for s in samples),decodeJobs=sum(s['decodeMs']>0 for s in samples),medianMs=statistics.median(s['ms'] for s in samples),serverTotalMs=sum(s['serverMs'] for s in samples),decodeTotalMs=sum(s['decodeMs'] for s in samples),encodeTotalMs=sum(s['encodeMs'] for s in samples),rangeRequests=len(events)-before,sourceBytes=sum(events[before:]),samples=samples)
                rows.append(row);print(json.dumps({k:v for k,v in row.items() if k!='samples'}),flush=True)
        finally:
            if created:request('/api/s3/sources/'+sid,method='DELETE')
    report=dict(scope='RAM-only cache capacity test; 16 MiB raster cache, disk cache disabled. Unique S3 identities with the same fixed high-entropy JP2. Headers opened before timing; no injected GET delay; source bytes version-pinned. Full 512px interior PNG independently decoded once, every subsequent wire PNG SHA-256 verified identical. Source unchanged. This measures repeated/cross-protocol capacity, not isolated cold-load improvement.',cacheBytes=a.cache_bytes,rows=rows)
    a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2))
finally:server.shutdown()
