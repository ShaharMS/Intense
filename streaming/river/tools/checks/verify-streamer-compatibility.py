"""Verify lazy image-URL tiles match WMTS and reuse its encoded cache."""
import argparse,io,json,threading,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.parse import urlparse,urlencode,unquote,quote
from urllib.request import Request,urlopen
from urllib.error import HTTPError
from PIL import Image

p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output');a=p.parse_args()
pixels=bytes((x*17+y*31+c*83)&255 for y in range(512) for x in range(512) for c in range(3))
buffer=io.BytesIO();Image.frombytes('RGB',(512,512),pixels).save(buffer,format='JPEG2000',irreversible=False,num_resolutions=6)
data=buffer.getvalue();key='folder/actual # % + & ? א image.jp2';calls=[]
other_key='folder/second %23 literal.jp2'
other_pixels=bytes(255-value for value in pixels)
other_buffer=io.BytesIO();Image.frombytes('RGB',(512,512),other_pixels).save(other_buffer,format='JPEG2000',irreversible=False,num_resolutions=6)
objects={key:data,other_key:other_buffer.getvalue()}
class S3(BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_HEAD(self):self.respond(False)
    def do_GET(self):self.respond(True)
    def respond(self,content):
        object_key=unquote(urlparse(self.path).path).removeprefix('/fixture/')
        assert object_key in objects,self.path
        calls.append((self.command,self.headers.get('Range')))
        object_data=objects[object_key]
        body=object_data;start=0;end=len(object_data)-1;status=200
        if content and self.headers.get('Range'):
            start,end=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));body=object_data[start:end+1];status=206
        self.send_response(status);self.send_header('Content-Type','application/octet-stream');self.send_header('Content-Length',str(len(body)))
        self.send_header('ETag','"fixture"');self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT')
        if status==206:self.send_header('Content-Range',f'bytes {start}-{end}/{len(object_data)}')
        self.end_headers()
        if content:self.wfile.write(body)
server=ThreadingHTTPServer(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(path,body=None,method=None):
    with urlopen(Request(a.api+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'},method=method),timeout=30) as response:
        return response.read(),response.headers
source='compat-'+uuid.uuid4().hex[:12];created=False
try:
    endpoint=f'http://host.docker.internal:{server.server_port}'
    request('/api/s3/sources',dict(id=source,name=source,endpoint=endpoint,bucket='fixture',region='us-east-1',prefix='folder',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True,virtualHostedStyle=False));created=True
    uri='s3://fixture/'+quote(key,safe='/')
    path='/streamer/v1/0/0/0/tile.png?'+urlencode(dict(edge='crop',image=uri,driver='jp2grok'))
    cold,headers=request(path);assert cold.startswith(b'\x89PNG')
    first_calls=len(calls)
    locator='s3-'+json.dumps([source,key],separators=(',',':'),ensure_ascii=False).encode().hex()
    manifest=json.loads(request('/api/assets/'+locator+'/manifest')[0]);asset=manifest['assetId']
    wmts='/wmts?'+urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/png',DRIVER='jp2grok',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=0,TILEROW=0,TILECOL=0))
    normal,normal_headers=request(wmts);assert cold==normal
    assert normal_headers['x-river-source-read-bytes']=='0'
    http_url=endpoint+'/fixture/'+quote(key,safe='/')+'?X-Amz-Credential=test%2Fscope&X-Amz-Signature=ignored&X-Amz-Security-Token=a%2Bb%2Fc%3D'
    repeated,repeated_headers=request('/streamer/v1/0/0/0/tile.png?'+urlencode(dict(edge='crop',image=http_url,driver='jp2grok')))
    assert repeated==cold and len(calls)==first_calls
    assert headers['Content-Type'].startswith('image/png') and headers['x-river-trace-id']
    failure_path='/streamer/v1/0/1/1/tile.png?'+urlencode(dict(edge='crop',image=http_url,driver='jp2grok'))
    try:request(failure_path)
    except HTTPError as error:
        failure=json.loads(error.read())
        assert error.code==404 and failure['code']=='tile_out_of_bounds',failure
        assert failure['requestId']==error.headers['x-river-request-id']
        failure_evidence=dict(url=a.api+failure_path,status=error.code,headers=dict(error.headers),body=failure)
    else:raise AssertionError('Out-of-bounds tile was accepted')
    openjpeg,openjpeg_headers=request('/streamer/v1/0/0/0/tile.png?'+urlencode(dict(edge='crop',image=uri,driver='openjpeg')))
    assert Image.open(io.BytesIO(openjpeg)).convert('RGBA').tobytes()==Image.open(io.BytesIO(cold)).convert('RGBA').tobytes()
    default,default_headers=request('/streamer/v1/0/0/0/tile.png?'+urlencode(dict(edge='crop',image=uri)))
    assert default==openjpeg
    try:request('/streamer/v1/0/0/0/tile.png?'+urlencode(dict(edge='crop',image=uri,driver='unknown')))
    except HTTPError as error:assert error.code==400
    else:raise AssertionError('Unknown driver was accepted')
    for suffix in ['tile.bad?'+urlencode(dict(edge='crop',image=uri)),'tile.png?'+urlencode(dict(edge='crop',image='https://unconnected.invalid/image.jp2'))]:
        try:request('/streamer/v1/0/0/0/'+suffix)
        except HTTPError as error:assert error.code==400
        else:raise AssertionError('Invalid compatibility input was accepted')
    spec=json.loads(request('/openapi.json')[0]);assert spec['paths']['/streamer/v1/{z}/{x}/{y}/tile.{ext}']['get']['tags']==['Compatibility']
    drivers=json.loads(request('/api/drivers')[0])
    for driver in drivers:
        if driver['id']!='nvjpeg2000':continue
        try:
            gpu,_=request('/streamer/v1/0/0/0/tile.png?'+urlencode(dict(edge='crop',image=uri,driver='nvjpeg2000')))
            assert driver['available']
            assert Image.open(io.BytesIO(gpu)).convert('RGBA').tobytes()==Image.open(io.BytesIO(cold)).convert('RGBA').tobytes()
        except HTTPError as error:assert error.code==503 and not driver['available']
    full_drivers=[]
    expected=Image.frombytes('RGB',(512,512),pixels).convert('RGBA').tobytes()
    for driver in drivers:
        if not driver['available']:continue
        full,full_headers=request('/streamer/v1/5/0/0/tile.png?'+urlencode(dict(edge='crop',image=uri,driver=driver['id'])))
        full_image=Image.open(io.BytesIO(full)).convert('RGBA')
        assert full_image.size==(512,512) and full_image.tobytes()==expected,(driver['id'],'full-resolution S3 pixels')
        full_drivers.append(driver['id'])
    # Same tile path, different query value: neither proxy nor River may reuse the first object's pixels.
    other_url=endpoint+'/fixture/'+quote(other_key,safe='/')+'?X-Amz-Signature=second&token=x%2By'
    second,_=request('/streamer/v1/5/0/0/tile.png?'+urlencode(dict(edge='crop',image=other_url,driver='openjpeg')))
    assert Image.open(io.BytesIO(second)).convert('RGB').tobytes()==other_pixels
    original_again,_=request('/streamer/v1/5/0/0/tile.png?'+urlencode(dict(edge='crop',image=uri,driver='openjpeg')))
    assert Image.open(io.BytesIO(original_again)).convert('RGB').tobytes()==pixels
    result=dict(successEvidence=dict(url=a.api+path,status=200,headers=dict(headers)),failureEvidence=failure_evidence,specialCharacterKey=key,presignedQueryPreservedAsSingleValue=True,fullResolutionDrivers=full_drivers,lazyS3Uri=True,httpObjectUrl=True,byteIdenticalToWmts=True,repeatSourceRequests=0,driversVerified=drivers,defaultDriver='openjpeg',swaggerTag='Compatibility',pixelSize=Image.open(io.BytesIO(cold)).size,passed=True)
    if a.output:
        from pathlib import Path
        Path(a.output).write_text(json.dumps(result,indent=2))
    print(json.dumps(result))
finally:
    if created:request('/api/s3/sources/'+source,method='DELETE')
    server.shutdown()
