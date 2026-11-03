"""Cold locator-to-PNG requests: source opening is inside the measured GET."""
import argparse,hashlib,io,json,pathlib,socket,statistics,threading,time,urllib.parse,urllib.request,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--old-api',required=True);p.add_argument('--new-api',required=True);p.add_argument('--baseline',default='checkpoint 140');p.add_argument('--candidate',default='candidate');p.add_argument('--source',type=pathlib.Path);p.add_argument('--preview-reduction',type=int,default=2,choices=range(1,6));p.add_argument('--latency-ms',type=float,default=10);p.add_argument('--rendered-cache-enabled',action='store_true');p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();root=pathlib.Path(__file__).resolve().parents[1];source=a.source or root/'reports/dense-indexed-m144/valid-indexed.jp2';stat=source.stat();events=[];rows=[]
full=Image.open(source).convert('RGBA');preview=Image.open(source);preview.reduce=a.preview_reduction;preview.load();preview=preview.convert('RGBA')
class S3(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
    def log_message(self,*args):pass
    def do_GET(self):
        lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));hi=min(hi,stat.st_size-1);assert hi-lo+1<=1024*1024
        assert self.headers.get('If-Match')=='"stable"' or (lo,hi)==(0,1023)
        assert source.stat().st_mtime_ns==stat.st_mtime_ns;time.sleep(a.latency_ms/1000)
        with source.open('rb') as f:f.seek(lo);body=f.read(hi-lo+1)
        events.append(len(body));self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{stat.st_size}');self.send_header('ETag','"stable"');self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT');self.end_headers();self.wfile.write(body)
class Origin(ThreadingHTTPServer):
    request_queue_size=256
server=Origin(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(api,path,body=None,method=None):
    with urllib.request.urlopen(urllib.request.Request(api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=60) as response:return json.load(response) if response.status!=204 else None
try:
    for trial in range(3):
        for label,api in ([('old',a.old_api),('new',a.new_api)] if trial%2==0 else [('new',a.new_api),('old',a.old_api)]):
            for case in ['detail','preview']:
                sid='direct-'+uuid.uuid4().hex[:12];request(api,'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=f'http://host.docker.internal:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True))
                x,y=[(1,1),(0,1),(1,0)][trial] if case=='detail' else (0,0);matrix=5 if case=='detail' else 5-a.preview_reduction;expected=full.crop((x*512,y*512,(x+1)*512,(y+1)*512)) if case=='detail' else preview
                try:
                    url=api+f'/streamer/v1/{matrix}/{x}/{y}/tile.png?'+urllib.parse.urlencode(dict(edge='crop',image='s3://fixture/image.jp2',driver='openjpeg'))
                    for state in ['cold','repeat']:
                        before=len(events);started=time.perf_counter()
                        with urllib.request.urlopen(url,timeout=60) as response:
                            timing={key:float(response.headers.get('X-River-'+header,'0')) for key,header in [('sourceMs','Source-Cache-Ms'),('decodeMs','Decode-Ms'),('encodeMs','Encode-Ms'),('serverMs','Server-Ms')]}
                            route=response.headers['X-River-Decoder-Route']
                            body=response.read();image=Image.open(io.BytesIO(body)).convert('RGBA')
                        elapsed=(time.perf_counter()-started)*1000;assert image.size==expected.size and image.tobytes()==expected.tobytes(),'Independent source pixels differ'
                        if a.rendered_cache_enabled and state=='repeat':assert route=='encoded-raster-cache' and len(events)==before,'Repeated request did not use the encoded cache'
                        rows.append(dict(trial=trial,version=label,case=case,state=state,x=x,y=y,ms=elapsed,sourceBytes=sum(events[before:]),rangeRequests=len(events)-before,pixels='exact',pngSha256=hashlib.sha256(body).hexdigest(),route=route,**timing))
                finally:request(api,'/api/s3/sources/'+sid,method='DELETE')
    summary=[dict(case=case,state=state,**{v:{key:statistics.median(r[key] for r in rows if r['case']==case and r['state']==state and r['version']==v) for key in ['ms','sourceBytes','rangeRequests']} for v in ['old','new']}) for case in ['detail','preview'] for state in ['cold','repeat']]
    report=dict(baseline=a.baseline,candidate=a.candidate,source=str(source),objectBytes=stat.st_size,scope='Three alternating trials, direct S3-locator compatibility GET, OpenJPEG/PNG. Header opening and client PNG decoding included, browser paint excluded. Fresh source/image identities; OS cache warm and transport reused. Controlled six-resolution RGB JP2. Native reduced-resolution and full-detail pixels checked against independent Pillow decoding. Detail positions vary across three tiles.',previewSize=list(preview.size),detailSize=[512,512],renderedCacheEnabled=a.rendered_cache_enabled,latencyMs=a.latency_ms,measurements=summary,samples=rows)
    a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2));print(json.dumps(summary,indent=2))
finally:server.shutdown()
