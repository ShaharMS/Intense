"""Controlled cold benchmark: full-suite correctness smoke, focused paired timing.

Only existing River Compose replica slots are accepted. Does not promote releases.
"""
import importlib.util
import argparse,hashlib,io,itertools,json,math,statistics,subprocess,time,urllib.error,urllib.parse,urllib.request,uuid
from pathlib import Path
from PIL import Image

a=argparse.ArgumentParser()
a.add_argument('--smoke-only',action='store_true',help='Check every manifest case on both versions without paired timing or promotion evidence.')
a.add_argument('--manifest',type=Path,help='Explicit frozen fixture manifest; default remains reports/fixed-gate-v2/manifest.json.')
a.add_argument('--capture-stage-logs',action='store_true',help='Capture decode-stage logs after each sample; both containers must enable RIVER_PROFILE_SOURCE. Diagnostic runs are not release timing evidence.')
a.add_argument('--capture-source-reads',action='store_true',help='Enable bounded source-read tracing for a smoke-only diagnostic; never release timing evidence.')
a.add_argument('--common-policy',action='store_true',help='Omit newer indexHeaders settings on both versions for comparisons with stable295.')
a.add_argument('--warm-samples',type=int,default=1,help='Timed exact repeats after each cold response; each must add zero source IO.')
for version in ['baseline','candidate']:a.add_argument('--'+version+'-driver',default='openjpeg',choices=['auto','openjpeg','jp2grok','openhtj2k','nvjpeg2000'])
for version in ['baseline','candidate']:a.add_argument('--'+version+'-serial-transport',default='sdk',choices=['sdk','curl','auto'])
a.add_argument('--compare-grok-pool',action='store_true',help='Same-image JP2Grok configuration experiment; permit only the native shared-pool environment value to differ.')
a.add_argument('--compare-openjpeg-threads',action='store_true',help='Same-image OpenJPEG worker-budget experiment; permit only RIVER_OPENJPEG_THREADS_PER_DECODE to differ.')
a.add_argument('--compare-decode-slots',action='store_true',help='Same-image OpenJPEG decode-admission experiment; permit only RIVER_DECODE_CONCURRENCY to differ.')
a.add_argument('--compare-tokio-workers',action='store_true',help='Same-image runtime scheduling experiment; permit only TOKIO_WORKER_THREADS to differ and record the running process configuration.')
a.add_argument('--compare-index-probes',action='store_true',help='Same-image OpenJPEG experiment; permit only index shard count and probe-byte budget to differ.')
a.add_argument('--source-request-limit',type=int,choices=range(1,65),help='Explicit identical per-source concurrent operation limit.')
a.add_argument('--require-empty-sources',action='store_true',help='Reject leftover configured sources before every cold sample.')
for name in ['baseline-container','baseline-api','candidate-container','candidate-api','output']:a.add_argument('--'+name,required=True)
a.add_argument('--case',action='append',required=True,dest='cases');a.add_argument('--pairs',type=int,default=6);a.add_argument('--read-mode',choices=['adaptive','observe','disabled'],default='adaptive');a.add_argument('--origin-container',default='replica-4');a.add_argument('--origin-control',default='http://127.0.0.1:18085');args=a.parse_args()
assert 6<=args.pairs<=12,'Use6to12pairs;four pairs cannot reach two-sided sign-permutation p<0.05.'
assert 1<=args.warm_samples<=20
assert not args.capture_source_reads or args.smoke_only,'Source tracing is a smoke-only diagnostic, not paired release timing evidence.'
root=Path(__file__).resolve().parents[1];out=Path(args.output).resolve();assert not out.exists()
manifest_path=args.manifest.resolve() if args.manifest else root/'reports/fixed-gate-v2/manifest.json';manifest=json.loads(manifest_path.read_text());all_cases=manifest['cases'];cases=[c for c in all_cases if c['name'] in args.cases];assert len(cases)==len(set(args.cases))
assert len({c['name'] for c in all_cases})==len(all_cases),'Fixture names must be unique.'
for c in all_cases:assert {'name','file','sha256','reference','referenceSha256','z','x','y','tileSize','maxZoom'}<=c.keys(),'Incomplete frozen fixture case.'
sizing_spec=importlib.util.spec_from_file_location('benchmark_sizing',root/'tools/benchmark-fixed.py');sizing_module=importlib.util.module_from_spec(sizing_spec);sizing_spec.loader.exec_module(sizing_module)
versions={v:dict(container=getattr(args,v+'_container'),api=getattr(args,v+'_api').rstrip('/')) for v in ['baseline','candidate']}
inspect={}
for name,v in versions.items():
 inspect[name]=json.loads(subprocess.check_output(['docker','inspect',v['container']],text=True))[0]
 labels=inspect[name]['Config'].get('Labels') or {}
 assert not any(e.startswith('RIVER_PER_VIEWPORT_DECODE_CONCURRENCY=') for e in inspect[name]['Config']['Env']),'Unrecognized sizing variable: use RIVER_VIEWPORT_DECODE_CONCURRENCY'
 assert labels.get('com.docker.compose.project')=='river' and labels.get('com.docker.compose.service') in ['replica-1','replica-2','replica-3','replica-4'],'Mutating benchmarks must use fixed River Compose replica slots.'
assert sum([args.compare_grok_pool,args.compare_index_probes,args.compare_openjpeg_threads,args.compare_tokio_workers,args.compare_decode_slots])<=1
if args.compare_openjpeg_threads:
 assert args.baseline_driver==args.candidate_driver=='openjpeg' and inspect['baseline']['Image']==inspect['candidate']['Image']
 envs={v:dict(x.split('=',1) for x in inspect[v]['Config']['Env']) for v in versions}
 thread_values={v:envs[v].pop('RIVER_OPENJPEG_THREADS_PER_DECODE') for v in versions}
 assert all(1<=int(n)<=64 for n in thread_values.values()) and envs['baseline']==envs['candidate']
elif args.compare_decode_slots:
 assert args.baseline_driver==args.candidate_driver=='openjpeg' and inspect['baseline']['Image']==inspect['candidate']['Image']
 envs={v:dict(x.split('=',1) for x in inspect[v]['Config']['Env']) for v in versions}
 slot_values={v:envs[v].pop('RIVER_DECODE_CONCURRENCY',None) for v in versions}
 assert slot_values['baseline']!=slot_values['candidate'] and envs['baseline']==envs['candidate']
elif args.compare_tokio_workers:
 assert args.baseline_driver==args.candidate_driver=='openjpeg' and inspect['baseline']['Image']==inspect['candidate']['Image']
 envs={v:dict(x.split('=',1) for x in inspect[v]['Config']['Env']) for v in versions}
 runtime_values={v:int(envs[v].pop('TOKIO_WORKER_THREADS')) for v in versions}
 assert all(1<=n<=64 for n in runtime_values.values()) and len(set(runtime_values.values()))==2 and envs['baseline']==envs['candidate']
elif args.compare_index_probes:
 assert args.baseline_driver==args.candidate_driver=='openjpeg' and inspect['baseline']['Image']==inspect['candidate']['Image']
 envs={v:dict(x.split('=',1) for x in inspect[v]['Config']['Env']) for v in versions}
 probe_values={v:{key:envs[v].pop(key,None) for key in ['RIVER_S3_INDEX_SHARDS','RIVER_S3_INDEX_PROBE_BYTES','RIVER_S3_INDEX_LANE_DEPTH']} for v in versions}
 assert envs['baseline']==envs['candidate']
 for values in probe_values.values():assert 1<=int(values['RIVER_S3_INDEX_SHARDS'])<=32 and 65536<=int(values['RIVER_S3_INDEX_PROBE_BYTES'])<=16777216
 for values in probe_values.values():assert values['RIVER_S3_INDEX_LANE_DEPTH'] is None or 1<=int(values['RIVER_S3_INDEX_LANE_DEPTH'])<=128
elif args.compare_grok_pool:
 assert args.baseline_driver==args.candidate_driver=='jp2grok' and inspect['baseline']['Image']==inspect['candidate']['Image']
 envs={v:dict(x.split('=',1) for x in inspect[v]['Config']['Env']) for v in versions}
 pool_values={v:envs[v].pop('RIVER_GROK_NATIVE_THREADS') for v in versions}
 assert all(1<=int(n)<=64 for n in pool_values.values()) and envs['baseline']==envs['candidate']
else:
 assert sorted(inspect['baseline']['Config']['Env'])==sorted(inspect['candidate']['Config']['Env'])
resource_keys=['NanoCpus','CpuQuota','CpuPeriod','CpusetCpus','Memory','MemorySwap']
assert all(inspect['baseline']['HostConfig'][k]==inspect['candidate']['HostConfig'][k] for k in resource_keys)
if args.capture_stage_logs:
 assert all('RIVER_PROFILE_SOURCE=1' in inspect[v]['Config']['Env'] for v in versions)
for c in all_cases:assert hashlib.sha256((manifest_path.parent/c['reference']).read_bytes()).hexdigest()==c['referenceSha256']
ip=subprocess.check_output(['docker','inspect',args.origin_container,'--format','{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}'],text=True).strip()
def http(url,data=None,method=None):
 try:
  with urllib.request.urlopen(urllib.request.Request(url,data=None if data is None else json.dumps(data).encode(),headers={'Content-Type':'application/json'},method=method),timeout=40) as r:return r.read(),dict(r.headers)
 except urllib.error.HTTPError as error:
  body=error.read(8193);error.response_body=body[:8192].decode(errors='replace');error.add_note(error.response_body);raise
def js(url,*rest):return json.loads(http(url,*rest)[0])
def cpu(container):return {k:int(v) for k,v in (line.split() for line in subprocess.check_output(['docker','exec',container,'cat','/sys/fs/cgroup/cpu.stat'],text=True).splitlines())}
rows=[];smoke=[]
report=dict(passed=False,scope=f'Fresh River process and unique S3 connection/object URL per sample. Origin OS cache warm;container persistent state retained. Counter reads outside timed HTTP;small counter-process CPU included. Full{len(all_cases)}case smoke is correctness evidence,not paired performance evidence. No release promotion.',sameBinaryControl=inspect['baseline']['Image']==inspect['candidate']['Image'],readMode=args.read_mode,manifestSha256=hashlib.sha256(manifest_path.read_bytes()).hexdigest(),versions={k:dict(image=v['Image'],resources={n:v['HostConfig'][n] for n in resource_keys},envSha256=hashlib.sha256(json.dumps(sorted(v['Config']['Env'])).encode()).hexdigest()) for k,v in inspect.items()},pairs=args.pairs,smoke=smoke,rows=rows,origin=js(args.origin_control+'/__manifest'))
report['fixtureManifest']=str(manifest_path)
report['fixtureScope']=manifest.get('scope')
def save():
 out.parent.mkdir(parents=True,exist_ok=True)
 temporary=out.with_suffix(out.suffix+'.tmp')
 temporary.write_text(json.dumps(report,indent=2))
 temporary.replace(out)

report['smokeOnly']=args.smoke_only
report['stageProfilingEnabled']=args.capture_stage_logs
report['sourceReadDiagnosticsEnabled']=args.capture_source_reads
if args.capture_source_reads:
 report['promotionEligible']=False
 import atexit
 def disable_source_traces():
  for v in versions.values():
   try:js(v['api']+'/api/diagnostics/source-reads',dict(enabled=False),'PUT')
   except Exception:pass
 atexit.register(disable_source_traces)
report['commonPolicy']=args.common_policy
report['warmSamplesPerColdRequest']=args.warm_samples
report['harnessSha256']=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
drivers={version:getattr(args,version+'_driver') for version in versions}
report['drivers']=drivers
report['sameDriverControl']=len(set(drivers.values()))==1
transports={v:getattr(args,v+'_serial_transport') for v in versions}
report['serialTransports']=transports
if len(set(transports.values()))>1:
 assert inspect['baseline']['Image']==inspect['candidate']['Image'] and report['sameDriverControl'],'Transport controls require the same binary and decoder'
 report['transportComparison']=dict(field='serialTransport',values=transports,codeComparison=False)
report['sourceLifecycle']='Each sample creates one fixture connection and deletes it after measurements; no unrelated connection is removed.'
if args.compare_index_probes:report['configurationComparison']=dict(fields=['RIVER_S3_INDEX_SHARDS','RIVER_S3_INDEX_PROBE_BYTES','RIVER_S3_INDEX_LANE_DEPTH'],values=probe_values,codeComparison=False)
report['requireEmptySourcesBeforeCold']=args.require_empty_sources
if args.compare_grok_pool:report['configurationComparison']=dict(field='RIVER_GROK_NATIVE_THREADS',values=pool_values,codeComparison=False)
if args.compare_openjpeg_threads:report['configurationComparison']=dict(field='RIVER_OPENJPEG_THREADS_PER_DECODE',values=thread_values,codeComparison=False)
if args.compare_decode_slots:report['configurationComparison']=dict(field='RIVER_DECODE_CONCURRENCY',values=slot_values,codeComparison=False)
if args.compare_tokio_workers:report['configurationComparison']=dict(field='TOKIO_WORKER_THREADS',values=runtime_values,codeComparison=False)
format_audit=manifest_path.parent/'format-audit.json'
if format_audit.exists():
 audit=json.loads(format_audit.read_text());assert audit['manifestSha256']==report['manifestSha256']
 assert all(any(c['sha256']==source['sha256'] and c['file']==source['file'] for c in all_cases) for source in audit['sources'])
 report['sourceFormatAudit']=audit
if args.smoke_only:report['pairs']=0
temporary_sources=set()
def sample(c,version,trial,phase):
 v=versions[version];api=v['api'];subprocess.run(['docker','restart',v['container']],check=True,stdout=subprocess.DEVNULL)
 for _ in range(100):
  try:http(api+'/health');break
  except OSError:time.sleep(.1)
 if args.require_empty_sources:assert js(api+'/api/s3/sources')==[], 'Use fresh benchmark instances; sources must be empty before every sample.'
 resolved_sizing=sizing_module.effective_sizing(v['container']);sizing_history=report.setdefault('effectiveStartupSizing',{})
 if version in sizing_history:assert sizing_history[version]==resolved_sizing,('Resolved startup sizing changed across restarts',version)
 sizing_history[version]=resolved_sizing
 if args.compare_tokio_workers:
  code="from pathlib import Path;import json,collections;p=next(p for p in Path('/proc').iterdir() if p.name.isdigit() and (p/'comm').exists() and (p/'comm').read_text().strip()=='river-server');e=dict(s.split('=',1) for s in (p/'environ').read_text().split(chr(0)) if '=' in s);names=collections.Counter((t/'comm').read_text().strip() for t in (p/'task').iterdir());print(json.dumps(dict(configuredWorkers=int(e['TOKIO_WORKER_THREADS']),threadNames=dict(names),processThreads=sum(names.values()))))"
  runtime_snapshot=json.loads(subprocess.check_output(['docker','exec',v['container'],'python3','-c',code],text=True))
  assert runtime_snapshot['configuredWorkers']==runtime_values[version]
  runtime_snapshot['tokioNamedThreads']=sum(n for name,n in runtime_snapshot['threadNames'].items() if name.startswith('tokio-'))
  assert runtime_snapshot['tokioNamedThreads']>=runtime_values[version]
 if args.capture_source_reads:
  js(api+'/api/diagnostics/source-reads',dict(enabled=True),'PUT')
  http(api+'/api/diagnostics/source-reads',method='DELETE')
 if args.capture_stage_logs:
  profile_started=subprocess.check_output(['docker','inspect','--format','{{.State.StartedAt}}',v['container']],text=True).strip()
 policy=dict(mode=args.read_mode,minimumSamples=8,maxOverfetchRatio=2,serialTransport=transports[version])
 if args.source_request_limit is not None:policy['maxSourceRequests']=args.source_request_limit
 if not args.common_policy:policy['indexHeaders']=dict(mode='adaptive',windowBytes=1048576,maxExtraBytes=33554432)
 sid='controlled-'+uuid.uuid4().hex;endpoint=f'http://{ip}:8000/{sid}'
 try:
  js(api+'/api/settings/read-policy',policy,'PUT')
  effective=js(api+'/api/settings/read-policy')['settings']
  public={key:effective[key] for key in policy}
  assert public==policy,('Effective read policy differs from requested control',version,public,policy)
  encoding=js(api+'/api/settings/encoding')
  controls=report.setdefault('publicSettingsControls',{})
  snapshot=dict(readPolicy=public,encoding=encoding)
  if version in controls:assert controls[version]==snapshot,('Public tuning changed across restarts',version)
  controls[version]=snapshot
  if len(controls)==2:
   assert controls['baseline']['encoding']==controls['candidate']['encoding'],'Encoding settings must match'
   left=dict(controls['baseline']['readPolicy']);right=dict(controls['candidate']['readPolicy'])
   if len(set(transports.values()))>1:left.pop('serialTransport');right.pop('serialTransport')
   assert left==right,'Public read-policy settings must match for a code comparison'
  js(api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
  temporary_sources.add((api,sid))
 except Exception as error:
  report['setupFailure']=dict(case=c['name'],version=version,trial=trial,error=repr(error),body=getattr(error,'response_body',None));save();raise
 control=args.origin_control+'/__stats/'+sid;assert not any(js(control)['wire'].values())
 q=dict(image=endpoint+'/fixture/'+c['name']+'.jp2',driver=drivers[version],edge='crop',tileSize=c['tileSize'],minZoom=0,maxZoom=c['maxZoom'],tilePixelRatio=1)
 if drivers[version]=='auto': q.pop('driver')
 url=api+f'/streamer/v1/{c["z"]}/{c["x"]}/{c["y"]}/tile.png?'+urllib.parse.urlencode(q)
 before=cpu(v['container']);t=time.perf_counter()
 try:body,headers=http(url)
 except Exception as e:
  row=dict(case=c['name'],version=version,trial=trial,phase=phase,error=repr(e),errorBody=getattr(e,'response_body',None),failureElapsedMs=(time.perf_counter()-t)*1000);(rows if phase=='timed' else smoke).append(row);save();raise
 ms=(time.perf_counter()-t)*1000;after=cpu(v['container'])
 image=Image.open(io.BytesIO(body));assert 'A' not in image.getbands() or image.getchannel('A').getextrema()==(255,255)
 image=image.convert('RGB');gold=Image.open(manifest_path.parent/c['reference']).convert('RGB');assert image.size==gold.size and image.tobytes()==gold.tobytes()
 wire=js(control)['wire'];repeat_times=[];repeat_phases=[]
 for _ in range(args.warm_samples):
  repeated_at=time.perf_counter()
  with urllib.request.urlopen(urllib.request.Request(url,headers={'Content-Type':'application/json'}),timeout=40) as response:
   headers_at=time.perf_counter();repeat=response.read();finished_at=time.perf_counter();rh=dict(response.headers)
  repeat_times.append((finished_at-repeated_at)*1000)
  repeat_phases.append(dict(headersMs=(headers_at-repeated_at)*1000,bodyMs=(finished_at-headers_at)*1000,wallMs=repeat_times[-1],serverMs=float(rh['x-river-server-ms']),decoderRoute=rh.get('x-river-decoder-route'),responseBytes=len(repeat)))
  assert repeat==body and js(control)['wire']==wire
 observation=js(api+'/api/settings/read-policy')['sources'][sid];reads=js(api+'/api/diagnostics/source-reads')
 if args.capture_source_reads:
  js(api+'/api/diagnostics/source-reads',dict(enabled=False),'PUT')
  assert reads['enabled'] and not reads['summary']['partial'],'Diagnostic trace was disabled or evicted; do not use incomplete evidence.'
  assert reads['summary']['physicalAttempts']==wire['gets'],'Trace sends do not match origin GETs.'
 row=dict(case=c['name'],version=version,trial=trial,phase=phase,wallMs=ms,serverMs=float(headers['x-river-server-ms']),sourceMs=float(headers['x-river-source-cache-ms']),decodeMs=float(headers['x-river-decode-ms']),encodeMs=float(headers['x-river-encode-ms']),wire=wire,cpuDelta={k:after[k]-before.get(k,0) for k in after},pixelsExact=True,opaque=True,repeatIoZero=True,observation=observation,sourceReads=reads)
 row['riverHeaders']={k:v for k,v in headers.items() if k.lower().startswith('x-river-')}
 row['driver']=drivers[version]
 row['sourceId']=sid
 row['responseBytes']=len(body)
 if args.capture_source_reads:
  # Keep the independent origin's timestamps for dependency/overlap analysis.
  # This is outside the measured request and disabled for latency benchmarks.
  origin_snapshot=js(control)
  assert origin_snapshot['wire']==wire
  row['originRanges']=origin_snapshot['ranges']
 if args.compare_tokio_workers:row['runtimeThreadSnapshot']=runtime_snapshot
 if args.capture_stage_logs:
  row['stageLogs']=subprocess.check_output(['docker','logs','--since',profile_started,v['container']],text=True,stderr=subprocess.STDOUT).splitlines()
 row['repeatWallMs']=repeat_times
 row['repeatPhases']=repeat_phases
 http(api+'/api/s3/sources/'+sid,method='DELETE')
 temporary_sources.remove((api,sid))
 (rows if phase=='timed' else smoke).append(row);save();print(json.dumps({k:row[k] for k in ['case','version','trial','phase','wallMs','wire']}),flush=True)
original_policies={}
for api in dict.fromkeys(v['api'] for v in versions.values()):
 policy=js(api+'/api/settings/read-policy')
 if 'configuredSettings' in policy:
  original_policies[api]=policy['configuredSettings']
try:
 for i,c in enumerate(all_cases):
  for version in (['baseline','candidate'] if i%2==0 else ['candidate','baseline']):sample(c,version,-1,'smoke')
 for trial in range(0 if args.smoke_only else args.pairs):
  for c in cases:
   for version in (['baseline','candidate'] if trial%2==0 else ['candidate','baseline']):sample(c,version,trial,'timed')
finally:
 try:
  for api,sid in sorted(temporary_sources):
   http(api+'/api/s3/sources/'+sid,method='DELETE')
 finally:
  for api,policy in original_policies.items():
   js(api+'/api/settings/read-policy',policy,'PUT')
   assert js(api+'/api/settings/read-policy')['configuredSettings']==policy,'Configured policy was not restored'
summary={}
for c in ([] if args.smoke_only else cases):
 name=c['name'];d=[];io_equal=[]
 for trial in range(args.pairs):
  pair={v:next(r for r in rows if r['case']==name and r['trial']==trial and r['version']==v) for v in versions}
  io_equal.append(pair['baseline']['wire']==pair['candidate']['wire'])
  d.append(math.log(pair['candidate']['wallMs']/pair['baseline']['wallMs']))
 observed=abs(statistics.mean(d));null=[abs(statistics.mean(s*x for s,x in zip(signs,d))) for signs in itertools.product([-1,1],repeat=args.pairs)]
 summary[name]=dict(wallMediansMs={v:statistics.median(r['wallMs'] for r in rows if r['case']==name and r['version']==v) for v in versions},pairedRatios=[math.exp(x) for x in d],geometricMeanRatio=math.exp(statistics.mean(d)),twoSidedSignPermutationP=sum(x>=observed-1e-12 for x in null)/len(null),sourceIoEqualByPair=io_equal,sourceWorkloadMatched=all(io_equal),wireVariants={v:[r['wire'] for r in rows if r['case']==name and r['version']==v] for v in versions},interpretation='Evidence only. Equal IO is not proof of a code effect. Adaptive IO variation is part of end-to-end behavior and must be reported separately. Apply unchanged frozen-suite/stable/LTS rules;do not infer release qualification from a median or p-value alone.')
report.update(passed=True,summary=summary);save();print(json.dumps(summary),flush=True)
