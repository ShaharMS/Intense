"""Verify Crystal tile footprints, output density, native-level fallback and cache isolation."""
import argparse,io,json,pathlib,socket,threading,urllib.parse,urllib.request,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.error import HTTPError
from PIL import Image,ImageFile

p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--endpoint-host',default='host.docker.internal');p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--resolution-levels',type=int,default=1);p.add_argument('--native-tile-size',type=int,default=256);a=p.parse_args();assert 16<=a.native_tile_size<=4096
pixels=bytes((x*17+y*23+c*61)%256 for y in range(530) for x in range(700) for c in range(3))
buffer=io.BytesIO();Image.frombytes('RGB',(700,530),pixels).save(buffer,format='JPEG2000',irreversible=False,num_resolutions=a.resolution_levels,tile_size=(a.native_tile_size,a.native_tile_size),plt=True,mct=1)
data=buffer.getvalue();source='edge-'+uuid.uuid4().hex[:12];events=[];cases=[];created=False
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
            assert self.headers.get('If-Match')=='"edge"' or (start,end)==(0,1023)
        payload=data[start:end+1];events.append((self.command,start,end))
        self.send_response(206 if body else 200);self.send_header('Content-Length',str(len(payload)));self.send_header('ETag','"edge"');self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT')
        if body:self.send_header('Content-Range',f'bytes {start}-{end}/{len(data)}')
        self.end_headers()
        if body:self.wfile.write(payload)
server=Origin(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(path,body=None,method=None,extra=None):
    headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'};headers.update(extra or {})
    with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers=headers),timeout=60) as r:return r.read(),r.headers
try:
    request('/api/s3/sources',dict(id=source,name=source,bucket=source,region='us-east-1',endpoint=f'http://{a.endpoint_host}:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True));created=True
    # No registration or manifest call: the first tile lazily opens the connected object URL.
    uri='s3://'+source+'/folder/a%20%23%25.jp2'
    drivers=[d['id'] for d in json.loads(request('/api/drivers')[0]) if d['available']]
    for driver in drivers:
        for size,maxzoom,z,x,y,ratio in [(256,7,7,0,0,1),(256,7,7,0,0,2),(256,7,7,0,0,1.5),(256,8,7,0,0,1),(512,7,7,0,0,0.5),(256,7,7,1,1,1),(256,7,6,1,0,1),(256,7,5,0,0,1),(256,7,0,0,0,1),(256,20,20,1,0,2),(512,8,8,1,1,1),(256,7,6,1,1,1.5),(256,7,7,2,2,0.5)]:
            span=size*(2**(maxzoom-z));edge=round(size*ratio)
            natural=Image.frombytes('RGB',(700,530),pixels).convert('RGBA').crop((x*span,y*span,min((x+1)*span,700),min((y+1)*span,530)))
            w=(natural.width*edge+span-1)//span;h=(natural.height*edge+span-1)//span
            reduction=min(a.resolution_levels-1,max(0,(span//edge).bit_length()-1),(span&-span).bit_length()-1)
            native=Image.open(io.BytesIO(data));scale=1<<reduction
            if reduction:
                # JP2 component dimensions use ceil; Pillow's public reduce
                # loader rounds odd dimensions and can allocate one row short.
                native._size=tuple((n+scale-1)//scale for n in native.size);t=native.tile[0];args=list(t.args);args[1]=reduction
                native.tile=[ImageFile._Tile(t.codec_name,(0,0,*native.size),t.offset,tuple(args))];ImageFile.ImageFile.load(native)
            else:native.load()
            native=native.convert('RGBA')
            native=native.crop((x*span//scale,y*span//scale,min(((x+1)*span+scale-1)//scale,native.width),min(((y+1)*span+scale-1)//scale,native.height)))
            expected=native.resize((w,h),Image.Resampling.BILINEAR)
            for mode in ['crop','black','transparent']:
                q=dict(image=uri,tileSize=size,maxZoom=maxzoom,tilePixelRatio=ratio,edge=mode,driver=driver)
                url=f'/streamer/v1/{z}/{x}/{y}/tile.png?'+urllib.parse.urlencode(q)
                body,headers=request(url);actual=Image.open(io.BytesIO(body)).convert('RGBA')
                oracle=expected
                if mode!='crop':
                    oracle=Image.new('RGBA',(edge,edge),(0,0,0,255 if mode=='black' else 0));oracle.paste(expected,(0,0))
                assert actual.size==oracle.size,(q,actual.size,oracle.size)
                difference=max(abs(a-b) for a,b in zip(actual.tobytes(),oracle.tobytes()))
                assert difference<=2,(q,difference)
                again,hit=request(url);assert again==body and hit['X-River-Source-Read-Bytes']=='0' and hit['X-River-Decoder-Route']=='encoded-raster-cache'
                cases.append(dict(driver=driver,size=size,maxZoom=maxzoom,z=z,position=[x,y],ratio=ratio,mode=mode,output=actual.size,maxPixelError=difference))
        # Format is real encoding; density and alpha survive each encoder.
        for fmt in ['webp','avif']:
            url='/streamer/v1/7/2/2/tile.'+fmt+'?'+urllib.parse.urlencode(dict(image=uri,tileSize=256,maxZoom=7,tilePixelRatio=2,edge='transparent',driver=driver))
            body,headers=request(url);im=Image.open(io.BytesIO(body)).convert('RGBA');assert im.size==(512,512) and im.getpixel((511,511))[3]==0
    before=len(events)
    for query in [dict(tileSize=256),dict(maxZoom=7),dict(tilePixelRatio=2),dict(tileSize=0,maxZoom=7),dict(tileSize=256,maxZoom=6),dict(tileSize=256,maxZoom=7,minZoom=8),dict(tileSize=256,maxZoom=7,tilePixelRatio=0),dict(tileSize=512,maxZoom=30)]:
        try:request('/streamer/v1/7/0/0/tile.png?'+urllib.parse.urlencode(dict(image=uri,**query)));raise AssertionError('invalid geometry accepted')
        except HTTPError as error:
            detail=json.loads(error.read());assert error.code==400 and detail['code']=='invalid_tile_geometry',(query,detail)
    assert len(events)==before,'invalid geometry fetched source bytes'
    a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(dict(passed=True,nativeResolutionLevels=a.resolution_levels,drivers=drivers,cases=cases,invalidCases=8),indent=2))
    print(json.dumps(dict(passed=True,cases=len(cases),drivers=drivers)))
finally:
    if created:request('/api/s3/sources/'+source,method='DELETE')
    server.shutdown()
