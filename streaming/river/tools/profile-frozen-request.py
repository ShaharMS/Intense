"""One request-correlated local lab trace; never a paired timing claim."""
import argparse,hashlib,io,json,subprocess,time,urllib.request,urllib.parse,uuid
from pathlib import Path
from PIL import Image
p=argparse.ArgumentParser()
for key in ['container','api','origin-container','origin-control','case','output']:p.add_argument('--'+key,required=True)
p.add_argument('--manifest',type=Path,default=Path(__file__).resolve().parents[1]/'reports/fixed-gate-v2/manifest.json')
a=p.parse_args();assert a.container.startswith('river-goal-') and a.origin_container.startswith('river-goal-')
assert urllib.parse.urlsplit(a.api).hostname in ['127.0.0.1','localhost']
out=Path(a.output);assert not out.exists();m=json.loads(a.manifest.read_text());case=next(c for c in m['cases'] if c['name']==a.case)
gold_path=a.manifest.parent/case['reference'];assert hashlib.sha256(gold_path.read_bytes()).hexdigest()==case['referenceSha256']
def http(url,data=None,method=None):
 with urllib.request.urlopen(urllib.request.Request(url,data=None if data is None else json.dumps(data).encode(),headers={'Content-Type':'application/json'},method=method),timeout=40) as r:return r.read(),dict(r.headers)
def js(url,*args,**kwargs):return json.loads(http(url,*args,**kwargs)[0])
subprocess.run(['docker','restart',a.container],check=True,stdout=subprocess.DEVNULL)
deadline=time.monotonic()+30
while True:
 try:http(a.api+'/health');break
 except OSError:
  if time.monotonic()>deadline:raise
  time.sleep(.1)
ip=subprocess.check_output(['docker','inspect',a.origin_container,'--format','{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}'],text=True).strip()
sid='profile-'+uuid.uuid4().hex;endpoint='http://'+ip+':8000/'+sid;stats_url=a.origin_control+'/__stats/'+sid
report=dict(passed=False,scope='One instrumented process/source-cold lab request; no comparative latency claim. Origin OS cache warm.',case=case,manifestSha256=hashlib.sha256(a.manifest.read_bytes()).hexdigest(),sourceId=sid,
 image=json.loads(subprocess.check_output(['docker','inspect',a.container]))[0]['Image'])
try:
 js(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
 js(a.api+'/api/diagnostics/source-reads',dict(enabled=True),'PUT');js(a.api+'/api/diagnostics/source-reads',method='DELETE')
 assert not any(js(stats_url)['wire'].values())
 q=dict(image=endpoint+'/fixture/'+case['name']+'.jp2',driver='openjpeg',edge='crop',tileSize=case['tileSize'],minZoom=0,maxZoom=case['maxZoom'],tilePixelRatio=1)
 url=a.api+f'/streamer/v1/{case["z"]}/{case["x"]}/{case["y"]}/tile.png?'+urllib.parse.urlencode(q)
 body,headers=http(url);image=Image.open(io.BytesIO(body)).convert('RGB');gold=Image.open(gold_path).convert('RGB')
 assert image.size==gold.size and image.tobytes()==gold.tobytes()
 origin=js(stats_url);trace=js(a.api+'/api/diagnostics/source-reads');policy=js(a.api+'/api/settings/read-policy')
 assert not trace['summary']['partial'] and trace['summary']['physicalAttempts']==origin['wire']['gets']
 repeated,_=http(url);assert repeated==body and js(stats_url)['wire']==origin['wire']
 report.update(passed=True,pixelsExact=True,repeatIoZero=True,headers=headers,origin=origin,sourceTrace=trace,settings=policy['settings'],observations=policy['sources'][sid])
finally:
 out.parent.mkdir(parents=True,exist_ok=True);out.write_text(json.dumps(report,indent=2))
 js(a.api+'/api/diagnostics/source-reads',dict(enabled=False),'PUT');http(a.api+'/api/s3/sources/'+sid,method='DELETE')
print(json.dumps(dict(passed=report['passed'],wire=report['origin']['wire'],groups=report['sourceTrace']['summary']['groups'])))
