"""Validate real encoded output reuse across a backend restart and endpoint reordering."""
import io,json,time,subprocess,urllib.request,urllib.parse,uuid
from pathlib import Path
from PIL import Image
ROOT=Path(__file__).resolve().parents[2];BASE='http://127.0.0.1:18080';sid='shared-'+uuid.uuid4().hex
settings=dict(namespace=sid,timeoutMs=100,maxFillsPerSecond=512,endpoints=[dict(id='redis',url='redis://replica-3:6379'),dict(id='valkey',url='valkey://replica-4:6379')])
def call(path,body=None,method=None):
 req=urllib.request.Request(BASE+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'})
 with urllib.request.urlopen(req,timeout=30) as r:return r.read(),dict(r.headers)
def setup():
 call('/api/settings/shared-cache',dict(settings=settings),'PUT')
 call('/api/s3/sources',dict(id=sid,name='Shared restart fixture',bucket='fixture',region='us-east-1',endpoint='http://host.containers.internal:18992/'+sid,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
setup();file='synthetic-texture-tlm-plt-d5.jp2';locator='s3-'+json.dumps([sid,file],separators=(',',':')).encode().hex();rows=[]
with Image.open(ROOT/'corpus/synthetic-texture-reference.png') as im:expected=im.convert('RGBA').crop((0,0,256,256)).tobytes()
path='/streamer/v1/4/0/0/tile.png?'+urllib.parse.urlencode(dict(image=locator,driver='openjpeg'))
for passno in range(2):
 data,headers=call(path)
 with Image.open(io.BytesIO(data)) as im:assert im.convert('RGBA').tobytes()==expected
 time.sleep(.2);diagnostics=json.loads(call('/api/settings/shared-cache')[0]);rows.append(dict(passno=passno,headers=headers,diagnostics=diagnostics))
 if passno==0:
  assert diagnostics['fills']>0
  subprocess.run(['podman','restart','replica-1'],check=True,stdout=subprocess.DEVNULL)
  for _ in range(100):
   try:call('/health');break
   except OSError:time.sleep(.1)
  settings['endpoints'].reverse();setup()
 else:assert diagnostics['tileHits']>0,rows
call('/api/settings/shared-cache',dict(settings=None),'PUT');call('/api/s3/sources/'+sid,method='DELETE')
(ROOT/'target/tests/container-shared-cache.json').write_text(json.dumps(dict(passed=True,restartReuse=True,exactPixels=True,endpointReordering=True,rows=rows),indent=2)+'\n');print('Shared encoded tile reused after backend restart and endpoint reordering; exact pixels')
