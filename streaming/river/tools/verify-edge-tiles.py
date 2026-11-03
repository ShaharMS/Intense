"""Pixel and wire-header checks for edge modes across HTTP and both WS request forms."""
import argparse,base64,http.client,io,json,pathlib,socket,subprocess,threading,time,urllib.parse,urllib.request,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.error import HTTPError
from PIL import Image

p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--endpoint-host',default='host.docker.internal');p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--require-full-cache-reuse',action='store_true');p.add_argument('--restart-container');a=p.parse_args()
if a.restart_container and not a.restart_container.startswith('river-goal-'):p.error('Only a task-owned test container may be restarted')
pixels=bytes((x*17+y*23+c*61)%256 for y in range(530) for x in range(700) for c in range(3))
buffer=io.BytesIO();Image.frombytes('RGB',(700,530),pixels).save(buffer,format='JPEG2000',irreversible=False,num_resolutions=6,tile_size=(256,256),plt=True,mct=1)
data=buffer.getvalue();source='edge-'+uuid.uuid4().hex[:12];events=[];cases=[];created=False;initial_paths=set();initial_lock=threading.Lock()
class Origin(ThreadingHTTPServer):request_queue_size=256
class S3(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
    def log_message(self,*_):pass
    def do_HEAD(self):self.reply(False)
    def do_GET(self):self.reply(True)
    def reply(self,body):
        start,end=0,len(data)-1
        if body:
            assert self.headers.get('Range');start,end=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));end=min(end,len(data)-1)
            if self.headers.get('If-Match')!='"edge"':
                with initial_lock:
                    assert start==0 and 0<=end<4096 and self.path not in initial_paths
                    initial_paths.add(self.path)
        payload=data[start:end+1];events.append((self.command,start,end))
        self.send_response(206 if body else 200);self.send_header('Content-Length',str(len(payload)));self.send_header('ETag','"edge"');self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT')
        if body:self.send_header('Content-Range',f'bytes {start}-{end}/{len(data)}')
        self.end_headers()
        if body:self.wfile.write(payload)
server=Origin(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(path,body=None,method=None,extra=None):
    headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'};headers.update(extra or {})
    with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers=headers),timeout=60) as r:return r.read(),r.headers
def oracle(x,y,reduction=0,source_edge=512):
    im=Image.open(io.BytesIO(data));im.reduce=reduction;im.load();im=im.convert('RGBA');edge=source_edge//(1<<reduction)
    return im.crop((x*edge,y*edge,min((x+1)*edge,im.width),min((y+1)*edge,im.height)))
def check(body,x,y,mode,reduction=0,lossy=False,source_edge=512):
    im=Image.open(io.BytesIO(body)).convert('RGBA');natural=oracle(x,y,reduction,source_edge);edge=source_edge//(1<<reduction)
    expected=natural
    if mode!='crop':
        expected=Image.new('RGBA',(edge,edge),(0,0,0,255 if mode=='black' else 0));expected.paste(natural,(0,0))
    assert im.size==expected.size,(mode,im.size,expected.size)
    if lossy:
        assert im.getpixel((edge-1,edge-1))[3]==(0 if mode=='transparent' else 255)
    else:assert im.tobytes()==expected.tobytes(),(mode,'pixel mismatch',reduction)
    return im.size
ws_script=r'''
const [url,request]=process.argv.slice(1),ws=new WebSocket(url);ws.binaryType='arraybuffer';const frames=[];
const timer=setTimeout(()=>{console.error('WS edge test timeout');process.exit(1)},30000);
ws.onerror=()=>{console.error('WS handshake or transport error');process.exit(1)};
ws.onopen=()=>ws.send(request);ws.onmessage=e=>{if(typeof e.data==='string'){const m=JSON.parse(e.data);if(m.type==='error'){console.error(m);process.exit(1)}return;}
const b=Buffer.from(e.data);if(b.subarray(0,4).toString()!=='RIVR')throw Error('bad frame');const n=b.readUInt32LE(4),header=JSON.parse(b.subarray(8,8+n));frames.push({header,png:b.subarray(8+n).toString('base64')});
if(header.pass==='final'){clearTimeout(timer);console.log(JSON.stringify(frames));ws.close();}};
'''
try:
    body,_=request('/api/s3/sources',dict(id=source,name=source,bucket=source,region='us-east-1',endpoint=f'http://{a.endpoint_host}:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True));created=True
    loaded,_=request('/api/s3/sources/'+source+'/assets',dict(key='edges.jp2'));asset=json.loads(loaded)['assetId']
    manifest=json.loads(request('/api/assets/'+asset+'/manifest')[0]);matrix=manifest['resolutionLevels']-1
    drivers=[d['id'] for d in json.loads(request('/api/drivers')[0]) if d['available']]
    def wmts(driver,x,y,mode=None,fmt='png',reduction=0,target=None):
        image=target or asset
        q=dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=image,STYLE='default',FORMAT='image/'+fmt,DRIVER=driver,TILEMATRIXSET='RiverPixel-'+image,TILEMATRIX=matrix-reduction,TILECOL=x,TILEROW=y)
        if mode is not None:q['EDGE']=mode
        return request('/wmts?'+urllib.parse.urlencode(q))
    for driver in drivers:
        if a.require_full_cache_reuse:
            for first_mode in ['black','transparent']:
                fresh=json.loads(request('/api/s3/sources/'+source+'/assets',dict(key=driver+'-'+first_mode+'-first.jp2'))[0])['assetId']
                initial,_=wmts(driver,0,0,first_mode,target=fresh);check(initial,0,0,first_mode)
                for next_mode in ['crop','black','transparent']:
                    repeated,headers=wmts(driver,0,0,next_mode,target=fresh)
                    assert repeated==initial and headers['X-River-Decoder-Route']=='encoded-raster-cache',(driver,first_mode,next_mode)
        for x,y in [(1,0),(0,1),(1,1),(0,0)]:
            for mode in ['crop','black','transparent']:
                body,_=wmts(driver,x,y,mode);size=check(body,x,y,mode)
                again,headers=wmts(driver,x,y,mode);assert again==body and headers['X-River-Source-Read-Bytes']=='0'
                cases.append(dict(driver=driver,protocol='wmts',mode=mode,position=[x,y],size=size))
        uri='s3://'+source+'/edges.jp2'
        body,_=request(f'/streamer/v1/{matrix}/1/1/tile.png?'+urllib.parse.urlencode(dict(image=uri,driver=driver)));check(body,1,1,'black')
        regular,_=wmts(driver,1,1);check(regular,1,1,'crop')
        coarse,_=request('/streamer/v1/0/0/0/tile.png?'+urllib.parse.urlencode(dict(image=uri,driver=driver)))
        check(coarse,0,0,'black',matrix,source_edge=512<<matrix)
        coarse,_=wmts(driver,0,0,reduction=matrix);check(coarse,0,0,'crop',matrix,source_edge=512<<matrix)
        for mode in ['crop','black','transparent']:
            comp,_=request(f'/streamer/v1/{matrix}/1/1/tile.png?'+urllib.parse.urlencode(dict(image=uri,driver=driver,edge=mode)))
            normal,_=wmts(driver,1,1,mode);assert comp==normal
        for mode in ['black','transparent']:
            for fmt in ['webp','avif']:
                body,_=wmts(driver,1,1,mode,fmt);check(body,1,1,mode,lossy=True)
        for endpoint,kind in [('/ws','viewport'),('/ws/tiles','tiles')]:
            for selected_mode in [None,'crop','black','transparent']:
                mode=selected_mode or 'crop'
                msg=dict(type=kind,format='png',driver=driver)
                if kind=='tiles':msg['batch']=dict(assetId=asset,sequence=1,tiles=[dict(x=1,y=1,z=0)])
                else:msg['viewport']=dict(assetId=asset,sequence=1,zoom=0,x=512,y=512,width=188,height=18)
                url=a.api.replace('http','ws',1)+endpoint+('' if selected_mode is None else '?edge='+mode)
                done=subprocess.run(['node','-e',ws_script,url,json.dumps(msg)],capture_output=True,text=True,timeout=45);assert done.returncode==0,done.stderr
                frames=json.loads(done.stdout);assert frames[-1]['header']['pass']=='final'
                for f in frames:
                    h=f['header'];reduction=2 if h['pass']=='preview' else 0;check(base64.b64decode(f['png']),1,1,mode,reduction)
                    assert (h['outputWidth'],h['outputHeight'])==((188,18) if mode=='crop' else (512,512)),h
                cases.append(dict(driver=driver,protocol=kind,mode=mode,default=selected_mode is None,passes=[f['header']['pass'] for f in frames]))
    before=len(events)
    bad=[('/wmts?EDGE=invalid',None),(f'/streamer/v1/{matrix}/1/1/tile.png?'+urllib.parse.urlencode(dict(image=asset,edge='invalid')),None)]
    upgrade={'Connection':'Upgrade','Upgrade':'websocket','Sec-WebSocket-Version':'13','Sec-WebSocket-Key':base64.b64encode(bytes(range(16))).decode()}
    bad.extend([(path+'?edge=invalid',upgrade) for path in ['/ws','/ws/tiles']])
    for path,headers in bad:
        if headers:
            # urllib deliberately rewrites Connection to close; use a real
            # upgrade request to exercise validation before the 101 response.
            base=urllib.parse.urlsplit(a.api);connection=http.client.HTTPConnection(base.hostname,base.port,timeout=10)
            connection.request('GET',path,headers=headers);response=connection.getresponse();body=json.loads(response.read());connection.close()
            assert response.status==400 and body['code']=='invalid_edge_mode',body
            continue
        try:request(path,extra=headers)
        except HTTPError as e:assert e.code==400 and json.loads(e.read())['code']=='invalid_edge_mode'
        else:raise AssertionError('invalid edge mode was accepted')
    assert len(events)==before,'invalid edge mode performed source IO'
    if a.restart_container:
        fresh=json.loads(request('/api/s3/sources/'+source+'/assets',dict(key='disk-crop-first.jp2'))[0])['assetId']
        saved,_=wmts('openjpeg',0,0,target=fresh);check(saved,0,0,'crop')
        with initial_lock:initial_paths.clear()
        subprocess.run(['docker','restart',a.restart_container],check=True,capture_output=True)
        for _ in range(150):
            try:
                request('/health');break
            except (OSError,http.client.HTTPException):time.sleep(.2)
        else:raise AssertionError('test instance did not restart')
        restored=json.loads(request('/api/s3/sources/'+source+'/assets',dict(key='disk-crop-first.jp2'))[0])['assetId'];assert restored==fresh
        body,headers=wmts('openjpeg',0,0,'black',target=restored)
        assert body==saved and headers['X-River-Decoder-Route']=='encoded-raster-cache'
    report=dict(passed=True,drivers=drivers,compatibilityDefault='black',regularDefault='crop',invalidCases=4,sourceImagesUnchanged=True,fullCacheReuseVerified=a.require_full_cache_reuse,diskRestartReuseVerified=bool(a.restart_container),cases=cases)
    a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2));print(json.dumps({k:v for k,v in report.items() if k!='cases'}|dict(cases=len(cases))))
finally:
    if created:request('/api/s3/sources/'+source,method='DELETE')
    server.shutdown()
