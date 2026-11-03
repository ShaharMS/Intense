"""Fresh-process GPU wave barriers versus checkpoint 93, independent pixels."""
import argparse,hashlib,io,json,pathlib,statistics,subprocess,time,urllib.request,urllib.parse,uuid
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--candidate',required=True);p.add_argument('--runs',type=int,default=3);p.add_argument('--report',default='native-wave-sync-m94');a=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports'/a.report;out.mkdir(exist_ok=True)
# Additional fixtures; served corpus originals are never rewritten.
import numpy as np
y,x=np.indices((2048,2048),dtype=np.uint32);pixels=np.stack([(x*13+y*7)%256,(x*x//37+y*19)%256,(x*3+y*y//43)%256],axis=2).astype('uint8')
entries=[];oracles={}
for edge in [128,256,512]:
 source=out/f'native-{edge}.jp2'
 if not source.exists():Image.fromarray(pixels).save(source,format='JPEG2000',tile_size=(edge,edge),num_resolutions=6,irreversible=False)
 asset=hashlib.sha256(source.read_bytes()).hexdigest();entries.append(dict(assetId=asset,width=2048,height=2048,bitDepth=8,source=source.name))
 reference=Image.open(source);reference.reduce=2;reference.load();reference=reference.convert('RGBA');oracles[asset]=(reference.size,hashlib.sha256(reference.tobytes()).hexdigest())
(out/'catalog.json').write_text(json.dumps({'assets':entries}))
rows=[];tag='river:2026.10.15-errors-combo-m93';run_id=uuid.uuid4().hex[:8]
for run in range(a.runs):
 for label,image in ([('old',tag),('new',a.candidate)] if run%2==0 else [('new',a.candidate),('old',tag)]):
  name=f'river-goal-waves-{run_id}-{run}-{label}';api='http://127.0.0.1:18586'
  command=['docker','run','-d','--gpus','all','--name',name,'-p','127.0.0.1:18586:8080','-e','RIVER_ENABLE_NVIDIA=true','-e','RIVER_OTEL_ENABLED=false','-e','RIVER_DECODE_CONCURRENCY=1','-e','RIVER_NVJPEG2000_NATIVE_WORKERS=1','-e','RIVER_RASTER_CACHE_BYTES=0','-e','RIVER_RASTER_DISK_CACHE_BYTES=0','-e','RIVER_ASSET_CATALOG=/data/catalog.json','--mount',f'type=bind,source={out},target=/data,readonly',image]
  subprocess.run(command,check=True,capture_output=True)
  try:
   deadline=time.monotonic()+60
   while True:
    try:json.load(urllib.request.urlopen(api+'/api/drivers',timeout=1));break
    except OSError:
     if time.monotonic()>deadline:raise
     time.sleep(.05)
   for edge,entry in zip([128,256,512],entries):
    asset=entry['assetId'];json.load(urllib.request.urlopen(api+'/api/assets/'+asset+'/manifest'))
    query=urllib.parse.urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/png',DRIVER='nvjpeg2000',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=3,TILEROW=0,TILECOL=0))
    for state in ['cold','repeat','repeat2']:
     started=time.perf_counter()
     with urllib.request.urlopen(api+'/wmts?'+query,timeout=60) as response:
      payload=response.read();headers=response.headers
     wall=(time.perf_counter()-started)*1000;result=Image.open(io.BytesIO(payload)).convert('RGBA')
     assert (result.size,hashlib.sha256(result.tobytes()).hexdigest())==oracles[asset],(label,edge,state)
     route=headers['X-River-Decoder-Route'];assert route.startswith('nvjpeg2000'),route
     row=dict(run=run,version=label,nativeTileEdge=edge,nativeTiles=(2048//edge)**2,state=state,wallMs=wall,decodeMs=float(headers['X-River-Decode-Ms']),pixels='exact',route=route)
     rows.append(row);print(json.dumps(row),flush=True)
  finally:subprocess.run(['docker','stop',name],check=True,capture_output=True)
  (out/'results.json').write_text(json.dumps(rows,indent=2))
summary=[]
for edge in [128,256,512]:
 for state in ['cold','repeat','repeat2']:
  sample={label:[row for row in rows if row['version']==label and row['nativeTileEdge']==edge and row['state']==state] for label in ['old','new']}
  summary.append(dict(nativeTileEdge=edge,state=state,**{label:{key:statistics.median(row[key] for row in group) for key in ['wallMs','decodeMs']} for label,group in sample.items()}))
(out/'summary.json').write_text(json.dumps({'scope':'Three alternating fresh process trials, original lossless test images, independently reduced OpenJPEG pixels; encoded caches off; OS/GPU caches warm; one GPU worker to isolate wave reuse','measurements':summary},indent=2));print(json.dumps(summary,indent=2))
