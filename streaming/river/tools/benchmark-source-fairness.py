"""Competing unindexed layered JP2 preparations must not block another source."""
import argparse,base64,subprocess,concurrent.futures,hashlib,io,json,pathlib,threading,time,urllib.request,urllib.parse,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--label',required=True);p.add_argument('--runs',type=int,default=1);p.add_argument('--protocol',choices=['wmts','websocket'],default='wmts');a=p.parse_args();root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/source-fairness-m106';paths={'heavy.jp2':out/'layered-large.jp2','fast.jp2':out/'fast.jp2'};events=[];active=threading.Event();armed=False
class S3(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def log_message(self,*args):pass
 def key(self):return urllib.parse.unquote(urllib.parse.urlparse(self.path).path).split('/')[-1]
 def do_HEAD(self):
  key=self.key();self.send_response(200);self.send_header('Content-Length',str(paths[key].stat().st_size));self.send_header('ETag','"fair-'+key+'"');self.end_headers()
 def do_GET(self):
  key=self.key();lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));assert self.headers['If-Match']=='"fair-'+key+'"'
  if armed and key=='heavy.jp2' and lo>1024:active.set()
  time.sleep(.01)
  with paths[key].open('rb') as stream:stream.seek(lo);data=stream.read(hi-lo+1)
  events.append({'key':key,'bytes':len(data)});self.send_response(206);self.send_header('Content-Length',str(len(data)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{paths[key].stat().st_size}');self.send_header('ETag','"fair-'+key+'"');self.end_headers();self.wfile.write(data)
server=ThreadingHTTPServer(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(path,body=None,method=None):
 with urllib.request.urlopen(urllib.request.Request(a.api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=45) as r:return json.load(r) if r.status!=204 else None
reference=Image.open(out/'layered-reference.png').convert('RGBA');ws_reference=Image.open(out/'layered-preview.ppm').convert('RGBA');fast_reference=Image.open(paths['fast.jp2']);fast_reference.reduce=5;fast_reference.load();fast_reference=fast_reference.convert('RGBA');rows=[]
try:
 for run in range(a.runs):
  source='fair-'+uuid.uuid4().hex[:12];start_event=len(events);active.clear();armed=False
  try:
   request('/api/s3/sources',dict(id=source,name=source,bucket='fixture',region='us-east-1',endpoint=f'http://host.docker.internal:{server.server_port}',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True))
   assets={}
   for key in paths:
    asset=request('/api/s3/sources/'+source+'/assets',{'key':key})['assetId'];assets[key]=request('/api/assets/'+asset+'/manifest')
   assert assets['heavy.jp2']['resolutionLevels']==5
   with paths['heavy.jp2'].open('rb') as fixture:
    header=fixture.read(65536);cod=header.index(b'\xff\x52');assert int.from_bytes(header[cod+6:cod+8],'big')==2
   def tile(key,x,y,matrix):
    asset=assets[key]['assetId'];query=urllib.parse.urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/png',DRIVER='jp2grok',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=matrix,TILECOL=x,TILEROW=y));start=time.perf_counter()
    with urllib.request.urlopen(a.api+'/wmts?'+query,timeout=45) as response:data=response.read()
    return {'elapsedMs':(time.perf_counter()-start)*1000,'png':data,'key':key,'x':x,'y':y}
   armed=True
   if a.protocol=='websocket':
    script=r"""
const [api,asset,x,y]=process.argv.slice(1);const ws=new WebSocket(api.replace('http','ws')+'/ws/tiles');ws.binaryType='arraybuffer';let start;
const timeout=setTimeout(()=>{console.error('WS timed out');process.exit(1)},45000);
ws.onopen=()=>console.log('READY');process.stdin.once('data',()=>{start=performance.now();ws.send(JSON.stringify({type:'tiles',driver:'jp2grok',format:'png',batch:{assetId:asset,sequence:1,tiles:[{x:Number(x),y:Number(y),z:0}]}}));});
ws.onmessage=e=>{if(typeof e.data==='string'){const m=JSON.parse(e.data);if(m.type==='error'){console.error(m);process.exit(1)}return;}
const b=Buffer.from(e.data),n=b.readUInt32LE(4),header=JSON.parse(b.subarray(8,8+n));if(header.pass!=='preview')return;
clearTimeout(timeout);console.log(JSON.stringify({elapsedMs:performance.now()-start,png:b.subarray(8+n).toString('base64'),header}));ws.close();process.stdin.destroy();};
"""
    children=[subprocess.Popen(['node','-e',script,a.api,assets['heavy.jp2']['assetId'],str(x),'7'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True) for x in [0,1]]
    for child in children:assert child.stdout.readline().strip()=='READY'
    for child in children:child.stdin.write('go\n');child.stdin.flush()
    assert active.wait(5);time.sleep(.05);fast=tile('fast.jp2',0,0,0);results=[]
    for x,child in enumerate(children):
     output,error=child.communicate(timeout=45);assert child.returncode==0,error;result=json.loads(output);result['png']=base64.b64decode(result['png']);result.update(key='heavy.jp2',x=x,y=7);results.append(result)
   else:
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
     heavy=[pool.submit(tile,'heavy.jp2',x,7,assets['heavy.jp2']['resolutionLevels']-1) for x in [0,1]]
     assert active.wait(5),'heavy scan did not begin';time.sleep(.05)
     fast=tile('fast.jp2',0,0,0)
     results=[future.result(timeout=45) for future in heavy]
   for result in results:
    actual=Image.open(io.BytesIO(result.pop('png'))).convert('RGBA');expected=ws_reference.crop((result['x']*128,896,result['x']*128+128,1024)) if a.protocol=='websocket' else reference.crop((result['x']*512,3584,result['x']*512+512,4096));assert actual.size==expected.size and actual.tobytes()==expected.tobytes(),'heavy ROI pixels'
   actual=Image.open(io.BytesIO(fast.pop('png'))).convert('RGBA');assert actual.size==fast_reference.size and actual.tobytes()==fast_reference.tobytes(),'fast source pixels'
   with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
    warm=list(pool.map(lambda x:tile('heavy.jp2',x,7,assets['heavy.jp2']['resolutionLevels']-1),[2,3]))
   for result in warm:
    actual=Image.open(io.BytesIO(result.pop('png'))).convert('RGBA');expected=reference.crop((result['x']*512,3584,result['x']*512+512,4096));assert actual.size==expected.size and actual.tobytes()==expected.tobytes(),'warm uncached ROI pixels'
   row={'warmUncachedMs':[r['elapsedMs'] for r in warm],'protocol':a.protocol,'version':a.label,'run':run,'fastMs':fast['elapsedMs'],'heavyMs':[r['elapsedMs'] for r in results],'sourceBytes':sum(e['bytes'] for e in events[start_event:]),'rangeRequests':len(events)-start_event,'pixels':'exact'};rows.append(row);print(json.dumps(row),flush=True)
  finally:armed=False;request('/api/s3/sources/'+source,method='DELETE')
  (out/(a.label+'.json')).write_text(json.dumps({'scope':'Two simultaneous cold bottom-row requests on one unindexed two-layer JP2, followed by a small-source request; two global source IO slots, no CPU quota, 10 ms per range. Auxiliary free-box padding makes the synthetic object large; original corpus untouched.','measurements':rows},indent=2))
finally:server.shutdown()
