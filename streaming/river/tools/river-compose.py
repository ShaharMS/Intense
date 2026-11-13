"""Generate and operate the fixed six-slot local River Compose project."""
import argparse,copy,json,os,subprocess,sys
from pathlib import Path
import yaml

ROOT=Path(__file__).resolve().parents[1]
STATE=ROOT/'deploy/local-stack.json'
SLOTS=['stable','lts',*[f'replica-{n}' for n in range(1,5)]]
NAMES={s:('river-'+s if s in ['stable','lts'] else s) for s in SLOTS}
PORTS=dict(zip(SLOTS,[18081,18082,18080,18083,18084,18085]))
def state():return json.loads(STATE.read_text()) if STATE.exists() else dict(target='replica-1',slots={})
def save(s):STATE.write_text(json.dumps(s,indent=2)+'\n')
def docker(*args):return subprocess.check_output(['docker',*args],cwd=ROOT,text=True,stderr=subprocess.STDOUT)
def compose(*args):return docker('compose','-p','river','-f',str(ROOT/'compose.yaml'),*args)
def audit():
 for attempt in range(3):
  ids=docker('ps','-aq').split()
  try:
   cs=json.loads(docker('inspect',*ids)) if ids else []
   break
  except subprocess.CalledProcessError as error:
   if attempt==2 or 'no such object' not in error.output.lower():raise
 slots=[];strays=[]
 for c in cs:
  name=c['Name'].lstrip('/');labels=c['Config'].get('Labels') or {};service=labels.get('com.docker.compose.service')
  managed=labels.get('com.docker.compose.project')=='river' and service in SLOTS and name==NAMES[service]
  if managed:slots.append(name)
  elif name.startswith('river-goal-') or c['Config']['Image'].startswith('river:') or (labels.get('com.docker.compose.project')=='river' and service in ['river','nvjpeg2000']):strays.append(name)
 assert len(slots)<=6 and not strays,dict(existingSlots=slots,strays=strays)
 return dict(existingBackendSlots=slots,count=len(slots),maximum=6,strays=strays)
def generate():
 cfg=state();target=cfg.get('target','replica-1');assert target in SLOTS
 infra=yaml.safe_load((ROOT/'deploy/compose.infrastructure.yaml').read_text())
 services=copy.deepcopy(infra['services']);volumes=copy.deepcopy(infra.get('volumes',{}))
 for slot in SLOTS:
  key=slot.upper().replace('-','_');spec=cfg.get('slots',{}).get(slot,{})
  image=spec.get('image','river:'+({'stable':'stable','lts':'lts'}.get(slot,'latest')))
  env=dict(RIVER_PROFILE='auto',RIVER_ENABLE_NVIDIA='${RIVER_ENABLE_NVIDIA:-false}',RIVER_S3_CONNECTIONS_FILE='/var/lib/river/config/s3-connections.json',RIVER_S3_CACHE_ROOT='/var/lib/river/s3-cache',RIVER_CORPUS_ROOT='/opt/river/corpus',RIVER_MOSAIC_ROOT='/app/sentinel-date-mosaics',RIVER_ASSET_CATALOG='/app/sentinel-date-mosaics/river-assets.json',RIVER_S3_ADAPTIVE_READS='${RIVER_S3_ADAPTIVE_READS:-observe}',RIVER_S3_SOURCE_MAX_REQUESTS='${RIVER_S3_SOURCE_MAX_REQUESTS:-16}',RIVER_S3_MAX_RETRIES='${RIVER_S3_MAX_RETRIES:-2}',RIVER_TRACE_SOURCE_READS='0',OTEL_EXPORTER_OTLP_ENDPOINT='http://otel-collector:4317',OTEL_SERVICE_NAME='river-server',RIVER_CORS_ORIGINS='${RIVER_'+key+'_CORS_ORIGINS:-${RIVER_CORS_ORIGINS:-*}}',RIVER_OTEL_ENABLED='${RIVER_'+key+'_OTEL_ENABLED:-'+('true' if slot==target else 'false')+'}')
  env.update(spec.get('environment',{}))
  mounts=['river-corpus:/opt/river/corpus:ro','river-corpus:/app/corpus:ro','./corpus/cache/sentinel-date-mosaics:/app/sentinel-date-mosaics:ro','./deploy/local-state/'+slot+'/config:/var/lib/river/config']
  cache='river-s3-cache' if slot=='replica-1' else slot+'-cache';volumes.setdefault(cache,{})
  if spec.get('cache_volume',True):mounts.append(cache+':/var/lib/river/s3-cache')
  if slot.startswith('replica-'):mounts.append('.:/workspace:ro')
  service=dict(image='${RIVER_'+key+'_IMAGE:-'+image+'}',pull_policy='never',container_name=NAMES[slot],cpus=spec.get('cpus','${RIVER_'+key+'_CPUS:-4}'),mem_limit=spec.get('memory','${RIVER_'+key+'_MEMORY:-2g}'),environment=env,ports=['127.0.0.1:${RIVER_'+key+'_PORT:-'+str(PORTS[slot])+'}:8080'],volumes=mounts,labels={'river.backend-slot':slot},logging=dict(driver='json-file',options={'max-size':'10m','max-file':'2'}))
  if slot=='replica-1':service['networks']={'default':{'aliases':['river']}}
  if spec.get('cpuset'):service['cpuset']=spec['cpuset']
  if spec.get('read_only'):service['read_only']=True
  if spec.get('shm_size'):service['shm_size']=spec['shm_size']
  if spec.get('gpus'):service['gpus']=spec['gpus']
  if spec.get('tls_directory'):
   service['volumes'].append(dict(type='bind',source=spec['tls_directory'],target='/run/river-tls',read_only=True))
   service['ports'].append('127.0.0.1:'+str(spec['tls_port'])+':8443')
   if spec.get('tls_udp'):service['ports'].append('127.0.0.1:'+str(spec['tls_port'])+':8443/udp')
   if 'RIVER_TLS_PFX_FILE' not in env and spec.get('tls_pem_defaults',True):
    service['environment'].setdefault('RIVER_TLS_CERT_FILE','/run/river-tls/tls.crt')
    service['environment'].setdefault('RIVER_TLS_KEY_FILE','/run/river-tls/tls.key')
  if spec.get('origin') or spec.get('lab'):
   assert slot.startswith('replica-')
   volumes['benchmark-data']=dict(external=True,name='river-goal-fixed-native-data')
   volumes['indexed-benchmark-data']=dict(external=True,name='river-goal-org780-indexed-data')
   service['volumes']+=['benchmark-data:/data:ro','indexed-benchmark-data:/indexed:ro']
   if spec.get('origin'):
    service['entrypoint']=['python3'];service['command']=['/workspace/'+spec['origin'],'--manifest',spec.get('manifest','/data/reports/fixed-gate-v2/manifest.json'),'--port','8000','--tcp-nodelay','--delay-ms',str(spec.get('delay_ms',14.5)),'--body-bytes-per-second',str(spec.get('rate',300000000))]
    service['command']+=spec.get('origin_args',[])
    service['ports']=['127.0.0.1:${RIVER_'+key+'_PORT:-'+str(PORTS[slot])+'}:8000']
   else:
    (ROOT/'reports/compose-lab'/slot).mkdir(parents=True,exist_ok=True)
    service['volumes']+=['./reports/compose-lab/'+slot+':/work','.:/repo:ro']
    service['entrypoint']=['/bin/sh','-c'];service['command']=['sleep infinity']
   service['healthcheck']={'disable':True}
  if spec.get('kv'):
   assert slot.startswith('replica-') and slot != target
   executable = {'redis':'redis-server', 'valkey':'valkey-server'}[spec['kv']]
   service = dict(image=image, pull_policy='never', container_name=NAMES[slot],
    labels={'river.backend-slot':slot}, entrypoint=[executable],
    command=['--save','','--appendonly','no','--maxmemory','64mb','--maxmemory-policy','allkeys-lfu'],
    tmpfs=['/data:size=32m'], mem_limit='256m')
  if spec.get('telemetry_fixture'):
   assert slot.startswith('replica-') and slot != target
   service = dict(image=image, pull_policy='never', container_name=NAMES[slot],
    labels={'river.backend-slot':slot}, command=['--config=/fixture.yaml'],
    volumes=[spec['telemetry_fixture']+':/fixture.yaml:ro'], mem_limit='256m')
  services[slot]=service
  (ROOT/'deploy/local-state'/slot/'config').mkdir(parents=True,exist_ok=True)
 viewer=services['viewer'];viewer['depends_on']={target:{'condition':'service_started'}}
 viewer['environment']={'RIVER_VIEWER_BACKEND':'${RIVER_VIEWER_BACKEND:-'+target+':8080}','NGINX_ENVSUBST_FILTER':'^RIVER_VIEWER_BACKEND$'}
 viewer['volumes']=['./web:/usr/share/nginx/html:ro','./deploy/nginx/default.conf.template:/etc/nginx/templates/default.conf.template:ro']
 (ROOT/'compose.yaml').write_text('# Generated by tools/river-compose.py; six fixed backend slots.\n'+yaml.safe_dump(dict(name='river',services=services,volumes=volumes),sort_keys=False),encoding='utf-8')
 return cfg
def replace(slots):
 audit()
 # Remove each old slot BEFORE up: Compose recreation otherwise briefly
 # creates a seventh container. Never delete its config/cache volumes.
 compose('stop',*slots);compose('rm','-f',*slots)
 compose('up','-d','--no-deps',*slots);audit()
def main():
 p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='action',required=True)
 sub.add_parser('generate');sub.add_parser('audit')
 up=sub.add_parser('up');up.add_argument('slots',nargs='*')
 target=sub.add_parser('target');target.add_argument('slot',choices=SLOTS);target.add_argument('--runtime',action='store_true',help='Reload viewer routing without restarting backend slots; telemetry selection applies on next Compose up.')
 slot=sub.add_parser('slot');slot.add_argument('slot',choices=SLOTS[2:]);slot.add_argument('--image');slot.add_argument('--cpus');slot.add_argument('--cpuset');slot.add_argument('--memory');slot.add_argument('--env',action='append',default=[]);slot.add_argument('--unset-env',action='append',default=[]);slot.add_argument('--origin');slot.add_argument('--backend',action='store_true');slot.add_argument('--lab',action='store_true');slot.add_argument('--manifest');slot.add_argument('--delay-ms',type=float,default=14.5);slot.add_argument('--rate',type=int,default=300000000)
 a=p.parse_args()
 if a.action=='audit':print(json.dumps(audit(),indent=2));return
 cfg=state()
 if a.action=='slot':
  spec=cfg.setdefault('slots',{}).setdefault(a.slot,{})
  for attr in ['image','cpus','cpuset','memory','manifest']:
   if getattr(a,attr) is not None:spec[attr]=getattr(a,attr)
  spec.setdefault('environment',{}).update(dict(e.split('=',1) for e in a.env))
  for key in a.unset_env:spec['environment'].pop(key,None)
  if a.origin:
   assert a.slot!=cfg.get('target','replica-1'),'Switch viewer target before making that slot an origin'
   path=(ROOT/a.origin).resolve();path.relative_to(ROOT);assert path.is_file();spec.update(origin=path.relative_to(ROOT).as_posix(),delay_ms=a.delay_ms,rate=a.rate)
   spec.pop('lab',None)
  if a.lab:
   assert a.slot!=cfg.get('target','replica-1'),'Switch viewer target before making that slot a lab'
   spec.pop('origin',None);spec['lab']=True
  if a.backend:spec.pop('origin',None);spec.pop('lab',None)
  save(cfg);generate();replace([a.slot])
 elif a.action=='target':
  assert not cfg.get('slots',{}).get(a.slot,{}).get('origin') and not cfg.get('slots',{}).get(a.slot,{}).get('lab'),'Viewer target must be a backend'
  old=cfg.get('target','replica-1');cfg['target']=a.slot;save(cfg);generate()
  if a.runtime:
   text=(ROOT/'deploy/nginx/default.conf.template').read_text().replace('${RIVER_VIEWER_BACKEND}',a.slot+':8080')
   subprocess.run(['docker','compose','-p','river','exec','-T','viewer','sh','-c','cat > /etc/nginx/conf.d/default.conf'],input=text,text=True,cwd=ROOT,check=True)
   compose('exec','-T','viewer','nginx','-t');compose('exec','-T','viewer','nginx','-s','reload')
  else:
   replace(list(dict.fromkeys([old,a.slot])));compose('up','-d','--no-deps','viewer')
 elif a.action=='up':
  assert all(s in SLOTS for s in a.slots),'Unknown backend slot'
  generate();replace(a.slots or SLOTS);compose('up','-d','--no-deps','viewer')
 else:generate();print('Generated compose.yaml')
if __name__=='__main__':main()
