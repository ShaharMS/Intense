"""Exercise concurrent GPU requests that switch asset, depth, and reduction."""
import concurrent.futures,io,json,urllib.parse,urllib.request
import numpy as np
from PIL import Image
api='http://river:8080'
assets=json.load(urllib.request.urlopen(api+'/api/assets'))
ids=['94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290',assets[0]['assetId']]
jobs=[]
for asset in ids:
 m=json.load(urllib.request.urlopen(api+'/api/assets/'+asset+'/manifest'))
 for r in [0,2,m['resolutionLevels']-1]:
  for col in [0,(m['width']-1)//(512*2**r)]:
   q=dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/png',DRIVER='openjpeg',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=m['resolutionLevels']-1-r,TILEROW=0,TILECOL=col)
   with urllib.request.urlopen(api+'/wmts?'+urllib.parse.urlencode(q),timeout=120) as response:ref=np.asarray(Image.open(io.BytesIO(response.read())).convert('RGBA'))
   q['DRIVER']='nvjpeg2000'
   jobs.append((q,ref))
def run(i):
 q,ref=jobs[(i*7)%len(jobs)]
 s=dict(index=i,asset=q['LAYER'],matrix=q['TILEMATRIX'],col=q['TILECOL'])
 try:
  with urllib.request.urlopen(api+'/wmts?'+urllib.parse.urlencode(q),timeout=120) as response:
   image=Image.open(io.BytesIO(response.read())).convert('RGBA');image.load();s['traceId']=response.headers.get('x-river-trace-id')
   s['ok']=bool(np.array_equal(np.asarray(image),ref))
 except Exception as e:s['ok']=False;s['error']=str(e)
 return s
with concurrent.futures.ThreadPoolExecutor(max_workers=32) as pool:results=list(pool.map(run,range(96)))
print(json.dumps(dict(concurrency=32,samples=results,failed=sum(not s['ok'] for s in results)),indent=2))
if any(not s['ok'] for s in results):raise SystemExit(1)
