"""Measure wire arrival of progressive frames, with independent final pixels."""
import argparse,io,json,time,urllib.request,uuid,hashlib
from pathlib import Path
from PIL import Image
from websockets.sync.client import connect
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--port',type=int,required=True);p.add_argument('--label',required=True);p.add_argument('--limit',type=int,default=15);p.add_argument('--driver',default='openjpeg');p.add_argument('--overview',action='store_true');a=p.parse_args();base=f'http://127.0.0.1:{a.port}';rows=[];suffix=a.label+('-'+a.driver+'-overview' if a.overview else '')
def request(path,body=None):
 req=urllib.request.Request(base+path,data=json.dumps(body).encode() if body else None,headers={'Content-Type':'application/json'})
 with urllib.request.urlopen(req,timeout=90) as r:return json.load(r)
def stats(sid):
 with urllib.request.urlopen('http://127.0.0.1:18992/__stats/'+sid) as r:return json.load(r)['wire']
for f in json.loads((ROOT/'target/fixtures/corpus.json').read_text())['fixtures'][:a.limit]:
 sid='ws-'+uuid.uuid4().hex
 request('/api/s3/sources',dict(id=sid,name='Controlled progressive fixture',bucket='fixture',region='us-east-1',endpoint='http://host.containers.internal:18992/'+sid,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True,virtualHostedStyle=False))
 locator='s3-'+json.dumps([sid,f['file']],separators=(',',':')).encode().hex()
 start=time.perf_counter();manifest=request('/api/assets/'+locator+'/manifest');prepare_ms=(time.perf_counter()-start)*1000
 before=stats(sid);frames=[]
 with connect(f'ws://127.0.0.1:{a.port}/ws',max_size=32<<20,open_timeout=10) as ws:
  start=time.perf_counter();ws.send(json.dumps(dict(type='viewport',format='png',driver=a.driver,progressivePatches=True,viewport=dict(assetId=manifest['assetId'],sequence=1,x=0,y=0,width=4096 if a.overview else 512,height=4096 if a.overview else 512,zoom=3 if a.overview else 0))))
  while True:
   data=ws.recv(timeout=90);elapsed=(time.perf_counter()-start)*1000
   if isinstance(data,str):
    control=json.loads(data)
    if control.get('type')=='error':raise RuntimeError(control)
    continue
   assert data[:4]==b'RIVR';length=int.from_bytes(data[4:8],'little');header=json.loads(data[8:8+length]);raster=data[8+length:]
   with Image.open(io.BytesIO(raster)) as im:pixels=im.convert('RGBA').tobytes()
   frames.append(dict(ms=elapsed,header=header,pixelSha256=hashlib.sha256(pixels).hexdigest()))
   if header['pass']=='final':
    with Image.open(ROOT/'corpus'/f['file' if a.overview else 'source']) as im:
     if a.overview:im.reduce=3
     expected=im.convert('RGBA').crop((0,0,512,512)).tobytes()
    if pixels!=expected:raise RuntimeError('Progressive final pixels mismatch: '+f['file'])
    break
 after=stats(sid);preview=[x for x in frames if x['header']['pass']=='preview'];expected_count=preview[0]['header'].get('previewPatchCount',1) if preview else 0
 if preview and len(preview)!=expected_count:raise RuntimeError('Incomplete preview')
 row=dict(image=f['file'],label=a.label,manifestMs=prepare_ms,firstFrameMs=frames[0]['ms'],fullPreviewMs=preview[-1]['ms'] if preview else None,finalMs=frames[-1]['ms'],referencePixelsExact=True,wireAfterManifest={k:after[k]-before[k] for k in after},wireIncludingManifest=after,frames=frames)
 rows.append(row);print(json.dumps({k:v for k,v in row.items() if k!='frames'}),flush=True)
 urllib.request.urlopen(urllib.request.Request(base+'/api/s3/sources/'+sid,method='DELETE')).close()
 (ROOT/'target/benchmarks'/('progressive-'+suffix+'.json')).write_text(json.dumps(dict(complete=False,rows=rows),indent=2)+'\n')
(ROOT/'target/benchmarks'/('progressive-'+suffix+'.json')).write_text(json.dumps(dict(complete=True,scope=('4096x4096 overview at reduction 3' if a.overview else '512x512 full-resolution viewport')+'; first/full-preview/final measured after manifest preparation; no browser paint',requestedDriver=a.driver,rows=rows),indent=2)+'\n')
