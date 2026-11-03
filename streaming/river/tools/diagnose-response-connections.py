"""A/A cached-response diagnostic: fresh versus reused HTTP connections."""
import argparse,hashlib,http.client,importlib.util,io,json,pathlib,statistics,subprocess,time,uuid
from urllib.parse import urlencode,urlsplit
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--container',required=True);p.add_argument('--manifest',type=pathlib.Path,required=True);p.add_argument('--origin',required=True);p.add_argument('--origin-control',required=True);p.add_argument('--samples',type=int,default=40);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();assert a.container.startswith('river-goal-') and a.samples>=20 and not a.output.exists()
spec=importlib.util.spec_from_file_location('fixed',pathlib.Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
raw=a.manifest.read_bytes();manifest=json.loads(raw);origin=json.loads(h.http(a.origin_control+'/__manifest'));assert origin['sha256']==h.sha(raw)
image=json.loads(subprocess.check_output(['docker','inspect',a.container]))[0]['Image'];stable=subprocess.check_output(['docker','image','inspect','river:stable','--format','{{.Id}}'],text=True).strip();assert image==stable
endpoint_api=urlsplit(a.api);assert endpoint_api.scheme=='http','Controlled local HTTP diagnostic only'
def connection():return http.client.HTTPConnection(endpoint_api.hostname,endpoint_api.port,timeout=30)
rows=[];report=dict(passed=False,sameImageControl=True,imageId=image,manifestSha256=h.sha(raw),samplesPerMode=a.samples,scope='One source-cold seed per case, followed by balanced cached responses on the same server and source identity. HTTP connection lifecycle is the only intended variation. Not a backend speedup or cold-load benchmark.',rows=rows)
try:
 for case in manifest['cases']:
  sid='connection-'+uuid.uuid4().hex;endpoint=a.origin+'/'+sid;keep=connection()
  try:
   h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
   query=dict(image=endpoint+'/fixture/'+case['name']+'.jp2',driver=case['driver'],edge='crop',tileSize=case['tileSize'],minZoom=0,maxZoom=case['maxZoom'],tilePixelRatio=1)
   path=f"/streamer/v1/{case['z']}/{case['x']}/{case['y']}/tile.png?"+urlencode(query);body=h.http(a.api+path)
   ref=a.manifest.parent/case['reference'];assert h.sha(ref.read_bytes())==case['referenceSha256'];expected=Image.open(ref).convert('RGB');actual=Image.open(io.BytesIO(body)).convert('RGB');assert actual.size==expected.size and actual.tobytes()==expected.tobytes()
   wire=json.loads(h.http(a.origin_control+'/__stats/'+sid));samples={mode:[] for mode in ['fresh','persistent']}
   # Establish the reusable TCP channel before timing cached samples.
   keep.request('GET',path);warm=keep.getresponse();assert warm.status==200 and warm.read()==body;assert not warm.will_close
   for n in range(a.samples):
    for mode in (['fresh','persistent'] if n%2==0 else ['persistent','fresh']):
     conn=connection() if mode=='fresh' else keep;t=time.perf_counter()
     try:
      conn.request('GET',path,headers={'Connection':'close'} if mode=='fresh' else {});port=conn.sock.getsockname()[1];response=conn.getresponse();headers_at=time.perf_counter();got=response.read();done=time.perf_counter();assert response.status==200 and got==body
      if mode=='persistent':assert not response.will_close
      samples[mode].append(dict(totalMs=(done-t)*1000,headersMs=(headers_at-t)*1000,bodyMs=(done-headers_at)*1000,serverMs=float(response.getheader('x-river-server-ms')),localPort=port))
     finally:
      if mode=='fresh':conn.close()
   assert json.loads(h.http(a.origin_control+'/__stats/'+sid))==wire
   ports={mode:len({s['localPort'] for s in values}) for mode,values in samples.items()};assert ports['persistent']==1 and ports['fresh']>1,ports
   row=dict(case=case['name'],bodyBytes=len(body),seedPixelsExact=True,repeatSourceIoZero=True,connections=ports,median={mode:{k:statistics.median(s[k] for s in values) for k in ['totalMs','headersMs','bodyMs','serverMs']} for mode,values in samples.items()},samples=samples);rows.append(row);print(json.dumps({k:v for k,v in row.items() if k!='samples'}),flush=True)
  finally:keep.close();h.http(a.api+'/api/s3/sources/'+sid,method='DELETE')
 report['passed']=True
finally:a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
