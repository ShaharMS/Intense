"""Compare real deployed decoder pixels with independently authored source samples."""
import argparse,hashlib,io,json,time,urllib.request,urllib.parse,urllib.error
from pathlib import Path
from PIL import Image
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--port',type=int,required=True);p.add_argument('--label',required=True);a=p.parse_args()
base=f'http://127.0.0.1:{a.port}';rows=[]
def request(path,body=None):
 req=urllib.request.Request(base+path,data=json.dumps(body).encode() if body else None,headers={'Content-Type':'application/json'})
 with urllib.request.urlopen(req,timeout=90) as r:return r.read(),dict(r.headers)
sid='edge-'+a.label
request('/api/s3/sources',dict(id=sid,name='Synthetic edge fixtures',bucket='fixture',region='us-east-1',endpoint='http://host.containers.internal:18992/'+sid,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True,virtualHostedStyle=False))
for fixture in json.loads((ROOT/'target/fixtures/edge-corpus.json').read_text())['fixtures']:
 for driver in ['openjpeg','jp2grok','openhtj2k','auto']:
  start=time.perf_counter()
  try:
   data,headers=request('/streamer/v1/1/0/0/tile.png?'+urllib.parse.urlencode(dict(image='s3-'+json.dumps([sid,fixture['file']],separators=(',',':')).encode().hex(),driver=driver)))
   elapsed=(time.perf_counter()-start)*1000
   with Image.open(io.BytesIO(data)) as im:actual=im.convert('RGBA');pixels=actual.tobytes();size=actual.size
   with Image.open(ROOT/'corpus'/fixture['source']) as im:expected=im.convert('RGBA').crop((0,0,256,256)).tobytes()
   mismatches=sum(x!=y for x,y in zip(pixels,expected));maxdelta=max(abs(x-y) for x,y in zip(pixels,expected))
   row=dict(file=fixture['file'],requestedDriver=driver,ms=elapsed,size=size,exact=pixels==expected,differingChannels=mismatches,maxChannelDifference=maxdelta,pixelSha256=hashlib.sha256(pixels).hexdigest(),headers={k:v for k,v in headers.items() if k.lower().startswith('x-river')})
  except urllib.error.HTTPError as e:row=dict(file=fixture['file'],requestedDriver=driver,status=e.code,error=e.read().decode())
  rows.append(row);print(json.dumps(row),flush=True)
(ROOT/'target/benchmarks'/('edge-'+a.label+'.json')).write_text(json.dumps(dict(label=a.label,rows=rows),indent=2)+'\n')
