"""Original aerial JP2 preview correctness over an instrumented read-only origin.
References are independently decoded from original files. No source rewrites.
"""
import argparse,hashlib,importlib.util,io,json,socket,threading,uuid
from pathlib import Path
from http.server import ThreadingHTTPServer
from urllib.parse import urlencode
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--inventory',type=Path,required=True)
p.add_argument('--references',type=Path,required=True);p.add_argument('--output',type=Path,required=True)
p.add_argument('--origin-host',default='127.0.0.1');p.add_argument('--driver',default='openjpeg');a=p.parse_args();assert not a.output.exists()
root=a.inventory.parent.resolve();assets=json.loads(a.inventory.read_text());refs={r['file']:r for r in json.loads((a.references/'manifest.json').read_text())}
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
files={};cases=[]
for n,row in enumerate(assets):
 f=(root/row['file']).resolve();assert f.parent==root and f.stat().st_size==row['bytes']
 with f.open('rb') as s:assert hashlib.file_digest(s,'sha256').hexdigest()==row['sha256']
 name='aerial-'+str(n);files[name]=f;cases.append(dict(name=name,sha256=row['sha256']))
counters={};lock=threading.Lock();base=h.make_origin(files,cases,dict(cases=cases),counters,lock)
class Origin(base):
 def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
server=ThreadingHTTPServer(('0.0.0.0',18997),Origin);threading.Thread(target=server.serve_forever,daemon=True).start()
report=dict(passed=False,scope='Original aerial files, independently decoded reference, one diagnostic preview each. Not a paired performance benchmark.',rows=[])
try:
 for case,asset in zip(cases,assets):
  sid='aerial-check-'+uuid.uuid4().hex;endpoint='http://'+a.origin_host+':18997/'+sid
  h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
  try:
   ref=refs[asset['file']];file=a.references/ref['reference'];assert hashlib.sha256(file.read_bytes()).hexdigest()==ref['referenceSha256']
   url=a.api+f"/streamer/v1/{12-ref['reduction']}/0/0/tile.png?"+urlencode(dict(image=endpoint+'/fixture/'+case['name']+'.jp2',driver=a.driver,edge='crop',tileSize=4096,maxZoom=12,minZoom=0,tilePixelRatio=1))
   body,headers=h.http(url,include_headers=True);actual=Image.open(io.BytesIO(body)).convert('RGB');gold=Image.open(file).convert('RGB')
   assert actual.size==gold.size and actual.tobytes()==gold.tobytes(),asset['file']
   before=dict(counters[sid]);assert h.http(url)==body and counters[sid]==before
   report['rows'].append(dict(file=asset['file'],sourceSha256=asset['sha256'],driver=a.driver,dimensions=actual.size,pixelsExact=True,repeatIoZero=True,wire=before,route=headers['x-river-decoder-route']))
   print(json.dumps(report['rows'][-1]),flush=True)
  finally:h.http(a.api+'/api/s3/sources/'+sid,method='DELETE')
 report['passed']=True
finally:
 server.shutdown();a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
