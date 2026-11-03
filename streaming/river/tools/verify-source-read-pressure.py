"""Verify persisted source admission against a delayed origin's independent counters."""
import argparse,concurrent.futures,hashlib,importlib.util,io,json,subprocess,time,uuid
from pathlib import Path
from urllib.parse import urlencode
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--container',required=True);p.add_argument('--origin',required=True);p.add_argument('--origin-control',required=True);p.add_argument('--manifest',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();assert a.container.startswith('river-goal-') and not a.output.exists()
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
manifest=json.loads(a.manifest.read_bytes());case=next(c for c in manifest['cases'] if c['name']=='mb50-deep')
policy=json.loads(h.http(a.api+'/api/settings/read-policy'))['settings'];saved=dict(policy,maxSourceRequests=2);h.http(a.api+'/api/settings/read-policy',saved,'PUT')
subprocess.run(['docker','restart',a.container],check=True,stdout=subprocess.DEVNULL)
for _ in range(100):
 try:h.http(a.api+'/api/drivers');break
 except OSError:time.sleep(.1)
assert json.loads(h.http(a.api+'/api/settings/read-policy'))['settings']['maxSourceRequests']==2
assert json.loads(h.http(a.api+'/api/settings/configuration'))['readPolicy']['maxSourceRequests']==2
sid='pressure-'+uuid.uuid4().hex;endpoint=a.origin+'/'+sid
report=dict(passed=False,limit=2,persistedAcrossRestart=True,scope='Two different cold object identities on one connection, each with batched deep-tile payloads. Origin injects20ms delay per GET; independent peak measured there.')
try:
 h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
 def request(name):
  q=dict(image=endpoint+'/fixture/'+name+'.jp2',driver='openjpeg',edge='crop',tileSize=case['tileSize'],minZoom=0,maxZoom=case['maxZoom'],tilePixelRatio=1)
  return a.api+f"/streamer/v1/{case['z']}/{case['x']}/{case['y']}/tile.png?"+urlencode(q)
 urls=[request('mb50-preview'),request('mb50-deep')]
 with concurrent.futures.ThreadPoolExecutor(2) as pool:bodies=list(pool.map(h.http,urls))
 ref=Image.open(a.manifest.parent/case['reference']).convert('RGB')
 for body in bodies:
  image=Image.open(io.BytesIO(body)).convert('RGB');assert image.size==ref.size and image.tobytes()==ref.tobytes()
 wire=json.loads(h.http(a.origin_control+'/__stats/'+sid));assert wire['pressure']['peak']==2 and wire['pressure']['active']==0,wire['pressure']
 for url,body in zip(urls,bodies):assert h.http(url)==body
 assert json.loads(h.http(a.origin_control+'/__stats/'+sid))==wire
 observations=json.loads(h.http(a.api+'/api/settings/read-policy'))['sources'][sid];assert observations['activeReads']==0 and observations['admissionWaits']>0,observations
 report.update(passed=True,pixelsExact=True,repeatSourceIoZero=True,source=wire,observations=observations)
finally:
 h.http(a.api+'/api/s3/sources/'+sid,method='DELETE');h.http(a.api+'/api/settings/read-policy',policy,'PUT');a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps({k:v for k,v in report.items() if k!='source'}))
