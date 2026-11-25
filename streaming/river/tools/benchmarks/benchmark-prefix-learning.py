"""Paired source-trained/object-cold prefix benchmark; training is real serving IO.

No observations are injected. Registration must do zero IO. The warmup is a
different S3 key with the same pinned synthetic bytes; every GET/byte is retained
in the report, separately from the target timer. Original files are read-only.
"""
import argparse,hashlib,importlib.util,io,json,statistics,subprocess,time,uuid
from pathlib import Path
from urllib.parse import urlencode
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--baseline-api',required=True);p.add_argument('--candidate-api',required=True)
p.add_argument('--baseline-container',required=True);p.add_argument('--candidate-container',required=True)
p.add_argument('--origin',required=True);p.add_argument('--origin-control',required=True)
p.add_argument('--workload',type=Path,required=True);p.add_argument('--output',type=Path,required=True)
p.add_argument('--ratio',type=int,choices=[2,4],default=2);p.add_argument('--warmups',type=int,choices=[0,1],default=1)
p.add_argument('--rounds',type=int,default=4);p.add_argument('--source-cap',type=int,default=16)
p.add_argument('--training',choices=['preview','metadata'],default='preview')
a=p.parse_args();assert not a.output.exists();assert a.rounds>=4 and a.rounds%2==0
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'))
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
w=json.loads(a.workload.read_text());source=(a.workload.parent/w['sourceFile']).resolve();ref=(a.workload.parent/w['referenceFile']).resolve()
assert hashlib.sha256(source.read_bytes()).hexdigest()==w['case']['sha256']
assert hashlib.sha256(ref.read_bytes()).hexdigest()==w['case']['referenceSha256']
reference=Image.open(ref).convert('RGB');origin=json.loads(h.http(a.origin_control+'/__manifest'))
assert origin['sha256']==w['originManifestSha256']
versions={};original_policies={};report=dict(passed=False,workloadSha256=h.sha(a.workload.read_bytes()),workload=w,
 origin=origin,settings=dict(mode='adaptive',minimumSamples=8,maxOverfetchRatio=a.ratio,maxSourceRequests=a.source_cap),
 warmups=a.warmups,training=a.training,rows=[])
for version in ['baseline','candidate']:
 container=getattr(a,version+'_container');assert container.startswith('river-goal-')
 cfg=json.loads(subprocess.check_output(['docker','inspect',container]))[0]
 versions[version]=dict(container=container,api=getattr(a,version+'_api'),imageId=cfg['Image'],environmentSha256=h.sha(json.dumps(sorted(cfg['Config']['Env'])).encode()),resources={k:cfg['HostConfig'].get(k) for k in ['Memory','NanoCpus','CpuQuota','CpuPeriod','CpusetCpus']})
 original_policies[version]=json.loads(h.http(versions[version]['api']+'/api/settings/read-policy'))['settings']
assert versions['baseline']['imageId']==subprocess.check_output(['docker','image','inspect','river:stable','--format','{{.Id}}'],text=True).strip()
assert versions['baseline']['environmentSha256']==versions['candidate']['environmentSha256']
assert versions['baseline']['resources']==versions['candidate']['resources']
report['versions']=versions
def wire(sid):return json.loads(h.http(a.origin_control+'/__stats/'+sid))['wire']
def delta(after,before):return {k:after[k]-before[k] for k in after}
def url(api,endpoint,name):
 c=w['case'];return api+f"/streamer/v1/{c['z']}/0/0/tile.png?"+urlencode(dict(image=endpoint+'/fixture/'+name+'.jp2',driver='openjpeg',edge='crop',tileSize=c['tileSize'],maxZoom=c['maxZoom'],minZoom=0,tilePixelRatio=1))
def pixels(body):
 actual=Image.open(io.BytesIO(body)).convert('RGB');assert actual.size==reference.size and actual.tobytes()==reference.tobytes()
try:
 for trial in range(a.rounds):
  for version in (['baseline','candidate'] if trial%2==0 else ['candidate','baseline']):
   v=versions[version];api=v['api'];subprocess.run(['docker','restart',v['container']],check=True,stdout=subprocess.DEVNULL)
   for attempt in range(100):
    try:h.http(api+'/api/drivers');break
    except OSError:time.sleep(.1)
   else:raise RuntimeError('API did not become ready')
   policy=dict(original_policies[version],**report['settings']);h.http(api+'/api/settings/read-policy',policy,'PUT')
   sid='prefix-learning-'+uuid.uuid4().hex;endpoint=a.origin+'/'+sid
   h.http(api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
   try:
    assert wire(sid)==dict(gets=0,heads=0,bytes=0),'Registration performed IO'
    started=time.perf_counter()
    if a.warmups:
     if a.training=='preview':pixels(h.http(url(api,endpoint,w['warmup']['name'])))
     else:
      for key in w['metadataWarmupKeys']:h.http(api+'/api/s3/sources/'+sid+'/assets',dict(key=key+'.jp2'),'POST')
    training_ms=(time.perf_counter()-started)*1000;before=wire(sid)
    trained=json.loads(h.http(api+'/api/settings/read-policy'))['sources'].get(sid)
    target=url(api,endpoint,w['case']['name']);started=time.perf_counter();body,headers=h.http(target,include_headers=True);elapsed=(time.perf_counter()-started)*1000
    cold_wire=wire(sid);pixels(body)
    started=time.perf_counter();assert h.http(target)==body;repeat_ms=(time.perf_counter()-started)*1000
    keepalive=h.keepalive_repeat(target,body);assert wire(sid)==cold_wire,'Exact repeat performed source IO'
    observation=json.loads(h.http(api+'/api/settings/read-policy'))['sources'].get(sid)
    origin_pressure=json.loads(h.http(a.origin_control+'/__stats/'+sid)).get('pressure')
    row=dict(version=version,trial=trial,coldMs=elapsed,repeatMs=repeat_ms,repeatKeepAlive=keepalive,trainingMs=training_ms,trainingWire=before,targetWire=delta(cold_wire,before),totalWire=cold_wire,sourceBeforeTarget=trained,sourceAfterTarget=observation,pixelsExact=True,repeatIoZero=True,headers={k:v for k,v in headers.items() if k.lower().startswith('x-river-')})
    row['originPressure']=origin_pressure
    report['rows'].append(row);print(json.dumps({k:row[k] for k in ['version','trial','coldMs','trainingWire','targetWire']}),flush=True)
   finally:h.http(api+'/api/s3/sources/'+sid,method='DELETE')
 report['summary']={v:dict(coldMedianMs=statistics.median(r['coldMs'] for r in report['rows'] if r['version']==v),trainingMedianMs=statistics.median(r['trainingMs'] for r in report['rows'] if r['version']==v),targetGets=sorted({r['targetWire']['gets'] for r in report['rows'] if r['version']==v}),targetBytes=sorted({r['targetWire']['bytes'] for r in report['rows'] if r['version']==v})) for v in versions}
 report['passed']=True
except Exception as error:
 report['failure']=str(error)
 raise
finally:
 for version,v in versions.items():h.http(v['api']+'/api/settings/read-policy',original_policies[version],'PUT')
 report['originNetworkEnd']=json.loads(h.http(a.origin_control+'/__netstat'));report['originNetworkDelta']={k:report['originNetworkEnd'][k]-origin['networkCounters'][k] for k in report['originNetworkEnd']}
 healthy=report['originNetworkDelta']['ListenDrops']==0 and report['originNetworkDelta']['ListenOverflows']==0
 if not healthy:report['passed']=False;report['failure']='Origin dropped connections; reject timings'
 a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
 assert healthy,'Origin dropped connections; reject timings'
