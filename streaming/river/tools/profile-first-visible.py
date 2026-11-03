"""Profile selection-to-exact-frame arrival for the unindexed979MB source."""
import argparse,base64,hashlib,importlib.util,io,json,pathlib,subprocess,time,uuid
from urllib.parse import urlencode
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--container',required=True);p.add_argument('--origin',required=True);p.add_argument('--origin-control',required=True);p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--drivers',nargs='+',choices=['openjpeg','jp2grok','openhtj2k'],default=['openjpeg']);p.add_argument('--modes',nargs='+',choices=['viewport','patches','tiles'],default=['viewport','patches','tiles']);a=p.parse_args();assert a.container.startswith('river-goal-') and not a.output.exists()
sp=importlib.util.spec_from_file_location('h',pathlib.Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(sp);sp.loader.exec_module(h)
root=pathlib.Path('reports/fixed-gate-v2');manifest=json.loads((root/'manifest.json').read_bytes());case=next(c for c in manifest['cases'] if c['name']=='mb979-preview');refs={5:Image.open(root/case['reference']).convert('RGB'),6:Image.open(root/'first-visible/mb979-r6.png').convert('RGB')};meta=json.loads((root/'first-visible/reference.json').read_bytes());assert h.sha((root/'first-visible/mb979-r6.png').read_bytes())==meta['sha256'];assert meta['sourceSha256']==case['sha256'];assert h.sha((root/case['reference']).read_bytes())==case['referenceSha256']
origin=json.loads(h.http(a.origin_control+'/__manifest'));assert origin['sha256']==h.sha((root/'manifest.json').read_bytes())
image=json.loads(subprocess.check_output(['docker','inspect',a.container]))[0]['Image'];rows=[];report=dict(passed=False,imageId=image,manifestSha256=origin['sha256'],responseDelayMs=origin.get('responseDelayMs'),scope='One diagnostic sample per driver/mode. Fresh River process/source identity. WebSocket established before selection; timing starts before metadata/asset open. Independent exact references for EVERY preview and final frame. Frame arrival is not browser paint. No release-speedup inference.',rows=rows)
node=r'''const [api,sid,driver,mode]=process.argv.slice(1);const path=mode==='tiles'?'/ws/tiles':'/ws';const ws=new WebSocket(api.replace(/^http/,'ws')+path);ws.binaryType='arraybuffer';let selected=0,sent=0,assetOpenMs=0;const frames=[],finals=new Set(),timer=setTimeout(()=>{console.error('timeout');process.exit(1)},45000);ws.onerror=()=>process.exit(2);ws.onopen=async()=>{try{selected=performance.now();const r=await fetch(api+'/api/s3/sources/'+sid+'/assets',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({key:'mb979-preview.jp2'})});if(!r.ok)throw Error(await r.text());const asset=(await r.json()).assetId;assetOpenMs=performance.now()-selected;const message=mode==='tiles'?{type:'tiles',format:'png',driver,batch:{assetId:asset,sequence:1,tiles:[0,1,2].map(x=>({x,y:0,z:5}))}}:{type:'viewport',format:'png',driver,progressivePatches:mode==='patches',viewport:{assetId:asset,sequence:1,zoom:5,x:0,y:0,width:40980,height:10980}};sent=performance.now();ws.send(JSON.stringify(message));}catch(e){console.error(e);process.exit(3)}};ws.onmessage=e=>{if(typeof e.data==='string'){const m=JSON.parse(e.data);if(m.type==='error'){console.error(m);process.exit(4)}return;}const now=performance.now(),b=Buffer.from(e.data);if(b.subarray(0,4).toString()!=='RIVR')throw Error('magic');const n=b.readUInt32LE(4),header=JSON.parse(b.subarray(8,8+n));frames.push({header,selectionMs:now-selected,requestMs:now-sent,png:b.subarray(8+n).toString('base64')});if(header.pass==='final')finals.add(`${header.tileX}/${header.tileY}`);if(finals.size===3){clearTimeout(timer);console.log(JSON.stringify({assetOpenMs,frames}));ws.close();}};'''
try:
 for driver in a.drivers:
  for mode in a.modes:
   subprocess.run(['docker','restart',a.container],check=True,stdout=subprocess.DEVNULL)
   for _ in range(100):
    try:h.http(a.api+'/api/drivers');break
    except OSError:time.sleep(.1)
   sid='visible-'+uuid.uuid4().hex;endpoint=a.origin+'/'+sid
   try:
    h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST');assert json.loads(h.http(a.origin_control+'/__stats/'+sid))['wire']==dict(gets=0,heads=0,bytes=0)
    result=subprocess.run(['node','-e',node,a.api,sid,driver,mode],capture_output=True,text=True,timeout=55);assert result.returncode==0,result.stderr;data=json.loads(result.stdout);frames=data['frames'];nonblack=[];finals=[]
    for f in frames:
     header=f['header'];im=Image.open(io.BytesIO(base64.b64decode(f['png']))).convert('RGB');reduce=5 if header['pass']=='final' else 6;ref=refs[reduce];edge=header.get('sourceTileEdge',512*(1<<header['zoom']))//(1<<reduce);x=header['tileX']*edge;y=header['tileY']*edge;gold=ref.crop((x,y,min(x+edge,ref.width),min(y+edge,ref.height)))
     assert im.size==gold.size and im.tobytes()==gold.tobytes(),(driver,mode,header,im.size,gold.size)
     if im.getbbox():nonblack.append(f)
     if header['pass']=='final':finals.append(f)
    assert len(finals)==3 and nonblack
    wire=json.loads(h.http(a.origin_control+'/__stats/'+sid))['wire']
    # Reuse the already selected asset; warm requests need no metadata open.
    start=node.index("const r=await fetch(");stop=node.index("assetOpenMs=",start)
    repeat_node=node[:start]+"const asset=process.argv[5];"+node[stop:]
    repeat=subprocess.run(['node','-e',repeat_node,a.api,sid,driver,mode,frames[0]['header']['assetId']],capture_output=True,text=True,timeout=55);assert repeat.returncode==0,repeat.stderr;repeat_frames=json.loads(repeat.stdout)['frames']
    def identity(f):
     h=f['header'];return (h['pass'],h['zoom'],h['tileX'],h['tileY'],h.get('sourceTileEdge'))
    cold={identity(f):(Image.open(io.BytesIO(base64.b64decode(f['png']))).size,hashlib.sha256(Image.open(io.BytesIO(base64.b64decode(f['png']))).convert('RGBA').tobytes()).hexdigest()) for f in frames}
    assert {identity(f) for f in repeat_frames}==set(cold)
    for f in repeat_frames:
     im=Image.open(io.BytesIO(base64.b64decode(f['png']))).convert('RGBA');assert (im.size,hashlib.sha256(im.tobytes()).hexdigest())==cold[identity(f)]
     assert im.getchannel('A').getextrema()==(255,255),'Opaque RGB fixture unexpectedly transparent'
    assert json.loads(h.http(a.origin_control+'/__stats/'+sid))['wire']==wire,'Cached WS request performed source IO'
    row=dict(driver=driver,mode=mode,assetOpenMs=data['assetOpenMs'],firstFrameMs=frames[0]['selectionMs'],firstNonBlackMs=nonblack[0]['selectionMs'],firstNonBlackPass=nonblack[0]['header']['pass'],firstFinalMs=finals[0]['selectionMs'],allFinalMs=max(f['selectionMs'] for f in finals),frames=len(frames),frameTimings=[dict(selectionMs=f['selectionMs'],requestMs=f['requestMs'],header=f['header']) for f in frames],everyFramePixelsExact=True,wire=wire,firstNonBlackHeader=nonblack[0]['header']);row['repeatSourceIoZero']=True;row['repeatEveryFramePixelsExact']=True;rows.append(row);print(json.dumps({k:v for k,v in row.items() if k!='frameTimings'}),flush=True)
   finally:h.http(a.api+'/api/s3/sources/'+sid,method='DELETE')
 report['passed']=True
finally:a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
