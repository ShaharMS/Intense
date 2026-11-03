"""Measure whether an active late-tile scan delays an already discovered early tile."""
import argparse,base64,concurrent.futures,io,json,pathlib,socket,subprocess,threading,time,urllib.parse,urllib.request,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image

p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--runs',type=int,default=3);p.add_argument('--protocol',choices=['http','websocket'],default='http');p.add_argument('--real',action='store_true');a=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[1];fixture=root/'reports/source-fairness-m106/layered-large.jp2';stat=fixture.stat()
reference=Image.open(root/'reports/source-fairness-m106/layered-reference.png').convert('RGBA')
ws_reference=Image.open(root/'reports/source-fairness-m106/layered-preview.ppm').convert('RGBA')
early_xy=(0,0);late_xy=(0,7);real_references={}
if a.real:
    assert a.protocol=='http','Real-source reduced overview parity is not assumed'
    fixture=root/'corpus/cache/sentinel-date-mosaics/2016-09-12.jp2';stat=fixture.stat();early_xy=(12,0);late_xy=(55,13)
    real_references={early_xy:Image.open(root/'reports/same-source-readiness-m165/early-reference.png').convert('RGBA'),late_xy:Image.open(root/'reports/gdal-random-m143/full-detail-core-timed.gdal.png').convert('RGBA')}
ws_script=r'''
const [api,asset,x,y]=process.argv.slice(1);const ws=new WebSocket(api.replace('http','ws')+'/ws/tiles');ws.binaryType='arraybuffer';const start=performance.now();
const timeout=setTimeout(()=>{console.error('WS timed out');process.exit(1)},45000);
ws.onopen=()=>ws.send(JSON.stringify({type:'tiles',driver:'openjpeg',format:'png',batch:{assetId:asset,sequence:1,tiles:[{x:Number(x),y:Number(y),z:0}]}}));
ws.onmessage=e=>{if(typeof e.data==='string'){const m=JSON.parse(e.data);if(m.type==='error'){console.error(m);process.exit(1)}return;}
const b=Buffer.from(e.data),n=b.readUInt32LE(4),header=JSON.parse(b.subarray(8,8+n));if(header.pass!=='preview')return;
clearTimeout(timeout);console.log(JSON.stringify({ms:performance.now()-start,png:b.subarray(8+n).toString('base64'),header}));ws.close();};
'''
events=[];active=threading.Event();armed=False;armed_gets=0;started=time.perf_counter()
class S3(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
    def log_message(self,*_):pass
    def do_GET(self):
        global armed_gets
        assert fixture.stat().st_mtime_ns==stat.st_mtime_ns
        lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));hi=min(hi,stat.st_size-1)
        assert hi-lo+1<=1024*1024
        if armed:
            armed_gets+=1
            if armed_gets>=10:active.set()
        begin=(time.perf_counter()-started)*1000;time.sleep(.01)
        with fixture.open('rb') as f:f.seek(lo);body=f.read(hi-lo+1)
        events.append(dict(start=lo,bytes=len(body),startMs=begin,endMs=(time.perf_counter()-started)*1000))
        self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{stat.st_size}');self.send_header('ETag','"stable"');self.end_headers();self.wfile.write(body)
server=ThreadingHTTPServer(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(path,body=None,method=None):
    with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=60) as response:return json.load(response) if response.status!=204 else None
rows=[]
try:
    for trial in range(a.runs):
        sid='ready-'+uuid.uuid4().hex[:12];created=False;armed=False;active.clear();armed_gets=0;event_start=len(events)
        try:
            request('/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=f'http://host.docker.internal:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True));created=True
            asset=request('/api/s3/sources/'+sid+'/assets',dict(key='image.jp2'))['assetId'];manifest=request('/api/assets/'+asset+'/manifest');matrix=manifest['resolutionLevels']-1
            def tile(x,y):
                if a.protocol=='websocket':
                    done=subprocess.run(['node','-e',ws_script,a.api,asset,str(x),str(y)],capture_output=True,text=True,timeout=60);assert done.returncode==0,done.stderr
                    result=json.loads(done.stdout);actual=Image.open(io.BytesIO(base64.b64decode(result.pop('png')))).convert('RGBA');expected=ws_reference.crop((x*128,y*128,(x+1)*128,(y+1)*128));assert actual.size==expected.size and actual.tobytes()==expected.tobytes()
                    return result|dict(pixels='exact')
                url=a.api+f'/streamer/v1/{matrix}/{x}/{y}/tile.png?'+urllib.parse.urlencode(dict(edge='crop',image=asset,driver='openjpeg'));begin=time.perf_counter()
                with urllib.request.urlopen(url,timeout=60) as response:body=response.read();headers=dict(response.headers)
                elapsed=(time.perf_counter()-begin)*1000;actual=Image.open(io.BytesIO(body)).convert('RGBA');expected=real_references[(x,y)] if a.real else reference.crop((x*512,y*512,(x+1)*512,(y+1)*512));assert actual.size==expected.size and actual.tobytes()==expected.tobytes()
                return dict(ms=elapsed,pixels='exact',headers=headers)
            armed=True
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                late=pool.submit(tile,*late_xy);assert active.wait(10),'late scan never became active'
                early=pool.submit(tile,*early_xy);early_result=early.result(timeout=60);early_before_late=not late.done();late_result=late.result(timeout=60)
            row=dict(trial=trial,early=early_result,late=late_result,earlyFinishedBeforeLate=early_before_late,rangeRequests=len(events)-event_start,sourceBytes=sum(e['bytes'] for e in events[event_start:]));rows.append(row)
            print(json.dumps({k:v for k,v in row.items() if k not in ['early','late']}|dict(earlyMs=early_result['ms'],lateMs=late_result['ms'])),flush=True)
        finally:
            armed=False
            if created:request('/api/s3/sources/'+sid,method='DELETE')
    a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(dict(protocol=a.protocol,source=str(fixture),objectBytes=stat.st_size,scope='Fresh S3 identities, 10 ms per GET. Synthetic source has trailing padding; real source is unchanged 978 MB corpus JP2. Start a late tile then an early tile after ten source GETs. HTTP full-detail 512px or synthetic WebSocket first-preview 128px. Independent original pixels checked. Header opening outside timing. WS timing begins before connection, excludes Node startup.',rows=rows),indent=2))
finally:server.shutdown()
