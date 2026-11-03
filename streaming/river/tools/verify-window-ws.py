"""Validate windowed final pixels through both WS forms on the physical11GB source."""
import argparse,base64,importlib.util,io,json,subprocess,threading,uuid,time
from pathlib import Path
from http.server import ThreadingHTTPServer
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--source-delay-ms',type=float,default=0);p.add_argument('--local-asset-id',help='Use an already cataloged local copy of the frozen 11GB source');a=p.parse_args();assert not a.output.exists()
spec=importlib.util.spec_from_file_location('h',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
root=Path('reports/fixed-gate-v2');m=json.loads((root/'manifest.json').read_text());c=next(c for c in m['cases'] if c['name']=='gb11-deep');file=(root/c['file']).resolve();aliases=[dict(c,name=driver+'-'+mode+'-'+c['name']) for driver in ['openjpeg','jp2grok','openhtj2k'] for mode in ['viewport','patches','tiles']];counters={};lock=threading.Lock();Origin=h.make_origin({v['name']:file for v in aliases},aliases,dict(cases=aliases),counters,lock)
class DelayedOrigin(Origin):
 def do_GET(self):
  if a.source_delay_ms:time.sleep(a.source_delay_ms/1000)
  super().do_GET()
 def do_HEAD(self):
  if a.source_delay_ms:time.sleep(a.source_delay_ms/1000)
  super().do_HEAD()
server=ThreadingHTTPServer(('0.0.0.0',18994),DelayedOrigin);threading.Thread(target=server.serve_forever,daemon=True).start()
sid='window-ws-'+uuid.uuid4().hex;endpoint=f'http://host.docker.internal:18994/{sid}';rows=[];report=dict(passed=False,rows=rows,scope='Two adjacent full-resolution tiles on the actual frozen11GB source; exact final pixels for viewport, progressive patches and XYZ batches across three CPU drivers. Preview payloads validated as images, not used for performance claims.')
node=r'''const [url,request]=process.argv.slice(1),ws=new WebSocket(url);ws.binaryType='arraybuffer';const frames=[],finals=new Set();const timer=setTimeout(()=>{console.error('timeout');process.exit(1)},30000);ws.onerror=()=>process.exit(2);ws.onopen=()=>ws.send(request);ws.onmessage=e=>{if(typeof e.data==='string'){let m=JSON.parse(e.data);if(m.type==='error'){console.error(m);process.exit(3)}return;}const b=Buffer.from(e.data);if(b.subarray(0,4).toString()!=='RIVR')throw Error('bad magic');let n=b.readUInt32LE(4),header=JSON.parse(b.subarray(8,8+n));frames.push({header,png:b.subarray(8+n).toString('base64')});if(header.pass==='final')finals.add(`${header.tileX}/${header.tileY}`);if(finals.size===2){clearTimeout(timer);console.log(JSON.stringify(frames));ws.close();}};'''
try:
 if not a.local_asset_id:h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
 report['responseDelayMs']=a.source_delay_ms
 report['sourceTransport']='local-file' if a.local_asset_id else 's3-range-fixture'
 gold={(68,160):Image.open(root/'references/eleven-deep.png').convert('RGB'),(69,160):Image.open(root/'references/eleven-neighbor.png').convert('RGB')}
 previews={(68,160):Image.open(root/'references/eleven-deep-r2.png').convert('RGB'),(69,160):Image.open(root/'references/eleven-neighbor-r2.png').convert('RGB')}
 for driver in ['openjpeg','jp2grok','openhtj2k']:
  for mode in ['viewport','patches','tiles']:
   asset=a.local_asset_id or json.loads(h.http(a.api+'/api/s3/sources/'+sid+'/assets',dict(key=driver+'-'+mode+'-'+c['name']+'.jp2'),'POST'))['assetId']
   if mode=='tiles':message=dict(type='tiles',format='png',driver=driver,batch=dict(assetId=asset,sequence=1,tiles=[dict(x=x,y=160,z=0) for x in [68,69]]));path='/ws/tiles'
   else:message=dict(type='viewport',format='png',driver=driver,progressivePatches=mode=='patches',viewport=dict(assetId=asset,sequence=1,zoom=0,x=34816,y=81920,width=1024,height=512));path='/ws'
   run=subprocess.run(['node','-e',node,a.api.replace('http','ws',1)+path,json.dumps(message)],capture_output=True,text=True,timeout=40);assert run.returncode==0,run.stderr;frames=json.loads(run.stdout);finals=[]
   for f in frames:
    im=Image.open(io.BytesIO(base64.b64decode(f['png']))).convert('RGB');header=f['header'];assert im.width>0 and im.height>0
    if header['pass']=='preview':
     ref=previews[(header['tileX'],header['tileY'])];assert im.size==ref.size and im.tobytes()==ref.tobytes(),(driver,mode,header)
    if header['pass']=='final':
     ref=gold[(header['tileX'],header['tileY'])];assert im.size==ref.size and im.tobytes()==ref.tobytes(),(driver,mode,header)
     route=header['serverTiming']['decoderRoute']
     assert 'precinct-window' in route or (a.local_asset_id and mode!='viewport' and route=='encoded-raster-cache'),header
     finals.append((header['tileX'],header['tileY']))
   assert len(finals)==2;rows.append(dict(driver=driver,mode=mode,frames=len(frames),exactFinalTiles=len(finals)));print(json.dumps(rows[-1]),flush=True)
 report['passed']=True
except Exception as error:
 report['failure']=str(error)
 raise
finally:
 try:
  if not a.local_asset_id:h.http(a.api+'/api/s3/sources/'+sid,method='DELETE')
 except Exception as error:report['cleanupError']=str(error)
 server.shutdown();a.output.write_text(json.dumps(report,indent=2)+'\n')
