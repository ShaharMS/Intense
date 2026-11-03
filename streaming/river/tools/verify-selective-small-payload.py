"""A cropped WebSocket view must not trigger the whole-small-file shortcut."""
import argparse,base64,io,json,pathlib,subprocess,threading,time,urllib.request,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--old-api',required=True);p.add_argument('--new-api',required=True);p.add_argument('--output',required=True);a=p.parse_args();root=pathlib.Path(__file__).resolve().parents[1];source=root/'reports/full-payload-m98/commented-full.jp2';data=source.read_bytes();events=[]
class S3(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def log_message(self,*args):pass
 def do_HEAD(self):self.send_response(200);self.send_header('Content-Length',str(len(data)));self.send_header('ETag','"selective"');self.end_headers()
 def do_GET(self):
  lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));assert self.headers['If-Match']=='"selective"';body=data[lo:hi+1];time.sleep(.01);events.append({'start':lo,'end':hi,'bytes':len(body)});self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{len(data)}');self.send_header('ETag','"selective"');self.end_headers();self.wfile.write(body)
server=ThreadingHTTPServer(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
script=r"""
const ws=new WebSocket(process.argv[1].replace('http','ws')+'/ws/tiles');ws.binaryType='arraybuffer';let start;
const timeout=setTimeout(()=>process.exit(1),30000);
ws.onopen=()=>{start=performance.now();ws.send(JSON.stringify({type:'tiles',driver:'jp2grok',format:'png',batch:{assetId:process.argv[2],sequence:1,tiles:[{x:0,y:0,z:0}]}}));};
ws.onmessage=e=>{if(typeof e.data==='string'){const m=JSON.parse(e.data);if(m.type==='error'){console.error(m);process.exit(1)}return;}const b=Buffer.from(e.data),n=b.readUInt32LE(4);clearTimeout(timeout);console.log(JSON.stringify({ms:performance.now()-start,header:JSON.parse(b.subarray(8,8+n)),png:b.subarray(8+n).toString('base64')}));ws.close();};
"""
def request(api,path,body=None,method=None):
 with urllib.request.urlopen(urllib.request.Request(api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}),timeout=30) as r:return json.load(r) if r.status!=204 else None
rows=[]
try:
 for version,api in [('old',a.old_api),('new',a.new_api)]:
  source_id='selective-'+uuid.uuid4().hex[:12];before=len(events)
  try:
   request(api,'/api/s3/sources',dict(id=source_id,name=source_id,bucket='fixture',endpoint=f'http://host.docker.internal:{server.server_port}',region='us-east-1',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True))
   opened=request(api,'/api/s3/sources/'+source_id+'/assets',{'key':'image.jp2'})
   response=json.loads(subprocess.check_output(['node','--input-type=module','-e',script,api,opened['assetId']],text=True));actual=Image.open(io.BytesIO(base64.b64decode(response['png']))).convert('RGBA');expected=Image.open(source).convert('RGBA').crop((0,0,512,512));assert actual.size==expected.size and actual.tobytes()==expected.tobytes();time.sleep(.15)
   row={'version':version,'elapsedMs':response['ms'],'pixels':'exact','objectBytes':len(data),'readBytes':sum(e['bytes'] for e in events[before:]),'requests':len(events)-before,'ranges':events[before:]};rows.append(row);print(json.dumps(row),flush=True)
  finally:request(api,'/api/s3/sources/'+source_id,method='DELETE')
 assert rows[0]['readBytes']<len(data)*.8 and rows[1]['readBytes']<=rows[0]['readBytes'],rows
 pathlib.Path(a.output).write_text(json.dumps({'scope':'Partial 512x512 transport tile of a 1024x512 source; additional lossless fixture with large tile comments; HTTP/1.1 and 10 ms range latency; original corpus unchanged','measurements':rows,'passed':True},indent=2))
finally:server.shutdown()
