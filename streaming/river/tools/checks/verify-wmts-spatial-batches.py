import concurrent.futures,io,json,math,random,time,urllib.parse,urllib.request
import numpy as np
from PIL import Image
api='http://river:8080'
ids=['94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290','034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299']
jobs=[]
def fetch(q):
 with urllib.request.urlopen(api+'/wmts?'+urllib.parse.urlencode(q),timeout=120) as r:
  return np.asarray(Image.open(io.BytesIO(r.read())).convert('RGBA')),r.headers.get('x-river-trace-id')
for asset in ids:
 m=json.load(urllib.request.urlopen(api+'/api/assets/'+asset+'/manifest'))
 for reduction in [0,2,m['resolutionLevels']-1]:
  edge=512*2**reduction;cols=math.ceil(m['width']/edge);rows=math.ceil(m['height']/edge)
  coordinates=set()
  for ox,oy in [(0,0),(max(0,cols//2-2),max(0,rows//2-1))]:
   coordinates.update((min(cols-1,ox+x),min(rows-1,oy+y)) for y in range(2) for x in range(4))
  for col,row in sorted(coordinates):
   q=dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/png',DRIVER='openjpeg',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=m['resolutionLevels']-1-reduction,TILEROW=row,TILECOL=col)
   pixels,_=fetch(q);q['DRIVER']='nvjpeg2000';jobs.append((q,pixels))
random.Random(13).shuffle(jobs)
def verify(item):
 q,reference=item;pixels,trace=fetch(q)
 equal=np.array_equal(pixels,reference)
 return dict(asset=q['LAYER'],matrix=q['TILEMATRIX'],col=q['TILECOL'],row=q['TILEROW'],traceId=trace,ok=bool(equal),maxError=int(np.abs(pixels.astype(np.int16)-reference.astype(np.int16)).max()))
with concurrent.futures.ThreadPoolExecutor(max_workers=32) as pool:samples=list(pool.map(verify,jobs*2))
time.sleep(6)
batches=[];native_traces=0
for s in samples:
 data=json.load(urllib.request.urlopen('http://jaeger:16686/api/traces/'+s['traceId']))
 for span in data['data'][0]['spans']:
  if span['operationName']=='river.cuda.native.decode_tiles':native_traces+=1
  if span['operationName']=='river.sidecar.http':
   tags={t['key']:t['value'] for t in span['tags']}
   batches.append(dict(traceId=s['traceId'],requests=int(tags.get('batch_requests',0)),outputs=int(tags.get('batch_outputs',0))))
report=dict(samples=samples,failed=sum(not s['ok'] for s in samples),batchMeasurements=batches,directNativeTraces=native_traces)
print(json.dumps(report,indent=2))
if report['failed'] or not (native_traces or any((s['requests'] or 0)>1 for s in batches)):raise SystemExit(1)
