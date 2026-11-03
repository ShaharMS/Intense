"""Compare River/OpenJPEG with GDAL's JP2 range reader on the same cold ROI."""
import argparse,io,json,pathlib,socket,subprocess,threading,time,urllib.parse,urllib.request,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--pilot-delay-ms',type=float,default=0);p.add_argument('--restart-container');p.add_argument('--bandwidth-mib-s',type=float,default=0);p.add_argument('--repeat-runs',type=int,default=0);p.add_argument('--driver',default='openjpeg');p.add_argument('--endpoint-host',default='host.docker.internal');p.add_argument('--reference',type=pathlib.Path);p.add_argument('--source',type=pathlib.Path,required=True);p.add_argument('--x',type=int,required=True);p.add_argument('--y',type=int,required=True);p.add_argument('--reduction',type=int,default=0);p.add_argument('--latency-ms',type=float,default=10);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();a.output.parent.mkdir(parents=True,exist_ok=True)
if a.restart_container and not a.restart_container.startswith('river-goal-'):p.error('Only a task-owned test container may be restarted')
source=a.source.resolve();stat=source.stat();events=[];heads=[];phase='';etag='"stable"';origin_started=time.perf_counter();bandwidth_lock=threading.Lock();next_body_at=0.0
class S3(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
    def log_message(self,*args):pass
    def do_HEAD(self):
        heads.append(phase);time.sleep(a.latency_ms/1000);self.send_response(200);self.send_header('Content-Length',str(stat.st_size));self.send_header('ETag',etag);self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT');self.end_headers()
    def do_GET(self):
        get_started=(time.perf_counter()-origin_started)*1000;get_phase=phase
        requested=self.headers.get('Range');assert requested,'Unbounded image GET'
        lo,hi=requested.removeprefix('bytes=').split('-');lo=int(lo);hi=min(int(hi) if hi else stat.st_size-1,stat.st_size-1);assert hi-lo+1<=4*1024*1024
        assert source.stat().st_mtime_ns==stat.st_mtime_ns
        time.sleep(a.latency_ms/1000)
        if a.pilot_delay_ms and hi-lo+1==65536:time.sleep(a.pilot_delay_ms/1000)
        with source.open('rb') as stream:stream.seek(lo);body=stream.read(hi-lo+1)
        if a.bandwidth_mib_s:
            global next_body_at
            with bandwidth_lock:
                next_body_at=max(next_body_at,time.perf_counter())+len(body)/(a.bandwidth_mib_s*1024*1024)
                due=next_body_at
            time.sleep(max(0,due-time.perf_counter()))
        events.append(dict(phase=get_phase,start=lo,end=hi,bytes=len(body),startMs=get_started,endMs=(time.perf_counter()-origin_started)*1000));self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{stat.st_size}');self.send_header('ETag',etag);self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT');self.end_headers()
        try:self.wfile.write(body)
        except OSError:pass
class Origin(ThreadingHTTPServer):
    request_queue_size=256
server=Origin(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start();sid='gdal-'+uuid.uuid4().hex[:12]
def request(path,body=None,method=None):
    with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=60) as response:return json.load(response) if response.status!=204 else None
def measured(label,started):return dict(ms=(time.perf_counter()-started)*1000,rangeRequests=sum(e['phase']==label for e in events),sourceBytes=sum(e['bytes'] for e in events if e['phase']==label),headRequests=heads.count(label))
created=False
try:
    request('/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=f'http://{a.endpoint_host}:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True));created=True
    phase='river-open';started=time.perf_counter();meta=request('/api/s3/sources/'+sid+'/assets',dict(key='image.jp2'));opening=measured(phase,started);manifest=request('/api/assets/'+meta['assetId']+'/manifest');matrix=manifest['resolutionLevels']-1-a.reduction;edge=512*(1<<a.reduction);x0=a.x*edge;y0=a.y*edge;width=min(edge,manifest['width']-x0);height=min(edge,manifest['height']-y0);assert width>0 and height>0
    phase='river-tile';started=time.perf_counter();query=urllib.parse.urlencode(dict(edge='crop',image=meta['assetId'],driver=a.driver))
    with urllib.request.urlopen(a.api+f'/streamer/v1/{matrix}/{a.x}/{a.y}/tile.png?'+query,timeout=60) as response:
        body=response.read();headers=dict(response.headers)
    river=measured(phase,started);actual=Image.open(io.BytesIO(body)).convert('RGBA');actual.save(a.output.with_suffix('.river.png'))
    repeated=[]
    for run in range(a.repeat_runs):
        phase='river-repeat-'+str(run);started=time.perf_counter()
        with urllib.request.urlopen(a.api+f'/streamer/v1/{matrix}/{a.x}/{a.y}/tile.png?'+query,timeout=60) as response:
            repeat_body=response.read();repeat_headers=dict(response.headers)
        sample=measured(phase,started);assert repeat_body==body
        sample['serverMs']=float(next(v for k,v in repeat_headers.items() if k.lower()=='x-river-server-ms'))
        sample['encodedCacheHit']=repeat_headers.get('X-River-Decoder-Route',repeat_headers.get('x-river-decoder-route')) in ['encoded-raster-cache','encoded-raster-disk-cache']
        repeated.append(sample)

    restarted=None
    if a.restart_container:
        subprocess.run(['docker','restart',a.restart_container],check=True,capture_output=True,timeout=45)
        deadline=time.monotonic()+30
        while True:
            try:urllib.request.urlopen(a.api+'/health',timeout=2).close();break
            except Exception:
                if time.monotonic()>deadline:raise
                time.sleep(.25)
        phase='river-restart';started=time.perf_counter();restart_query=urllib.parse.urlencode(dict(edge='crop',image='s3://fixture/image.jp2',driver=a.driver))
        with urllib.request.urlopen(a.api+f'/streamer/v1/{matrix}/{a.x}/{a.y}/tile.png?'+restart_query,timeout=60) as response:restart_body=response.read()
        restarted=measured(phase,started);assert restart_body==body
        assert restarted['rangeRequests']<16 and restarted['sourceBytes']<4*1024*1024,'restart did not reuse the technical prefix index'
    if a.reference:
        expected=Image.open(a.reference).convert('RGBA');assert actual.size==expected.size and actual.tobytes()==expected.tobytes(),'Independent reference pixels differ'
        report=dict(scope='Cold River tile with fresh S3 identity, checked against an independent decoded-source reference. No GDAL process timing in this run.',driver=a.driver,reference=str(a.reference),sourceBytes=stat.st_size,latencyMs=a.latency_ms,riverOpen=opening,riverTile=river,riverRepeat=repeated,riverRestart=restarted,riverHeaders={k:v for k,v in headers.items() if k.lower().startswith('x-river-')},exactPixels=True,events=events)
        a.output.write_text(json.dumps(report,indent=2));print(json.dumps({k:v for k,v in report.items() if k!='events'},indent=2));raise SystemExit(0)
    phase='gdal';started=time.perf_counter();out=a.output.with_suffix('.gdal.png').resolve();maximum=(1<<manifest['bitDepth'])-1
    script="from osgeo import gdal;import time,json;gdal.UseExceptions();t=time.perf_counter();ds=gdal.Translate("+repr('/out/'+out.name)+","+repr('/vsicurl/'+f'http://host.docker.internal:{server.server_port}/fixture/image.jp2')+",format='PNG',outputType=gdal.GDT_Byte,scaleParams=[[0,"+str(maximum)+",0,255]],srcWin="+repr([x0,y0,width,height])+",width="+str(actual.width)+",height="+str(actual.height)+");ds=None;print(json.dumps({'operationMs':(time.perf_counter()-t)*1000}))"
    command=['docker','run','--rm','--entrypoint','python3','--mount',f'type=bind,source={out.parent},target=/out','-e','GDAL_DISABLE_READDIR_ON_OPEN=EMPTY_DIR','-e','CPL_VSIL_CURL_ALLOWED_EXTENSIONS=.jp2','-e','GDAL_HTTP_MERGE_CONSECUTIVE_RANGES=YES','-e','GDAL_HTTP_MULTIPLEX=YES','-e','VSI_CACHE=TRUE','-e','VSI_CACHE_SIZE=5000000','-e','GDAL_CACHEMAX=200','-e','GDAL_NUM_THREADS=1','-e','OPJ_NUM_THREADS=1','ghcr.io/osgeo/gdal:ubuntu-small-latest','-c',script]
    completed=subprocess.run(command,capture_output=True,text=True,timeout=120);assert completed.returncode==0,completed.stderr;gdal=measured(phase,started);gdal.update(json.loads(completed.stdout));expected=Image.open(out).convert('RGBA');pixels=actual.size==expected.size and actual.tobytes()==expected.tobytes()
    report=dict(scope='One cold ROI per reader, same unmodified JP2 and HTTP/1.1 origin, added per-request latency. River process warm but fresh S3 identity; GDAL starts in a new container/process, so its timing includes startup. GDAL uses TiTiler-recommended range/cache settings; this is a GDAL-core comparison, not the TiTiler HTTP application.',source=str(source),sourceBytes=stat.st_size,bitDepth=manifest['bitDepth'],window=[x0,y0,width,height],reduction=a.reduction,latencyMs=a.latency_ms,riverOpen=opening,riverTile=river,riverRepeat=repeated,riverRestart=restarted,gdal=gdal,exactPixels=pixels,riverHeaders={k:v for k,v in headers.items() if k.lower().startswith('x-river-')},events=events)
    a.output.write_text(json.dumps(report,indent=2));print(json.dumps({k:v for k,v in report.items() if k not in ['events','riverHeaders']},indent=2));assert pixels,'GDAL/River pixels differ; inspect saved images before claiming parity'
finally:
    if created:request('/api/s3/sources/'+sid,method='DELETE')
    server.shutdown()
