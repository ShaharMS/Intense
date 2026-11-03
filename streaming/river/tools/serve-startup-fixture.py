"""Temporary delayed S3 listing for end-to-end viewer startup verification."""
import argparse,json,threading,time,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.request import Request,urlopen
from urllib.parse import parse_qs,urlparse
p=argparse.ArgumentParser();p.add_argument('--apis',nargs='+',required=True);p.add_argument('--port',type=int,default=18557)
p.add_argument('--delay-ms',type=int,default=2000);a=p.parse_args();source='startup-'+uuid.uuid4().hex[:12]
class S3(BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_GET(self):
        if 'list-type' not in parse_qs(urlparse(self.path).query):self.send_error(404);return
        time.sleep(a.delay_ms/1000)
        body=b'<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Contents><Key>folder/search-result.jp2</Key><LastModified>2026-10-15T00:00:00Z</LastModified><ETag>"fixture"</ETag><Size>12345</Size></Contents><IsTruncated>false</IsTruncated></ListBucketResult>'
        self.send_response(200);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Type','application/xml');self.end_headers();self.wfile.write(body)
    def do_POST(self):
        if self.path!='/stop':self.send_error(404);return
        self.send_response(200);self.send_header('Content-Length','0');self.end_headers();threading.Thread(target=server.shutdown,daemon=True).start()
server=ThreadingHTTPServer(('0.0.0.0',a.port),S3);registered=[]
try:
    for api in a.apis:
        value=dict(id=source,name='Delayed startup fixture',endpoint=f'http://host.docker.internal:{a.port}',bucket='fixture',region='us-east-1',prefix='folder',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True,virtualHostedStyle=False)
        urlopen(Request(api+'/api/s3/sources',data=json.dumps(value).encode(),headers={'Authorization':'Bearer fixture-admin','Content-Type':'application/json'})).close();registered.append(api)
    timer=threading.Timer(180,server.shutdown);timer.daemon=True;timer.start()
    print(json.dumps(dict(ready=True,sourceId=source,delayMs=a.delay_ms,stopUrl=f'http://127.0.0.1:{a.port}/stop')),flush=True)
    server.serve_forever()
finally:
    for api in registered:
        urlopen(Request(api+'/api/s3/sources/'+source,method='DELETE',headers={'Authorization':'Bearer fixture-admin'})).close()
    server.server_close()
