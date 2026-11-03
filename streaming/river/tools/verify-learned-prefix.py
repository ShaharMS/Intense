"""Actual source-cap/strict-range-cap proof for learned multipart coalescing."""
import argparse,hashlib,importlib.util,io,json,subprocess,uuid
from pathlib import Path
from urllib.parse import urlencode
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--container',required=True)
p.add_argument('--origin',required=True);p.add_argument('--origin-control',required=True)
p.add_argument('--workload',type=Path,required=True);p.add_argument('--output',type=Path,required=True)
p.add_argument('--require-zero-cache',action='store_true');p.add_argument('--allow-no-pressure',action='store_true')
a=p.parse_args();assert not a.output.exists() and a.container.startswith('river-goal-')
cfg=json.loads(subprocess.check_output(['docker','inspect',a.container]))[0]
assert 'RIVER_S3_REQUEST_RANGES=64' in cfg['Config']['Env']
if a.require_zero_cache:assert 'RIVER_S3_RANGE_CACHE_BYTES=0' in cfg['Config']['Env']
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'))
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
w=json.loads(a.workload.read_text());reference=Image.open(a.workload.parent/w['referenceFile']).convert('RGB')
assert hashlib.sha256((a.workload.parent/w['referenceFile']).read_bytes()).hexdigest()==w['case']['referenceSha256']
saved=json.loads(h.http(a.api+'/api/settings/read-policy'))['settings']
h.http(a.api+'/api/settings/read-policy',dict(saved,mode='adaptive',minimumSamples=8,maxOverfetchRatio=4,maxSourceRequests=2),'PUT')
report=dict(passed=False,imageId=cfg['Image'],explicitContextRequestCap=64,sourceConcurrencyCap=2,zeroCacheRequired=a.require_zero_cache,origin=json.loads(h.http(a.origin_control+'/__manifest')),rows=[])
def url(endpoint,name,driver):
 c=w['case'];return a.api+f"/streamer/v1/{c['z']}/0/0/tile.png?"+urlencode(dict(image=endpoint+'/fixture/'+name+'.jp2',driver=driver,edge='crop',tileSize=c['tileSize'],maxZoom=c['maxZoom'],minZoom=0,tilePixelRatio=1))
def check(body):
 actual=Image.open(io.BytesIO(body)).convert('RGB');assert actual.size==reference.size and actual.tobytes()==reference.tobytes()
try:
 for driver in ['openjpeg','jp2grok','openhtj2k']:
  sid='prefix-cap-'+uuid.uuid4().hex;endpoint=a.origin+'/'+sid
  h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
  try:
   check(h.http(url(endpoint,w['warmup']['name'],'openjpeg')))
   before=json.loads(h.http(a.origin_control+'/__stats/'+sid))
   target=url(endpoint,w['case']['name'],driver);body,headers=h.http(target,include_headers=True);check(body)
   after=json.loads(h.http(a.origin_control+'/__stats/'+sid));gets=after['wire']['gets']-before['wire']['gets']
   assert 0<gets<=64
   if 'pressure' in after:assert after['pressure']['peak']<=2 and after['pressure']['active']==0
   else:assert a.allow_no_pressure,'Independent pressure counters required'
   assert 'multipart-prefix-ranges' in headers['x-river-decoder-route'],'Strict cap incorrectly forced compatibility fallback'
   obs=json.loads(h.http(a.api+'/api/settings/read-policy'))['sources'][sid]
   assert obs['lastDecision']['applied'];assert obs['lastDecision']['plannedRequests']<=64
   assert h.http(target)==body
   assert json.loads(h.http(a.origin_control+'/__stats/'+sid))==after,'Repeat performed source IO'
   report['rows'].append(dict(driver=driver,pixelsExact=True,repeatIoZero=True,targetGets=gets,sourcePeak=after.get('pressure',{}).get('peak'),route=headers['x-river-decoder-route'],decision=obs['lastDecision']))
  finally:h.http(a.api+'/api/s3/sources/'+sid,method='DELETE')
 report['passed']=True
finally:
 h.http(a.api+'/api/settings/read-policy',saved,'PUT');a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps(report,indent=2))
