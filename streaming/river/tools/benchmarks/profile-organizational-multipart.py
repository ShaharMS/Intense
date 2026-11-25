"""One source-cold preview per synthetic multipart fixture; no promotion gate."""
import argparse,hashlib,importlib.util,io,json,socket,threading,time,uuid
from pathlib import Path
from http.server import ThreadingHTTPServer
from urllib.parse import urlencode
from urllib.request import Request,urlopen
from urllib.error import HTTPError
from PIL import Image,ImageFile

p=argparse.ArgumentParser();p.add_argument('--api',required=True)
p.add_argument('--fixtures',type=Path,required=True);p.add_argument('--output',type=Path,required=True)
p.add_argument('--origin-host',default='host.docker.internal')
p.add_argument('--reduction',type=int,action='append')
p.add_argument('--driver',default='openjpeg')
p.add_argument('--fixture-manifest',default='manifest.json')
p.add_argument('--record-ranges',action='store_true',help='Retain complete synthetic-origin attempts; diagnostic timing only')
p.add_argument('--inspect-structure',action='store_true',help='Open metadata first and verify opt-in diagnostics add no source IO; timings are diagnostic only')
a=p.parse_args();assert not a.output.exists()
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'))
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
manifest=json.loads((a.fixtures/a.fixture_manifest).read_text(encoding='utf-8'))
files={f['name']:a.fixtures/f['file'] for f in manifest['fixtures']}
for f in manifest['fixtures']:
 assert hashlib.sha256(files[f['name']].read_bytes()).hexdigest()==f['sha256'],'Synthetic source changed'
counters={};range_events={};lock=threading.Lock();base=h.make_origin(files,manifest['fixtures'],{'cases':manifest['fixtures']},counters,lock)
class Handler(base):
 def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
 def do_GET(self):
  if not a.record_ranges:return super().do_GET()
  sid=self.path.strip('/').split('/')[0]
  event=dict(range=self.headers.get('Range'),started=time.perf_counter())
  with lock:range_events.setdefault(sid,[]).append(event)
  try:return super().do_GET()
  finally:event['originHandlerMs']=(time.perf_counter()-event['started'])*1000
server=ThreadingHTTPServer(('0.0.0.0',18997),Handler)
threading.Thread(target=server.serve_forever,daemon=True).start()
report={'diagnosticOnly':True,'rows':[]}
try:
 for f,reduction in [(f,r) for f in manifest['fixtures'] for r in (a.reduction or [6])]:
  sid='multipart-'+uuid.uuid4().hex;endpoint='http://'+a.origin_host+':18997/'+sid
  h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
  try:
   structure=None
   if a.inspect_structure:
    loaded=json.loads(h.http(a.api+'/api/s3/sources/'+sid+'/assets',dict(key=f['name']+'.jp2'),'POST'))
    manifest_url=a.api+'/api/assets/'+loaded['assetId']+'/manifest'
    before=dict(counters[sid]);plain=json.loads(h.http(manifest_url));assert 'codestreamStructure' not in plain
    structure=json.loads(h.http(manifest_url+'?includeStructure=true'))['codestreamStructure']
    assert counters[sid]==before,'Structure inspection fetched source bytes'
    assert structure['inspectionDoesNotFetchSource']
    if 'style' in f:assert structure['coding']['codeblockStyle']==f['style']
   url=a.api+f'/streamer/v1/{6-reduction}/0/0/tile.png?'+urlencode(dict(image=endpoint+'/fixture/'+f['name']+'.jp2',driver=a.driver,edge='crop',tileSize=4096,maxZoom=6,minZoom=0,tilePixelRatio=1))
   start=time.perf_counter();row=dict(fixture=f['name'],sha256=f['sha256'],reduction=reduction,driver=a.driver)
   try:
    with urlopen(url,timeout=180) as r:body=r.read();row.update(status=r.status,headers=dict(r.headers))
    row['wallMs']=(time.perf_counter()-start)*1000
    reference=Image.open(files[f['name']]);scale=1<<reduction;reference._size=tuple((n+scale-1)//scale for n in reference.size)
    t=reference.tile[0];args=list(t.args);args[1]=reduction
    reference.tile=[ImageFile._Tile(t.codec_name,(0,0,*reference.size),t.offset,tuple(args))]
    ImageFile.ImageFile.load(reference)
    actual=Image.open(io.BytesIO(body)).convert('RGB');reference=reference.convert('RGB')
    row['pixelsExact']=actual.size==reference.size and actual.tobytes()==reference.tobytes()
    before=dict(counters[sid]);row['repeatExact']=h.http(url)==body;row['repeatIoZero']=before==counters[sid]
   except HTTPError as e:row.update(status=e.code,error=e.read().decode(errors='replace'),wallMs=(time.perf_counter()-start)*1000)
   row['origin']=dict(counters.get(sid,{}))
   if structure is not None:row['codestreamStructure']=structure;row['structureIoZero']=True
   if a.record_ranges:
    row['originRanges']=list(range_events.get(sid,[]))
    assert len(row['originRanges'])==row['origin']['gets'],'Origin attempt log incomplete'
    policy=json.loads(h.http(a.api+'/api/settings/read-policy'))
    row['readPolicy']=policy['settings'];row['sourceObservation']=policy['sources'].get(sid)
   report['rows'].append(row)
  finally:h.http(a.api+'/api/s3/sources/'+sid,method='DELETE')
finally:
 report['passed']=bool(report['rows']) and all(r.get('pixelsExact') and r.get('repeatExact') and r.get('repeatIoZero') for r in report['rows'])
 server.shutdown();a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps(dict(passed=report['passed'],output=str(a.output),rows=[dict(fixture=r['fixture'],
 reduction=r['reduction'],status=r['status'],pixelsExact=r.get('pixelsExact'),
 origin=r.get('origin'),recordedRanges=len(r.get('originRanges',[]))) for r in report['rows']]),indent=2))
if not report['passed']:raise SystemExit(1)
