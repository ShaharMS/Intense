"""Check live WebP changes, encoded cache separation and restart persistence."""
import argparse,json,subprocess,threading,time
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.request import Request,urlopen
from urllib.parse import urlencode
from urllib.error import HTTPError
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--driver',default='jp2grok');p.add_argument('--container',required=True);a=p.parse_args()
assert a.container.startswith('river-goal-')
path=Path(__file__).resolve().parents[1]/'corpus/cache/Bretagne2.j2k';size=path.stat().st_size
class S3(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def log_message(self,*args):pass
    def do_HEAD(self):
        self.send_response(200);self.send_header('Content-Length',str(size));self.send_header('ETag','"encoding-test"');self.end_headers()
    def do_GET(self):
        start,end=map(int,self.headers['Range'][6:].split('-'))
        with path.open('rb') as f:f.seek(start);body=f.read(end-start+1)
        self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {start}-{end}/{size}');self.send_header('ETag','"encoding-test"');self.end_headers();self.wfile.write(body)
server=ThreadingHTTPServer(('127.0.0.1',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(path,body=None,method=None,auth=True):
    headers={'Content-Type':'application/json'}
    if auth:headers['Authorization']='Bearer fixture-admin'
    return urlopen(Request(a.api+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers=headers),timeout=30)
def json_request(path,body=None,method=None):
    with request(path,body,method) as response:return json.load(response)
def restart():
    subprocess.run(['docker','restart',a.container],capture_output=True,check=True)
    deadline=time.monotonic()+30
    while True:
        try:
            with request('/health'):return
        except OSError:
            if time.monotonic()>deadline:raise
            time.sleep(.1)
try:
    for body,auth in [({'webpMethod':7},False),({'webpMethod':-1},True),({'webpMethod':4,'unexpected':1},True)]:
        try:
            request('/api/settings/encoding',body,'PUT',auth).close();raise AssertionError('invalid/unauthorized update accepted')
        except HTTPError as error:assert error.code in (400,401,403,422),error.code
    json_request('/api/s3/sources',{'id':'encoding-test','name':'encoding-test','endpoint':f'http://host.docker.internal:{server.server_port}','allowHttp':True,'bucket':'fixture','accessKeyId':'fixture','secretAccessKey':'fixture'})
    asset=json_request('/api/s3/sources/encoding-test/assets',{'key':'test.j2k'})['assetId'];meta=json_request('/api/assets/'+asset+'/manifest')
    query=urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/webp',DRIVER=a.driver,LAYERS=0,
        TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=meta['resolutionLevels']-1,TILEROW=0,TILECOL=0))
    payloads={}
    for method in (0,4,0):
        assert json_request('/api/settings/encoding',{'webpMethod':method},'PUT')['webpMethod']==method
        with request('/wmts?'+query) as response:body=response.read()
        if method in payloads:assert body==payloads[method],'method-specific cached result changed'
        payloads[method]=body
    assert payloads[0]!=payloads[4],'method change returned old encoded cache entry'
    for method in (4,0):
        json_request('/api/settings/encoding',{'webpMethod':method},'PUT');restart()
        assert json_request('/api/settings/encoding')['webpMethod']==method
        assert json_request('/api/s3/sources/encoding-test/assets',{'key':'test.j2k'})['assetId']==asset
        with request('/wmts?'+query) as response:assert response.read()==payloads[method],'disk cache mixed encoding methods'
    print('PASS: API validation without authentication, live method changes, separate memory/disk cache entries, persisted settings and S3 source after restart')
finally:
    with request('/api/s3/sources/encoding-test',method='DELETE'):pass
    server.shutdown()
