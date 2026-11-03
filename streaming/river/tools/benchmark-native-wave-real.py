"""Checkpoint 93 versus wave synchronization on unchanged 979 MB source."""
import base64,hashlib,io,json,pathlib,subprocess,time,urllib.request,urllib.parse,uuid
from PIL import Image
root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/native-wave-real-m94';out.mkdir(exist_ok=True)
corpus=root/'corpus/cache/sentinel-date-mosaics';entry=next(e for e in json.loads((corpus/'river-assets.json').read_text())['assets'] if e['source']=='2016-09-12.jp2');asset=entry['assetId']
(out/'catalog.json').write_text(json.dumps({'assets':[{k:entry[k] for k in ['assetId','width','height','bitDepth','source']}]}));api='http://127.0.0.1:18587';rows=[];uid=uuid.uuid4().hex[:8]
def start(name,image,gpu):
 command=['docker','run','-d','--name',name,'-p','127.0.0.1:18587:8080','-e','RIVER_ENABLE_NVIDIA='+str(gpu).lower(),'-e','RIVER_OTEL_ENABLED=false','-e','RIVER_REFINE_IDLE_DELAY_MS=0','-e','RIVER_RASTER_CACHE_BYTES=0','-e','RIVER_RASTER_DISK_CACHE_BYTES=0','-e','RIVER_ASSET_CATALOG=/study/catalog.json','--mount',f'type=bind,source={corpus},target=/data,readonly','--mount',f'type=bind,source={out/"catalog.json"},target=/study/catalog.json,readonly',image]
 if gpu:command[2:2]=['--gpus','all']
 subprocess.run(command,check=True,capture_output=True)
 deadline=time.monotonic()+60
 while True:
  try:json.load(urllib.request.urlopen(api+'/api/drivers',timeout=1));break
  except OSError:
   if time.monotonic()>deadline:raise
   time.sleep(.05)
 return json.load(urllib.request.urlopen(api+'/api/assets/'+asset+'/manifest'))
script=r"""
const api=process.argv[1],asset=process.argv[2];const request={type:'tiles',driver:'nvjpeg2000',format:'png',batch:{assetId:asset,sequence:1,tiles:[{x:0,y:0,z:2},{x:1,y:0,z:2},{x:0,y:1,z:2},{x:1,y:1,z:2}]}};
const ws=new WebSocket(api.replace('http','ws')+'/ws/tiles');ws.binaryType='arraybuffer';let start,first,frames=[];
const timeout=setTimeout(()=>{ws.close();console.error('timeout');process.exit(1)},60000);
ws.onopen=()=>{start=performance.now();ws.send(JSON.stringify(request));};
ws.onmessage=event=>{
 if(typeof event.data==='string'){const m=JSON.parse(event.data);if(m.type==='error'){console.error(m);process.exit(1)}return;}
 const b=Buffer.from(event.data);const n=b.readUInt32LE(4),header=JSON.parse(b.subarray(8,8+n));
 if(header.pass!=='final')return;
 first??=performance.now()-start;frames.push({header,data:b.subarray(8+n).toString('base64')});
 if(frames.length===4){clearTimeout(timeout);console.log(JSON.stringify({firstMs:first,fullMs:performance.now()-start,frames}));ws.close();}
};
"""
def fingerprint(data):
 image=Image.open(io.BytesIO(data)).convert('RGBA');return image.size,hashlib.sha256(image.tobytes()).hexdigest()
oracles={};name=f'river-goal-waves-oracle-{uid}'
try:
 manifest=start(name,'river:2026.10.15-errors-combo-m93',False);matrix=manifest['resolutionLevels']-3
 for x,y in [(0,0),(1,0),(0,1),(1,1)]:
  query=urllib.parse.urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/png',DRIVER='jp2grok',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=matrix,TILECOL=x,TILEROW=y))
  with urllib.request.urlopen(api+'/wmts?'+query,timeout=60) as response:oracles[(x,y)]=fingerprint(response.read())
finally:subprocess.run(['docker','stop',name],check=True,capture_output=True)
for run in range(3):
 versions=[('old','river:2026.10.15-errors-combo-m93'),('new','river:2026.10.15-gpu-waves-trial-m94')]
 for label,image in (versions if run%2==0 else list(reversed(versions))):
  name=f'river-goal-waves-real-{uid}-{run}-{label}'
  try:
   start(name,image,True)
   for state in ['cold','repeat']:
    result=json.loads(subprocess.check_output(['node','--input-type=module','-e',script,api,asset],text=True));times=[]
    for frame in result.pop('frames'):
     h=frame['header'];assert fingerprint(base64.b64decode(frame['data']))==oracles[(h['tileX'],h['tileY'])]
     assert h['serverTiming']['decoderRoute'].startswith('nvjpeg2000-native'),h
     times.append(h['serverTiming']['decodeMs'])
    row=dict(run=run,version=label,state=state,pixels='exact',decodeMs=max(times),**result);rows.append(row);print(json.dumps(row),flush=True)
  finally:subprocess.run(['docker','stop',name],check=True,capture_output=True)
  (out/'results.json').write_text(json.dumps(rows,indent=2))
