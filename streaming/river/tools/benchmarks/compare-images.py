"""Matched m1227/current container comparison; source/object caches cold per flow.
Both fixed Compose containers have the same CPU quota and memory limit.
"""
import argparse,hashlib,io,json,time,urllib.request,urllib.parse,urllib.error,uuid,statistics
from pathlib import Path
from PIL import Image
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--pairs',type=int,default=3);p.add_argument('--limit',type=int,default=15);p.add_argument('--before-port',type=int,default=18081);p.add_argument('--current-port',type=int,default=18080);p.add_argument('--image');p.add_argument('--output',type=Path,default=ROOT/'target/benchmarks/container-comparison.json');a=p.parse_args()
fixtures=[f for f in json.loads((ROOT/'target/fixtures/corpus.json').read_text())['fixtures'] if not a.image or f['file']==a.image][:a.limit];rows=[]
out=a.output
def request(url,body=None):
 req=urllib.request.Request(url,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json'})
 with urllib.request.urlopen(req,timeout=120) as r:return r.read(),dict(r.headers)
def stats(sid):return json.loads(request('http://127.0.0.1:18992/__stats/'+sid)[0])
def run(f,version,pair,flow):
 base='http://127.0.0.1:'+str(a.before_port if version=='m1227' else a.current_port)
 sid='measure-'+uuid.uuid4().hex
 request(base+'/api/s3/sources',dict(id=sid,name='Controlled synthetic fixture',bucket='fixture',region='us-east-1',endpoint='http://host.containers.internal:18992/'+sid,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True,virtualHostedStyle=False))
 image='s3-'+json.dumps([sid,f['file']],separators=(',',':')).encode().hex()
 with Image.open(ROOT/'corpus'/f['source']) as ref:expected=ref.convert('RGB').crop((0,0,256,256)).tobytes()
 try:
  for phase,z in ([('cold_zoom',4)] if flow=='cold' else [('cold_preview',0),('zoom_after_preview',4),('repeated_zoom',4)]):
   before=stats(sid);start=time.perf_counter();data,headers=request(base+f'/streamer/v1/{z}/0/0/tile.png?'+urllib.parse.urlencode(dict(image=image,driver='openjpeg')));elapsed=(time.perf_counter()-start)*1000;after=stats(sid)
   with Image.open(io.BytesIO(data)) as im:pixels=im.convert('RGB').tobytes()
   exact=pixels==expected if z==4 else None
   if exact is False:raise RuntimeError('Independent pixel mismatch: '+f['file'])
   row=dict(image=f['file'],version=version,pair=pair,phase=phase,ms=elapsed,wire={k:after['wire'][k]-before['wire'][k] for k in after['wire']},originRanges=after['ranges'][len(before['ranges']):],referencePixelsExact=exact,pixelSha256=hashlib.sha256(pixels).hexdigest(),headers={k:v for k,v in headers.items() if k.lower().startswith('x-river')})
   rows.append(row);out.write_text(json.dumps(dict(complete=False,rows=rows),indent=2));print(json.dumps({k:row[k] for k in ['image','version','pair','phase','ms','wire','referencePixelsExact']}),flush=True)
 finally:
  req=urllib.request.Request(base+'/api/s3/sources/'+sid,method='DELETE');urllib.request.urlopen(req).close()
for f in fixtures:
 for pair in range(a.pairs):
  for version in (['m1227','current'] if pair%2==0 else ['current','m1227']):
   for flow in ['cold','reuse']:run(f,version,pair,flow)
for f in fixtures:
 if len({r['pixelSha256'] for r in rows if r['image']==f['file'] and r['phase']=='cold_preview'})!=1:raise RuntimeError('Preview pixel regression: '+f['file'])
out.write_text(json.dumps(dict(complete=True,cpuQuota=4,memoryLimitBytes=2<<30,sourceDiskCold=False,coldScope='new source identity and object; process and native workers already running',driver='openjpeg',originDelayMs=7,originBytesPerSecond=300000000,rows=rows),indent=2)+'\n')
