"""Validate measured origin amplification and bounded per-source feedback."""
import argparse,json,os,subprocess,time,urllib.request,urllib.parse,uuid,io
from pathlib import Path
from PIL import Image
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--count',type=int,default=40);p.add_argument('--edge',type=int,default=128);a=p.parse_args();assert 8<=a.count<=48
sid='feedback-'+uuid.uuid4().hex;base='http://127.0.0.1:18995';origin='http://127.0.0.1:18992/'+sid
settings={k:v for k,v in os.environ.items() if not k.startswith('RIVER_')};settings.update(RIVER_BIND='127.0.0.1:18995',RIVER_CONFIG_MODE='env',RIVER_AUTO_ADAPTATION_MODE='liberal,no-disk',RIVER_MEMORY_BUDGET_BYTES=str(2<<30),RIVER_DECODE_CONCURRENCY='2',RIVER_OPENJPEG_THREADS_PER_DECODE='2',RIVER_OTEL_ENABLED='false',RIVER_RESOURCE_DISPLAY='off',RIVER_LOG_LEVEL='warn',RIVER_S3_ADAPTIVE_READS='adaptive',RIVER_S3_SOURCE_COUNT='1',RIVER_S3_SOURCE_1_ID='fixture',RIVER_S3_SOURCE_1_BUCKET='fixture',RIVER_S3_SOURCE_1_REGION='us-east-1',RIVER_S3_SOURCE_1_ENDPOINT=origin,RIVER_S3_SOURCE_1_ALLOW_HTTP='true',RIVER_S3_SOURCE_1_ACCESS_KEY_ID='fixture',RIVER_S3_SOURCE_1_SECRET_ACCESS_KEY='fixture')
for key in ['MULTIPART_PLT','MULTIPART_PREFIX','SINGLE_PREFIX','INDEX_ONLY_PREFIX','STREAMED_PLT_ROI','PRECINCT_ROI','EXACT_SUPPORT']:settings['RIVER_S3_'+key]='true'
def get(url):
 with urllib.request.urlopen(url,timeout=60) as r:return r.read()
rows=[]
with (ROOT/'target/tests/read-feedback-server.log').open('w') as log:
 process=subprocess.Popen([str(ROOT/'target/benchmarks/river-current')],cwd=ROOT,env=settings,stdout=log,stderr=subprocess.STDOUT)
 try:
  for _ in range(200):
   try:get(base+'/health');break
   except OSError:time.sleep(.05)
  with Image.open(ROOT/'corpus/synthetic-texture-tlm-plt-d5.jp2') as im:im.reduce=12-(a.edge.bit_length()-1);expected=im.convert('RGB').tobytes()
  for i in range(a.count):
   start=time.perf_counter();data=get(base+'/streamer/v1/0/0/0/tile.png?'+urllib.parse.urlencode(dict(image=f's3://fixture/feedback-{i}.jp2',driver='openjpeg',tileSize=a.edge,maxZoom=12-(a.edge.bit_length()-1))));elapsed=(time.perf_counter()-start)*1000
   with Image.open(io.BytesIO(data)) as im:assert im.convert('RGB').tobytes()==expected,'Independent overview pixels differ'
   policy=json.loads(get(base+'/api/settings/read-policy'));source=policy['sources']['fixture'];wire=json.loads(get('http://127.0.0.1:18992/__stats/'+sid))['wire']
   accounted=sum(source[k]['measuredOriginBytes']+source[k]['unmeasuredOriginBytes'] for k in ['actualOverfetch','metadataOverfetch'])
   assert accounted==wire['bytes'],(accounted,wire,source)
   feedback=source['actualOverfetch'];assert 1<=feedback['effectiveOptionalReadRatio']<=2
   rows.append(dict(iteration=i,ms=elapsed,wire=wire,source=source));print(json.dumps(dict(iteration=i,ms=elapsed,wire=wire,ratio=feedback.get('measuredRatio'),cap=feedback['effectiveOptionalReadRatio'])),flush=True)
   (ROOT/'target/tests/read-feedback.json').write_text(json.dumps(dict(complete=False,rows=rows),indent=2))
  final=rows[-1]['source']['actualOverfetch'];assert final['measuredSamples']>=8
  assert final['adjustmentCount']>0 and final['effectiveOptionalReadRatio']<2,'Fixture did not exercise feedback backoff'
  (ROOT/'target/tests/read-feedback.json').write_text(json.dumps(dict(complete=True,passed=True,exactPixels=True,originByteAccountingExact=True,requests=a.count,scope='controlled no-retry fixture; aliases force cold objects on one source without downloads/listing',rows=rows),indent=2)+'\n')
 finally:process.terminate();process.wait(timeout=10)
