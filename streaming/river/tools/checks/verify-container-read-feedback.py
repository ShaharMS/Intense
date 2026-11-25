"""Check real origin accounting and adaptive planning on an already deployed image."""
import argparse,io,json,time,uuid,urllib.request,urllib.parse
from pathlib import Path
from PIL import Image
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--api',default='http://127.0.0.1:18080');a=p.parse_args()
def call(path,body=None,method=None):
 req=urllib.request.Request(a.api+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'})
 with urllib.request.urlopen(req,timeout=60) as r:return r.read()
old=json.loads(call('/api/settings/read-policy'))['settings'];sid='feedback-'+uuid.uuid4().hex;rows=[]
try:
 call('/api/settings/read-policy',dict(mode='adaptive',minimumSamples=16,maxOverfetchRatio=2),'PUT')
 call('/api/s3/sources',dict(id=sid,name='Measured feedback fixture',bucket='fixture',region='us-east-1',endpoint='http://host.containers.internal:18992/'+sid,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
 with Image.open(ROOT/'corpus/synthetic-texture-tlm-plt-d5.jp2') as im:im.reduce=5;expected=im.convert('RGB').tobytes()
 for i in range(40):
  locator='s3-'+json.dumps([sid,f'feedback-{i}.jp2'],separators=(',',':')).encode().hex()
  start=time.perf_counter();data=call('/streamer/v1/0/0/0/tile.png?'+urllib.parse.urlencode(dict(image=locator,driver='openjpeg',tileSize=128,maxZoom=5)))
  with Image.open(io.BytesIO(data)) as im:assert im.convert('RGB').tobytes()==expected
  ms=(time.perf_counter()-start)*1000;source=json.loads(call('/api/settings/read-policy'))['sources'][sid]
  with urllib.request.urlopen('http://127.0.0.1:18992/__stats/'+sid) as r:wire=json.load(r)['wire']
  assert sum(source[k]['measuredOriginBytes']+source[k]['unmeasuredOriginBytes'] for k in ['actualOverfetch','metadataOverfetch'])==wire['bytes']
  feedback=source['actualOverfetch'];assert 1<=feedback['effectiveOptionalReadRatio']<=2
  if feedback['measuredSamples']<16:assert feedback['adjustmentCount']==0
  rows.append(dict(iteration=i,ms=ms,wire=wire,source=source))
  print(json.dumps(dict(iteration=i,ms=ms,ratio=feedback.get('measuredRatio'),optionalRatio=feedback['effectiveOptionalReadRatio'])),flush=True)
 assert feedback['adjustmentCount']>0
 (ROOT/'target/tests/container-read-feedback.json').write_text(json.dumps(dict(passed=True,exactPixels=True,originAccountingExact=True,configuredWarmupRespected=True,rows=rows),indent=2)+'\n')
finally:
 call('/api/settings/read-policy',old,'PUT');call('/api/s3/sources/'+sid,method='DELETE')
