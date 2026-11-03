"""Validate version-folder-independent Compose configuration and volume reuse."""
raise SystemExit('This historical runner creates extra Compose projects and is disabled under the six-slot River budget. Stage baseline/candidate images sequentially in an existing replica with tools/river-compose.py slot; preserve its config/cache. See deploy/LOCAL-COMPOSE.md.')
import argparse,json,os,pathlib,re,shutil,subprocess,urllib.request,uuid
p=argparse.ArgumentParser();p.add_argument('--bundle',type=pathlib.Path,required=True);p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--checkpoint',default='110');p.add_argument('--range-cache');p.add_argument('--require-structural-reads',action='store_true');p.add_argument('--require-single-prefix',action='store_true');p.add_argument('--require-coarse-worker',action='store_true');p.add_argument('--require-exact-support',action='store_true');p.add_argument('--require-aligned-window',action='store_true');p.add_argument('--require-wide-workers',action='store_true',help='On a16+CPU host, verify auto16 then explicit8 with synchronous codec initialization');p.add_argument('--read-policy-mode',choices=['disabled','observe','adaptive']);p.add_argument('--source-request-limit',type=int,choices=range(1,65));p.add_argument('--source-max-retries',type=int,choices=range(0,11));p.add_argument('--serial-transport',choices=['sdk','curl','auto']);p.add_argument('--index-header-window',type=int,choices=[1048576,8388608]);p.add_argument('--baseline-image');p.add_argument('--grok-native-threads',type=int,choices=range(1,65));p.add_argument('--payload-refresh',choices=['true','false'],help='Persist the optional mature payload refresh flag; baseline must support it');a=p.parse_args();assert not a.output.exists();bundle=a.bundle.resolve();uid=uuid.uuid4().hex[:12];project='river-goal-upgrade-m'+a.checkpoint+'-'+uid;root=a.output.resolve().parent/('upgrade-fixture-'+uid);first=root/'version-a';second=root/'version-b';data=root/'shared-data';data.mkdir(parents=True);task_env={k:v for k,v in os.environ.items() if not k.startswith(('RIVER_','COMPOSE_'))};nonce=uuid.uuid4().hex
baseline_image=a.baseline_image or os.environ.get('RIVER_UPGRADE_BASELINE_IMAGE')
if baseline_image:assert re.fullmatch(r'[A-Za-z0-9_./:-]+',baseline_image)
for folder in [first,second]:
 folder.mkdir();shutil.copyfile(bundle/'compose.yaml',folder/'compose.yaml')
 if folder==first and baseline_image:
  target=folder/'compose.yaml';source=target.read_text();lines=[line for line in source.splitlines(keepends=True) if line.startswith('    image: ')];assert len(lines)==1
  target.write_text(source.replace(lines[0],'    image: '+baseline_image+'\n',1))
 (folder/'.env').write_text((f'COMPOSE_PROJECT_NAME={project}\nRIVER_PORT=18614\nRIVER_ENABLE_NVIDIA=false\nRIVER_DATA_DIR={data.as_posix()}\n'+(f'RIVER_S3_RANGE_CACHE_BYTES={a.range_cache}\n' if a.range_cache is not None else '')),encoding='ascii')
if a.grok_native_threads is not None:
 for folder in [first,second]:
  with (folder/'.env').open('a',encoding='ascii') as f:f.write(f'RIVER_GROK_NATIVE_THREADS={a.grok_native_threads}\n')

def compose(folder,*args,envfile=None):
 return subprocess.check_output(['docker','compose','--env-file',str(envfile or folder/'.env'),'-f',str(folder/'compose.yaml'),*args],cwd=folder,env=task_env,text=True,stderr=subprocess.STDOUT)
def request(path,body=None,method=None):
 with urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:18614'+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json'}),timeout=30) as r:return json.load(r)
def defaults(folder):
 envfile=root/'defaults.env';envfile.write_text('RIVER_ENABLE_NVIDIA=false\n',encoding='ascii');cfg=json.loads(compose(folder,'config','--format','json',envfile=envfile));assert cfg['services']['river']['environment'].get('RIVER_S3_RANGE_CACHE_BYTES') is None;return {'project':cfg['name'],'volumes':{k:v['name'] for k,v in cfg['volumes'].items()}}
def check_workers(folder,expected):
 logs=re.sub(r'\x1b\[[0-9;]*m','',compose(folder,'logs','--no-color','river'))
 values=re.findall(r'\bopenjpeg_threads_per_decode=([0-9]+)',logs);assert values and int(values[-1])==expected,values
 code="from pathlib import Path;p=next(p for p in Path('/proc').iterdir() if p.name.isdigit() and (p/'comm').exists() and (p/'comm').read_text().strip()=='river-server');print(next(x for x in (p/'environ').read_text().split(chr(0)) if x.startswith('OPJ_NUM_THREADS=')))"
 assert compose(folder,'exec','-T','river','python3','-c',code).strip()=='OPJ_NUM_THREADS=0'
report={'baselineImageOverride':baseline_image};live=None
try:
 before=defaults(first);after=defaults(second);assert before==after and before['project']=='river-portable',(before,after);report['defaultProject']=before
 compose(first,'up','-d','--wait');live=first
 if a.require_wide_workers:check_workers(first,16)
 if a.require_structural_reads or a.require_single_prefix or a.require_coarse_worker or a.require_exact_support:
  flags=(['RIVER_S3_PRECINCT_ROI','RIVER_S3_MULTIPART_PLT','RIVER_S3_MULTIPART_PREFIX'] if a.require_structural_reads else []) + (['RIVER_S3_SINGLE_PREFIX'] if a.require_single_prefix else []) + (['RIVER_OPENHT_COARSE'] if a.require_coarse_worker else []) + (['RIVER_S3_EXACT_SUPPORT'] if a.require_exact_support else [])
  code="import os,json;print(json.dumps({k:os.getenv(k) for k in "+repr(flags)+"}))"
  actual=json.loads(compose(first,'exec','-T','river','python3','-c',code));assert all(actual[k]=='true' for k in flags),actual;report['structuralReadFlags']=actual
 if a.require_aligned_window:
  actual=compose(first,'exec','-T','river','python3','-c',"import os;print(os.getenv('RIVER_S3_PRECINCT_REBASE'))").strip();assert actual=='auto',actual;report['alignedWindowMode']=actual
 if a.range_cache is not None:
  actual=compose(first,'exec','-T','river','python3','-c',"import os;print(os.getenv('RIVER_S3_RANGE_CACHE_BYTES'))").strip();assert actual==a.range_cache;report['rangeCacheForwarded']=actual
 request('/api/settings/encoding',{'webpMethod':4},'PUT')
 if a.serial_transport or a.read_policy_mode or a.source_request_limit or a.index_header_window or a.payload_refresh is not None:
  policy=request('/api/settings/read-policy')['settings']
  if a.serial_transport:policy['serialTransport']=a.serial_transport
  if a.read_policy_mode:policy['mode']=a.read_policy_mode
  if a.source_request_limit:policy['maxSourceRequests']=a.source_request_limit
  if a.index_header_window:policy['indexHeaders']={'mode':'adaptive','windowBytes':a.index_header_window,'maxExtraBytes':33554432}
  if a.payload_refresh is not None:policy['refreshPayloadCosts']=a.payload_refresh=='true'
  request('/api/settings/read-policy',policy,'PUT')
 source=dict(id='upgrade-proof',name='Upgrade proof',bucket='fixture',region='us-east-1',endpoint='http://host.docker.internal:9',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True)
 if a.source_max_retries is not None:source['maxRetries']=a.source_max_retries
 request('/api/s3/sources',source)
 compose(first,'exec','-T','river','python3','-c',"from pathlib import Path;Path('/var/lib/river/s3-cache/upgrade-proof.txt').write_text("+repr(nonce)+")")
 compose(first,'down');live=None
 if a.require_wide_workers:
  with (second/'.env').open('a',encoding='ascii') as f:f.write('RIVER_OPENJPEG_THREADS_PER_DECODE=8\n')
 compose(second,'up','-d','--wait');live=second
 if a.require_wide_workers:
  check_workers(second,8);report['openjpegWorkers']={'auto':16,'explicitOverride':8,'initialPoolThreads':0}
 if a.grok_native_threads is not None:
  code="from pathlib import Path;p=next(p for p in Path('/proc').iterdir() if p.name.isdigit() and (p/'comm').exists() and (p/'comm').read_text().strip()=='river-server');print(next(x for x in (p/'environ').read_text().split(chr(0)) if x.startswith('RIVER_GROK_NATIVE_THREADS=')))"
  assert compose(second,'exec','-T','river','python3','-c',code).strip()=='RIVER_GROK_NATIVE_THREADS='+str(a.grok_native_threads)
  report['nativeGrokPoolForwarded']=a.grok_native_threads
 encoding=request('/api/settings/encoding');assert encoding['webpMethod']==4,encoding
 if a.serial_transport or a.read_policy_mode or a.source_request_limit or a.index_header_window or a.payload_refresh is not None:
  restored=request('/api/settings/read-policy')['settings']
  if a.index_header_window:assert restored['indexHeaders']=={'mode':'adaptive','windowBytes':a.index_header_window,'maxExtraBytes':33554432};report['indexHeaderPolicyPersisted']=restored['indexHeaders']
  if a.payload_refresh is not None:assert restored['refreshPayloadCosts']==(a.payload_refresh=='true');report['payloadRefreshPersisted']=restored['refreshPayloadCosts']
  if a.serial_transport:assert restored['serialTransport']==a.serial_transport;report['serialTransportPersisted']=a.serial_transport
  if a.read_policy_mode:assert restored['mode']==a.read_policy_mode;report['readPolicyModePersisted']=a.read_policy_mode
  if a.source_request_limit:assert restored['maxSourceRequests']==a.source_request_limit;report['sourceRequestLimitPersisted']=a.source_request_limit
 sources=request('/api/s3/sources');assert len(sources)==1 and sources[0]['id']=='upgrade-proof',sources
 assert 'accessKeyId' not in sources[0] and 'secretAccessKey' not in sources[0]
 if a.source_max_retries is not None:assert sources[0]['maxRetries']==a.source_max_retries;report['sourceMaxRetriesPersisted']=a.source_max_retries
 saved=compose(second,'exec','-T','river','python3','-c',"from pathlib import Path;print(Path('/var/lib/river/s3-cache/upgrade-proof.txt').read_text())").strip();assert saved==nonce
 report.update(projectOverride=project,folderChange=True,encodingPersisted=True,s3ConnectionPersisted=True,credentialsNotReturned=True,cacheVolumePersisted=True,scope='Two separate version folders, unique task-owned Compose project, stop/recreate without deleting volumes; actual settings/API restore plus cache marker. Default name checked read-only; no existing user project touched.',passed=True)
 a.output.write_text(json.dumps(report,indent=2),encoding='utf8');print(json.dumps(report))
finally:
 if live:compose(live,'down')
 for path in [first/'.env',second/'.env',root/'defaults.env']:
  if path.exists():path.unlink()
