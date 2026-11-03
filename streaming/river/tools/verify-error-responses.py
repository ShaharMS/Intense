"""Exercise the public HTTP error contract, including framework rejections."""
import argparse,json,urllib.request,urllib.error,urllib.parse
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output');a=p.parse_args();results=[]
def request(path,method='GET',body=None,content_type=None,auth=True):
    headers={'Authorization':'Bearer fixture-admin'} if auth else {}
    if content_type:headers['Content-Type']=content_type
    try:
        with urllib.request.urlopen(urllib.request.Request(a.api+path,method=method,data=body,headers=headers),timeout=30) as r:return r.status,r.headers,r.read()
    except urllib.error.HTTPError as e:return e.code,e.headers,e.read()
def check(path,status,code=None,**kwargs):
    actual,headers,data=request(path,**kwargs);assert actual==status,(path,actual,data)
    value=json.loads(data);assert headers['Content-Type'].startswith('application/json')
    for key in ['code','message','details','status','path','method','requestId']:assert key in value,(path,key,value)
    assert value['message'].strip() and value['status']==status and value['path']==path.split('?')[0]
    assert headers['X-River-Request-Id']==value['requestId'];assert headers['Cache-Control']=='no-store'
    if code:assert value['code']==code,(path,value)
    assert 'secret-query-value' not in data.decode()
    results.append(dict(path=path.split('?')[0],status=status,code=value['code']));return value
status,_,data=request('/api/assets');assert status==200
assets=json.loads(data);asset=next(v for v in assets if v.get('width',0)>512)['assetId']
base='/streamer/v1/';query='?'+urllib.parse.urlencode({'image':asset})
assert request(base+'0/0/0/tile.png'+query)[0]==200
error=check(base+'1/1/1/tile.png'+query,404,'tile_out_of_bounds');assert error['details']['maxColumn']==0 and error['details']['maxRow']==0
check(base+'15/0/0/tile.png'+query,400,'unsupported_tile_matrix')
check(base+'0/0/0/tile.png'+query+'&driver=unknown',400,'unknown_driver')
check(base+'0/0/0/tile.bad'+query,400,'unsupported_tile_extension')
check(base+'bad/0/0/tile.png'+query,400)
check(base+'0/0/0/tile.png',400)
check('/api/assets?limit=wrong',400)
check('/api/assets/'+('a'*64)+'/manifest',404,'asset_not_found')
check('/api/s3/sources/absent/objects',404,'s3_source_not_found')
check('/api/assets?source=absent',404,'s3_source_not_found')
check('/api/s3/sources',400,method='POST',body=b'{',content_type='application/json')
check('/api/s3/sources',422,method='POST',body=b'{}',content_type='application/json')
check('/api/s3/sources',415,method='POST',body=b'{}',content_type='text/plain')
check('/api/settings/encoding',400,'invalid_webp_method',method='PUT',body=b'{"webpMethod":7}',content_type='application/json')
check('/api/assets',405,method='DELETE')
check('/does-not-exist?token=secret-query-value',404,'route_not_found')
check('/ws',400)
# Validate source setup and upstream failures without relying on a real S3 account.
import threading,uuid,subprocess
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
class MissingObject(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_HEAD(self):self.send_response(404);self.send_header('Content-Length','0');self.end_headers()
    def do_GET(self):self.do_HEAD()
server=ThreadingHTTPServer(('0.0.0.0',0),MissingObject);threading.Thread(target=server.serve_forever,daemon=True).start()
source='errors-'+uuid.uuid4().hex[:12]
connection=dict(id=source,name='error fixture',region='us-east-1',bucket='fixture',endpoint=f'http://host.docker.internal:{server.server_port}',prefix='allowed',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True)
try:
    bad=dict(connection,name='')
    invalid=check('/api/s3/sources',400,'invalid_s3_source',method='POST',body=json.dumps(bad).encode(),content_type='application/json');assert 'name' in invalid['message']
    assert request('/api/s3/sources',method='POST',body=json.dumps(connection).encode(),content_type='application/json')[0]==200
    check('/api/s3/sources/'+source+'/assets',404,'s3_object_not_found',method='POST',body=b'{"key":"allowed/absent.jp2"}',content_type='application/json')
    check('/api/s3/sources/'+source+'/assets',400,'object_outside_source_prefix',method='POST',body=b'{"key":"outside.jp2"}',content_type='application/json')
    check('/api/s3/sources/'+source+'/assets',400,'unsupported_object',method='POST',body=b'{"key":"allowed/file.txt"}',content_type='application/json')
finally:
    request('/api/s3/sources/'+source,method='DELETE');server.shutdown()
script = r"""
const base=process.argv[1],asset=process.argv[2];
async function failure(payload,code) {
 return await new Promise((resolve,reject)=>{
  const ws=new WebSocket(base.replace('http','ws')+'/ws/tiles');
  const timeout=setTimeout(()=>{ws.close();reject(new Error('WebSocket error response timed out'));},5000);
  ws.onopen=()=>ws.send(typeof payload==='string'||payload instanceof Uint8Array?payload:JSON.stringify(payload));
  ws.onmessage=event=>{if(typeof event.data!=='string')return;const error=JSON.parse(event.data);if(error.type!=='error')return;
   clearTimeout(timeout);ws.close();if(error.code!==code||!error.message)reject(new Error(JSON.stringify(error)));else resolve(error);};
  ws.onerror=()=>{clearTimeout(timeout);reject(new Error('WebSocket failed'));};
 });
}
const bad=await failure({type:'tiles',batch:{assetId:asset,sequence:7,tiles:[{x:99999,y:99999,z:0}]}},'invalid_tiles');
if(bad.sequence!==7||!bad.message.includes('pixels'))throw new Error(JSON.stringify(bad));
await failure('{','invalid_message');await failure(new Uint8Array([1,2]),'unsupported_message_type');
const missing=await failure({type:'tiles',batch:{assetId:'a'.repeat(64),sequence:8,tiles:[{x:0,y:0,z:0}]}},'asset_not_found');
if(!missing.message.includes('a'.repeat(64)))throw new Error(JSON.stringify(missing));
console.log('PASS: WebSocket malformed JSON, binary message, bounds and unknown image diagnostics');
"""
subprocess.run(['node','--input-type=module','-e',script,a.api,asset],check=True)
report={'websocketCases':4,'passed':len(results),'cases':results,'successTile':'0/0/0','outOfBounds':error}
if a.output:open(a.output,'w',encoding='utf8').write(json.dumps(report,indent=2))
print(json.dumps(report,indent=2))
