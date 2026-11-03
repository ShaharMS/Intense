"""Validate the full unindexed preview through progressive WebSockets."""
import argparse,base64,importlib.util,io,json,re,subprocess,threading,time,uuid
from pathlib import Path
from http.server import ThreadingHTTPServer
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();assert not a.output.exists()
sp=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(sp);sp.loader.exec_module(h)
root=Path('reports/fixed-gate-v2');m=json.loads((root/'manifest.json').read_text());c=next(c for c in m['cases'] if c['name']=='mb979-preview');file=(root/c['file']).resolve();ref=Image.open(root/c['reference']).convert('RGB');assert ref.size==(1281,344)
aliases=[dict(c,name=d+'-'+mode) for d in ['openjpeg','jp2grok','openhtj2k'] for mode in ['viewport','patches','tiles']];counts={};lock=threading.Lock();Origin=h.make_origin({c['name']:file for c in aliases},aliases,dict(cases=aliases),counts,lock)
class Delayed(Origin):
 def do_GET(self):time.sleep(.02);super().do_GET()
server=ThreadingHTTPServer(('0.0.0.0',18994),Delayed);threading.Thread(target=server.serve_forever,daemon=True).start()
node=re.search("node=r'''(.*?)'''",Path(__file__).with_name('verify-window-ws.py').read_text(),re.S)[1].replace('finals.size===2','finals.size===3')
sid='whole-ws-'+uuid.uuid4().hex;endpoint='http://host.docker.internal:18994/'+sid;rows=[];report=dict(passed=False,responseDelayMs=20,scope='Full unindexed979MB preview at reduction5, three cropped PNG tiles; independent original-file final reference. Preview payloads decoded but not claimed pixel-exact.',rows=rows)
try:
 h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
 for driver in ['openjpeg','jp2grok','openhtj2k']:
  for mode in ['viewport','patches','tiles']:
   asset=json.loads(h.http(a.api+'/api/s3/sources/'+sid+'/assets',dict(key=driver+'-'+mode+'.jp2'),'POST'))['assetId']
   if mode=='tiles':message=dict(type='tiles',format='png',driver=driver,batch=dict(assetId=asset,sequence=1,tiles=[dict(x=x,y=0,z=5) for x in range(3)]));path='/ws/tiles'
   else:message=dict(type='viewport',format='png',driver=driver,progressivePatches=mode=='patches',viewport=dict(assetId=asset,sequence=1,zoom=5,x=0,y=0,width=40980,height=10980));path='/ws'
   run=subprocess.run(['node','-e',node,a.api.replace('http','ws',1)+path,json.dumps(message)],capture_output=True,text=True,timeout=40);assert run.returncode==0,run.stderr;frames=json.loads(run.stdout);finals=set()
   for f in frames:
    im=Image.open(io.BytesIO(base64.b64decode(f['png']))).convert('RGB');head=f['header'];assert im.width>0 and im.height>0
    if head['pass']=='final':
     x=head['tileX'];assert x in range(3) and head['tileY']==0;gold=ref.crop((x*512,0,min((x+1)*512,1281),344));assert im.size==gold.size and im.tobytes()==gold.tobytes(),(driver,mode,head,im.size,gold.size);finals.add(x)
   assert finals=={0,1,2};rows.append(dict(driver=driver,mode=mode,frames=len(frames),exactFinalTiles=3));print(json.dumps(rows[-1]),flush=True)
 report['passed']=True
finally:
 h.http(a.api+'/api/s3/sources/'+sid,method='DELETE');server.shutdown();a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
