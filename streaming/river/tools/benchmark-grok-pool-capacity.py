"""Same-image Grok pool capacity experiment, one source with distinct object keys.

Frozen bytes/references are reused via read-only aliases; no files are rewritten.
"""
import argparse,concurrent.futures,hashlib,io,json,statistics,subprocess,threading,time,urllib.error,urllib.parse,urllib.request,uuid
from pathlib import Path
from PIL import Image
p=argparse.ArgumentParser()
for key in ['baseline-container','baseline-api','candidate-container','candidate-api','origin-container','origin-control','aliases-manifest','output']:p.add_argument('--'+key,required=True)
p.add_argument('--rounds',type=int,default=4);p.add_argument('--code-comparison',action='store_true');p.add_argument('--require-empty-sources',action='store_true');args=p.parse_args();assert 2<=args.rounds<=4
root=Path(__file__).resolve().parents[1];out=Path(args.output);assert not out.exists()
raw=(root/'reports/fixed-gate-v2/manifest.json').read_bytes();frozen=json.loads(raw)
aliases=Path(args.aliases_manifest).read_bytes();assert json.loads(aliases)['frozenManifestSha256']==hashlib.sha256(raw).hexdigest()
versions={v:dict(container=getattr(args,v+'_container'),api=getattr(args,v+'_api')) for v in ['baseline','candidate']}
inspect={v:json.loads(subprocess.check_output(['docker','inspect',s['container']],text=True))[0] for v,s in versions.items()}
assert all(s['container'].startswith('river-goal-') for s in versions.values())
if not args.code_comparison:assert inspect['baseline']['Image']==inspect['candidate']['Image']
envs={v:dict(x.split('=',1) for x in inspect[v]['Config']['Env']) for v in versions}
pools={v:envs[v].pop('RIVER_GROK_NATIVE_THREADS') for v in versions};assert envs['baseline']==envs['candidate']
if args.code_comparison:assert pools['baseline']==pools['candidate']
assert envs['baseline']['RIVER_DECODE_CONCURRENCY']=='4'
resources=['NanoCpus','CpuQuota','CpuPeriod','CpusetCpus','Memory','MemorySwap']
assert all(inspect['baseline']['HostConfig'][k]==inspect['candidate']['HostConfig'][k] for k in resources)
ip=subprocess.check_output(['docker','inspect',args.origin_container,'--format','{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}'],text=True).strip()
def http(url,data=None,method=None):
 req=urllib.request.Request(url,data=None if data is None else json.dumps(data).encode(),headers={'Content-Type':'application/json'},method=method)
 try:
  with urllib.request.urlopen(req,timeout=60) as response:return response.read(),dict(response.headers)
 except urllib.error.HTTPError as error:
  error.response_body=error.read(8192).decode(errors='replace');error.add_note(error.response_body);raise
def js(url,*rest):return json.loads(http(url,*rest)[0])
origin=js(args.origin_control+'/__manifest');assert origin['sha256']==hashlib.sha256(aliases).hexdigest()
assert origin['materialization']['manifestSha256']==hashlib.sha256(raw).hexdigest()
refs={}
for c in frozen['cases']:
 ref=(root/'reports/fixed-gate-v2'/c['reference']).read_bytes();assert hashlib.sha256(ref).hexdigest()==c['referenceSha256']
 refs[c['name']]=Image.open(io.BytesIO(ref)).convert('RGB')
 assert any(f['sha256']==c['sha256'] and f['bytes']==c['sourceBytes'] for f in origin['materialization']['files'])
 for n in range(4):assert origin['files'][c['name']+f'-client{n}']==c['sourceBytes']
def stat(container,name):
 text=subprocess.check_output(['docker','exec',container,'cat','/sys/fs/cgroup/'+name],text=True)
 return {k:int(v) for k,v in (line.split() for line in text.splitlines())}
rows=[];report=dict(passed=False,scope='Configuration capacity experiment: same351binary, four concurrent distinct keys on one source, four admitted decodes, source concurrency4. OriginOS cache warm, process/source identity fresh per cohort. PNG validation outside timers. cgroup peak includes cache. No production-S3 or release qualification.',frozenManifestSha256=hashlib.sha256(raw).hexdigest(),aliasesManifestSha256=hashlib.sha256(aliases).hexdigest(),harnessSha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),image=inspect['baseline']['Image'],nativePools=pools,resources={k:inspect['baseline']['HostConfig'][k] for k in resources},rounds=args.rounds,origin=origin,rows=rows)
def save():out.write_text(json.dumps(report,indent=2))
active_source=None
report['codeComparison']=args.code_comparison
report['images']={v:inspect[v]['Image'] for v in versions}
if args.code_comparison:
 report['scope']='Code comparison on recorded images with identical environment/resources: four concurrent distinct frozen object aliases on one source, four admitted decodes, source concurrency4. OriginOS cache warm, fresh process/source identity per cohort; pixels/repeats validated outside timers. Cgroup peak includes file cache. Capacity diagnostic, not single-request release qualification.'
def phase(urls):
 barrier=threading.Barrier(5)
 def request(url):
  barrier.wait(timeout=30);started=time.perf_counter();body,headers=http(url)
  return dict(ms=(time.perf_counter()-started)*1000,body=body,headers=headers)
 with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
  pending=[pool.submit(request,url) for url in urls];started=time.perf_counter();barrier.wait(timeout=30)
  jobs=[future.result() for future in pending];elapsed=(time.perf_counter()-started)*1000
 return jobs,elapsed
try:
 for trial in range(args.rounds):
  for case in frozen['cases']:
   for version in (['baseline','candidate'] if trial%2==0 else ['candidate','baseline']):
    v=versions[version];api=v['api'];container=v['container'];subprocess.run(['docker','restart',container],check=True,stdout=subprocess.DEVNULL)
    for _ in range(100):
     try:http(api+'/health');break
     except OSError:time.sleep(.1)
    else:raise RuntimeError('Readiness timeout')
    roster_count=len(js(api+'/api/s3/sources'))
    if args.require_empty_sources:assert roster_count==0,'Use fresh benchmark containers.'
    js(api+'/api/settings/read-policy',dict(mode='adaptive',minimumSamples=8,maxOverfetchRatio=2,maxSourceRequests=4,serialTransport='sdk'),'PUT')
    sid='pool-capacity-'+uuid.uuid4().hex;endpoint=f'http://{ip}:8000/{sid}'
    js(api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
    active_source=(api,sid)
    control=args.origin_control+'/__stats/'+sid;assert not any(js(control)['wire'].values())
    urls=[]
    for n in range(4):
     q=dict(image=endpoint+'/fixture/'+case['name']+f'-client{n}.jp2',driver='jp2grok',edge='crop',tileSize=case['tileSize'],minZoom=0,maxZoom=case['maxZoom'],tilePixelRatio=1)
     urls.append(api+f'/streamer/v1/{case["z"]}/{case["x"]}/{case["y"]}/tile.png?'+urllib.parse.urlencode(q))
    before=stat(container,'cpu.stat');cold,cold_ms=phase(urls);after=stat(container,'cpu.stat');wire=js(control)
    for job in cold:
     image=Image.open(io.BytesIO(job['body']));assert image.getchannel('A').getextrema()==(255,255) if 'A' in image.getbands() else True
     image=image.convert('RGB');gold=refs[case['name']];assert image.size==gold.size and image.tobytes()==gold.tobytes()
    warm,warm_ms=phase(urls)
    assert all(a['body']==b['body'] for a,b in zip(cold,warm)) and js(control)['wire']==wire['wire']
    assert wire['pressure']['peak']<=4,wire['pressure']
    assert stat(container,'memory.events').get('oom_kill',0)==0
    peak=int(subprocess.check_output(['docker','exec',container,'cat','/sys/fs/cgroup/memory.peak'],text=True))
    row=dict(case=case['name'],trial=trial,version=version,sourceId=sid,coldBatchMs=cold_ms,warmBatchMs=warm_ms,coldClientMs=[j['ms'] for j in cold],warmClientMs=[j['ms'] for j in warm],sourceWire=wire['wire'],sourcePeakActive=wire['pressure']['peak'],peakMemoryBytes=peak,cpuDelta={k:after[k]-before[k] for k in after},pixelsExact=True,repeatIoZero=True)
    row['sourceRosterBeforeCold']=roster_count
    http(api+'/api/s3/sources/'+sid,method='DELETE');active_source=None
    rows.append(row);save();print(json.dumps({k:row[k] for k in ['case','trial','version','coldBatchMs','sourcePeakActive','peakMemoryBytes']}),flush=True)
 report['summary']={}
 for c in frozen['cases']:
  name=c['name'];rs=[r for r in rows if r['case']==name]
  report['summary'][name]={v:dict(coldBatchMedianMs=statistics.median(r['coldBatchMs'] for r in rs if r['version']==v),warmBatchMedianMs=statistics.median(r['warmBatchMs'] for r in rs if r['version']==v),peakMemoryBytes=max(r['peakMemoryBytes'] for r in rs if r['version']==v)) for v in versions}
 report['passed']=True
except Exception as error:
 report['error']=repr(error);report['errorBody']=getattr(error,'response_body',None);raise
finally:
 if active_source:
  try:http(active_source[0]+'/api/s3/sources/'+active_source[1],method='DELETE')
  except Exception as error:report['cleanupError']=repr(error)
 save()
