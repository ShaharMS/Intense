"""Exercise real runtime sampling on a bounded repeat-plus-churn workload."""
import argparse,json,os,subprocess,time,urllib.request,urllib.parse
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--seconds',type=int,default=250);a=p.parse_args()
base='http://127.0.0.1:18994';env={k:v for k,v in os.environ.items() if not k.startswith('RIVER_')}
env.update(RIVER_BIND='127.0.0.1:18994',RIVER_CONFIG_MODE='env',RIVER_CORPUS_ROOT=str(ROOT/'corpus'),RIVER_AUTO_ADAPTATION_MODE='liberal,no-disk',RIVER_MEMORY_BUDGET_BYTES=str(2<<30),RIVER_CACHE_MEMORY_BYTES=str(4<<20),RIVER_DECODE_CONCURRENCY='2',RIVER_OPENJPEG_THREADS_PER_DECODE='2',RIVER_OTEL_ENABLED='false',RIVER_RESOURCE_DISPLAY='off',RIVER_LOG_LEVEL='warn',RIVER_ADMIN_TOKEN='fixture-admin')
def get(path):
 with urllib.request.urlopen(base+path,timeout=30) as r:return r.read()
with (ROOT/'target/tests/learning-server.log').open('w') as log:
 process=subprocess.Popen([str(ROOT/'target/benchmarks/river-current')],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT)
 try:
  for _ in range(200):
   try:get('/health');break
   except OSError:time.sleep(.05)
  start=time.monotonic();requests=0;snapshots=[];next_sample=0
  while time.monotonic()-start<a.seconds:
   # One hot tile interleaved with changing viewports: reuse plus eviction.
   tile=0 if requests%2==0 else 1+(requests//2)%255
   get(f'/streamer/v1/4/{tile%16}/{tile//16}/tile.png?'+urllib.parse.urlencode(dict(image='local:synthetic-texture-tlm-plt-d5.jp2',driver='openjpeg')));requests+=1
   elapsed=time.monotonic()-start
   if elapsed>=next_sample:
    snapshots.append(dict(seconds=elapsed,diagnostics=json.loads(get('/api/diagnostics/resources'))));next_sample+=15
    (ROOT/'target/tests/cache-learning.json').write_text(json.dumps(dict(complete=False,requests=requests,snapshots=snapshots),indent=2))
   time.sleep(.15)
  diagnostics=json.loads(get('/api/diagnostics/resources'));learned=diagnostics['cacheLearning']
  assert learned['sampledWindows']>=3,learned
  targets={x['name']:x['learnedTargetBytes'] for x in learned['targets']}
  baseline={k:v['effective_target_bytes'] for k,v in diagnostics['plan']['caches'].items() if k in targets}
  changed={k:dict(before=baseline[k],after=v) for k,v in targets.items() if v!=baseline[k]}
  assert changed,'Workload did not exercise redistribution; inspect misses and eviction patterns'
  assert sum(targets.values())==sum(baseline.values())
  for k,v in targets.items():assert baseline[k]//2<=v<=baseline[k]*2
  request=urllib.request.Request(base+'/api/settings/cache-learning',data=b'{"enabled":false}',headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'},method='PUT')
  urllib.request.urlopen(request).close();disabled=json.loads(get('/api/settings/cache-learning'));assert disabled['enabled'] is False
  (ROOT/'target/tests/cache-learning.json').write_text(json.dumps(dict(complete=True,passed=True,requests=requests,changed=changed,final=diagnostics,liveDisable=disabled,snapshots=snapshots),indent=2)+'\n')
  print(json.dumps(dict(passed=True,requests=requests,changed=changed)),flush=True)
 finally:process.terminate();process.wait(timeout=10)
