"""Regression: a valid JP2 comment may contain a decodable fake SOT."""
import argparse,hashlib,importlib.util,io,json,pathlib,threading,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.parse import urlencode
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--fixture',type=pathlib.Path,required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();assert not a.output.exists();data=a.fixture.read_bytes();etag='"'+hashlib.sha256(data).hexdigest()+'"';calls=[]
spec=importlib.util.spec_from_file_location('h',pathlib.Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
class Origin(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_GET(self):
  lo,hi=map(int,self.headers['Range'][6:].split('-'));hi=min(hi,len(data)-1);calls.append(dict(start=lo,endInclusive=hi,ifMatch=self.headers.get('If-Match')));assert self.headers.get('If-Match') in [None,etag]
  self.send_response(206);self.send_header('ETag',etag);self.send_header('Last-Modified','Thu, 15 Oct 2026 00:00:00 GMT');self.send_header('Content-Range',f'bytes {lo}-{hi}/{len(data)}');self.send_header('Content-Length',str(hi-lo+1));self.end_headers();self.wfile.write(data[lo:hi+1])
server=ThreadingHTTPServer(('0.0.0.0',0),Origin);threading.Thread(target=server.serve_forever,daemon=True).start();endpoint=f'http://host.docker.internal:{server.server_port}';sid='embedded-'+uuid.uuid4().hex;rows=[];report=dict(passed=False,fixtureSha256=hashlib.sha256(data).hexdigest(),scope='Adversarial correctness regression, not a latency benchmark. Generated two-tile fixture; original three-size source files untouched.',rows=rows)
try:
 h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST');assert not calls
 for driver in ['openjpeg','jp2grok','openhtj2k']:
  q=dict(image=endpoint+'/fixture/embedded.jp2',driver=driver,edge='crop',tileSize=64,minZoom=0,maxZoom=6,tilePixelRatio=1)
  url=a.api+'/streamer/v1/6/1/0/tile.png?'+urlencode(q);body=h.http(url);actual=Image.open(io.BytesIO(body)).convert('RGB');expected=Image.new('RGB',(64,64),(90,20,10));assert actual.size==expected.size and actual.tobytes()==expected.tobytes(),driver
  before=len(calls);assert h.http(url)==body and len(calls)==before;rows.append(dict(driver=driver,realTilePixelsExact=True,fakeTileNotUsed=True,repeatSourceIoZero=True))
 report.update(passed=True,originCalls=calls);print(json.dumps(report))
finally:
 h.http(a.api+'/api/s3/sources/'+sid,method='DELETE');server.shutdown();a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
