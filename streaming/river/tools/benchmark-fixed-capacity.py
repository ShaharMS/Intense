"""Capacity qualification on a case from the frozen manifest; not a new speedup case."""
import argparse,concurrent.futures,hashlib,importlib.util,io,json,statistics,subprocess,threading,time,uuid
from pathlib import Path
from http.server import ThreadingHTTPServer
from urllib.parse import urlencode
from PIL import Image
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
p=argparse.ArgumentParser();p.add_argument('--manifest',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--baseline-channel',choices=['stable','lts'],default='stable');p.add_argument('--capture-transports',action='store_true',help='Read serial transport counters after both timed phases')
for v in ['baseline','candidate']:
 p.add_argument('--'+v+'-api',required=True);p.add_argument('--'+v+'-container',required=True)
p.add_argument('--same-image-control',action='store_true');p.add_argument('--decode-concurrency-control',action='store_true',help='Same-current-stable-image experiment varying only RIVER_DECODE_CONCURRENCY');p.add_argument('--shared-placement',action='store_true');p.add_argument('--placement-port',type=int,default=18884);p.add_argument('--external-origin');p.add_argument('--external-origin-control');p.add_argument('--clients',type=int,default=8);p.add_argument('--rounds',type=int,default=3);p.add_argument('--case',required=True);a=p.parse_args()
assert 1<=a.clients<=16 and 1<=a.rounds<=4
assert bool(a.external_origin)==bool(a.external_origin_control)
assert not a.output.exists(),'Report already exists; preserve benchmark evidence'
assert a.baseline_container!=a.candidate_container,'Use separate test instances'
raw=a.manifest.read_bytes();manifest=json.loads(raw);case=next(c for c in manifest['cases'] if c['name']==a.case);source=(a.manifest.parent/case['file']).resolve()
with source.open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==case['sha256']
reference=(a.manifest.parent/case['reference']).read_bytes();assert h.sha(reference)==case['referenceSha256'];gold=Image.open(io.BytesIO(reference)).convert('RGB')
files={case['name']:source};counters={};lock=threading.Lock();server=None;origin_description=None
if a.external_origin:
 origin_description=json.loads(h.http(a.external_origin_control.rstrip('/')+'/__manifest'))
 probe="import json,sys,urllib.request;print(json.dumps(json.load(urllib.request.urlopen(sys.argv[1]+'/__manifest',timeout=3))))"
 reachable=json.loads(subprocess.check_output(['docker','exec',a.baseline_container,'python3','-c',probe,a.external_origin.rstrip('/')],text=True))
 assert reachable['sha256']==origin_description['sha256'] and reachable.get('responseDelayMs')==origin_description.get('responseDelayMs'),'Fixture endpoint/control mismatch' 
 assert origin_description['sha256']==h.sha(raw) and origin_description['files'][case['name']]==case['sourceBytes']
else:
 origin=h.make_origin(files,[case],dict(cases=[case]),counters,lock)
 class CapacityOrigin(ThreadingHTTPServer):request_queue_size=256
 server=CapacityOrigin(('0.0.0.0',18992),origin);threading.Thread(target=server.serve_forever,daemon=True).start()
def source_wire(sid):
 if a.external_origin:return json.loads(h.http(a.external_origin_control.rstrip('/')+'/__stats/'+sid))['wire']
 return dict(counters[sid])
def endpoint(sid):return (a.external_origin.rstrip('/') if a.external_origin else 'http://host.docker.internal:18992')+'/'+sid
versions={v:dict(api=getattr(a,v+'_api').rstrip('/'),container=getattr(a,v+'_container')) for v in ['baseline','candidate']}
inspections={}
for v in versions.values():
 assert v['container'].startswith('river-goal-')
 inspections[v['container']]=json.loads(subprocess.check_output(['docker','inspect',v['container']],text=True))[0]
 v['imageId']=inspections[v['container']]['Image']
 v['environmentSha256']=h.sha(json.dumps(sorted(inspections[v['container']]['Config']['Env'])).encode())
 v['effectiveSizing']=h.effective_sizing(v['container'])
 v['readPolicy']=json.loads(h.http(v['api']+'/api/settings/read-policy'))['settings']
configuration_changes=[]
if a.decode_concurrency_control:
 assert not a.same_image_control and a.shared_placement
 assert versions['baseline']['imageId']==versions['candidate']['imageId'],'Configuration experiment requires identical images'
 envs=[dict(item.split('=',1) for item in inspections[getattr(a,v+'_container')]['Config']['Env']) for v in ['baseline','candidate']]
 configuration_changes=sorted(k for k in envs[0].keys()|envs[1].keys() if envs[0].get(k)!=envs[1].get(k))
 assert configuration_changes==['RIVER_DECODE_CONCURRENCY'],'Only decode concurrency may vary in this control'
 assert versions['baseline']['effectiveSizing']['decode_concurrency']!=versions['candidate']['effectiveSizing']['decode_concurrency'],'No-op experiment: container profiles resolved both decode limits to the same value'
else:assert versions['baseline']['environmentSha256']==versions['candidate']['environmentSha256'],'Capacity comparison requires identical server environments'
assert all(inspections[a.baseline_container]['HostConfig'].get(k)==inspections[a.candidate_container]['HostConfig'].get(k) for k in ['Memory','NanoCpus','CpuQuota','CpuPeriod','CpusetCpus']),'Capacity resource limits differ'
assert versions['baseline']['imageId']==subprocess.check_output(['docker','image','inspect','river:'+a.baseline_channel,'--format','{{.Id}}'],text=True).strip()
if a.same_image_control:
 assert a.shared_placement,'A/A capacity controls require the fixed placement slot'
 versions['candidate']['imageId']=versions['baseline']['imageId']
 versions['candidate']['readPolicy']=versions['baseline']['readPolicy']
rows=[];report=dict(passed=False,runnerSha256=h.sha(Path(__file__).read_bytes()),manifestSha256=h.sha(raw),originRunnerSha256=h.sha(Path(h.__file__).read_bytes()),case=a.case,clients=a.clients,versions=versions,rows=rows,originEndpoint=a.external_origin,originControl=origin_description,sameImageControl=a.same_image_control,measurementProtocolVersion=3 if a.shared_placement else 2,sharedPlacement=a.shared_placement,batchIncludesRepeatsAndPixelValidation=False,scope=f'{a.clients} simultaneous process-cold sources, then a separate concurrent exact-repeat phase; PNG/pixel validation outside both timers; identical frozen deep tile, same explicitly recorded Range origin (Windows-host when no external origin is supplied); per-cohort restart. Cgroup peak includes file cache.');started=time.monotonic()
report['decodeConcurrencyControl']=a.decode_concurrency_control;report['configurationChanges']=configuration_changes
report['baselineChannel']=a.baseline_channel
placement_name='river-goal-fixed-capacity-slot';placement_ip=None
if a.shared_placement:
 assert not subprocess.run(['docker','inspect',placement_name],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0,'Existing placement slot requires inspection before reuse'
 for inspect in inspections.values():
  assert not inspect['Mounts'] and not inspect['Config'].get('Volumes'),'Shared placement requires fixture-only containers without data mounts'
  assert not any(inspect['HostConfig'].get(k) for k in ['Memory','NanoCpus','CpuQuota','CpuPeriod','CpusetCpus']),'Shared placement currently requires default resource limits'
def stop_placement():
 result=subprocess.run(['docker','inspect',placement_name],capture_output=True,text=True)
 if result.returncode:return
 cfg=json.loads(result.stdout)[0];assert cfg['Config']['Labels'].get('river.benchmark.placement')=='fixed-capacity-v1'
 subprocess.run(['docker','stop',placement_name],check=True,stdout=subprocess.DEVNULL);subprocess.run(['docker','rm',placement_name],check=True,stdout=subprocess.DEVNULL)
def start_placement(v):
 global placement_ip
 args=['docker','run','-d','--name',placement_name,'--label','river.benchmark.placement=fixed-capacity-v1','-p',f'127.0.0.1:{a.placement_port}:8080']
 for item in inspections[v['container']]['Config']['Env']:args+=['-e',item]
 subprocess.run(args+[v['imageId']],check=True,stdout=subprocess.DEVNULL)
 cfg=json.loads(subprocess.check_output(['docker','inspect',placement_name],text=True))[0]
 ip=cfg['NetworkSettings']['Networks']['bridge']['IPAddress']
 assert placement_ip is None or placement_ip==ip,'Placement address changed; reject this comparison'
 placement_ip=ip
 return dict(v,api=f'http://127.0.0.1:{a.placement_port}',container=placement_name,placementIp=ip)
def request_cold(v,sid,barrier):
 query=dict(image=endpoint(sid)+f'/fixture/{case["name"]}.jp2',driver=case['driver'],edge='crop',tileSize=case['tileSize'],minZoom=0,maxZoom=case['maxZoom'],tilePixelRatio=1)
 url=v['api']+f'/streamer/v1/{case["z"]}/{case["x"]}/{case["y"]}/tile.png?'+urlencode(query)
 barrier.wait(timeout=30);t=time.perf_counter();body,headers=h.http(url,include_headers=True);cold=(time.perf_counter()-t)*1000
 return dict(sid=sid,url=url,body=body,coldMs=cold,responseTimingHeaders={k:value for k,value in headers.items() if k.lower().startswith('x-river-')})
def request_repeat(job,barrier):
 barrier.wait(timeout=30);t=time.perf_counter();body=h.http(job['url']);warm=(time.perf_counter()-t)*1000
 return body,warm
try:
 for trial in range(a.rounds):
  for label in (['baseline','candidate'] if trial%2==0 else ['candidate','baseline']):
   assert time.monotonic()-started<180
   v=versions[label]
   if a.shared_placement:v=start_placement(v)
   else:subprocess.run(['docker','restart',v['container']],check=True,stdout=subprocess.DEVNULL,timeout=20)
   for _ in range(100):
    try:h.http(v['api']+'/api/drivers');break
    except OSError:time.sleep(.1)
   else:raise RuntimeError('Readiness timeout')
   if a.shared_placement:h.http(v['api']+'/api/settings/read-policy',v['readPolicy'],'PUT')
   actual_policy=json.loads(h.http(v['api']+'/api/settings/read-policy'))['settings']
   assert actual_policy==v['readPolicy'],'Runtime read policy changed or was lost when recreating the placement slot'
   sources=[]
   try:
    for _ in range(a.clients):
     sid='capacity-'+uuid.uuid4().hex;sources.append(sid);counters[sid]=dict(gets=0,heads=0,bytes=0)
     h.http(v['api']+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint(sid),accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST');assert not any(source_wire(sid).values())
    with concurrent.futures.ThreadPoolExecutor(max_workers=a.clients) as pool:
     barrier=threading.Barrier(a.clients+1);pending=[pool.submit(request_cold,v,sid,barrier) for sid in sources]
     t=time.perf_counter();barrier.wait(timeout=30);cold_jobs=[job.result() for job in pending];elapsed=(time.perf_counter()-t)*1000
     # No repeats or pixel-validation CPU may compete with unfinished cold IO.
     wires=[source_wire(job['sid']) for job in cold_jobs]
     gold_bytes=gold.tobytes()
     for job in cold_jobs:
      im=Image.open(io.BytesIO(job['body'])).convert('RGB');assert im.size==gold.size and im.tobytes()==gold_bytes
     barrier=threading.Barrier(a.clients+1);pending=[pool.submit(request_repeat,job,barrier) for job in cold_jobs]
     t=time.perf_counter();barrier.wait(timeout=30);warm_jobs=[job.result() for job in pending];repeat_elapsed=(time.perf_counter()-t)*1000
     jobs=[]
     for job,wire,(repeat,warm) in zip(cold_jobs,wires,warm_jobs):
      assert repeat==job['body'] and source_wire(job['sid'])==wire
      jobs.append(dict(sourceId=job['sid'],coldMs=job['coldMs'],repeatMs=warm,wire=wire,pixelsExact=True,repeatIoZero=True,responseTimingHeaders=job['responseTimingHeaders']))
    if a.capture_transports:
     observations=json.loads(h.http(v['api']+'/api/settings/read-policy'))['sources']
     for job in jobs:
      current=observations.get(job['sourceId'],{});job['nativeSerialRequests']=current.get('nativeSerialRequests');job['sdkSerialReads']=current.get('sdkSerialReads')
    peak=int(subprocess.check_output(['docker','exec',v['container'],'cat','/sys/fs/cgroup/memory.peak'],text=True))
    row=dict(version=label,trial=trial,readPolicy=actual_policy,placementIp=v.get('placementIp'),imageId=v['imageId'],batchMs=elapsed,coldBatchMs=elapsed,repeatBatchMs=repeat_elapsed,peakMemoryBytes=peak,requests=jobs);rows.append(row);print(json.dumps({k:value for k,value in row.items() if k!='requests'}),flush=True)
   finally:
    for sid in sources:h.http(v['api']+'/api/s3/sources/'+sid,method='DELETE')
    if a.shared_placement:stop_placement()
 if a.external_origin and origin_description.get('networkCounters') is not None:
  report['originNetworkEnd']=json.loads(h.http(a.external_origin_control.rstrip('/')+'/__netstat'))
  report['originNetworkDelta']={k:report['originNetworkEnd'][k]-origin_description['networkCounters'][k] for k in report['originNetworkEnd']}
  assert report['originNetworkDelta']['ListenDrops']==0 and report['originNetworkDelta']['ListenOverflows']==0,'Fixture origin dropped connections; reject capacity timings'
 report['summary']={v:dict(batchMedianMs=statistics.median(r['coldBatchMs'] for r in rows if r['version']==v),coldBatchMedianMs=statistics.median(r['coldBatchMs'] for r in rows if r['version']==v),repeatBatchMedianMs=statistics.median(r['repeatBatchMs'] for r in rows if r['version']==v),peakMemoryBytes=max(r['peakMemoryBytes'] for r in rows if r['version']==v)) for v in versions};report['passed']=True
finally:
 if a.shared_placement:stop_placement()
 report['elapsedSeconds']=time.monotonic()-started;a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2)+'\n')
 if server:server.shutdown()
