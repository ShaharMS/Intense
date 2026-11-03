"""Bounded same-image capacity screen: one shared S3 source, distinct frozen keys."""
import argparse,concurrent.futures,hashlib,io,ipaddress,itertools,json,statistics,subprocess,threading,time,urllib.request,urllib.parse,uuid
from pathlib import Path
from PIL import Image
parser=argparse.ArgumentParser();parser.add_argument('--output-dir',type=Path,required=True);parser.add_argument('--phase',choices=['large','cross','warm','placement'],required=True)
args=parser.parse_args();root=Path(__file__).resolve().parents[1];out=args.output_dir.resolve();out.mkdir(exist_ok=False)
manifest=json.loads((root/'reports/fixed-gate-v2/manifest.json').read_text());cases=manifest['cases']
template=json.loads(subprocess.check_output(['docker','inspect','river-goal-primary700-candidate']))[0]
run_id='capacity-'+uuid.uuid4().hex[:8];label='river.benchmark='+run_id
network='river-goal-'+run_id+'-net';origin='river-goal-'+run_id+'-origin';slot='river-goal-'+run_id+'-slot';client='river-goal-'+run_id+'-client';api='http://127.0.0.1:18887';control='http://127.0.0.1:18946'
profiles={'latency':(1,16),'balanced':(4,16),'throughput':(8,4)}
def run(*args):return subprocess.check_output(list(args),text=True).strip()
def http(url,data=None,method=None):
 try:
  with urllib.request.urlopen(urllib.request.Request(url,data=None if data is None else json.dumps(data).encode(),headers={'Content-Type':'application/json'},method=method),timeout=40) as r:return r.read(),{k.lower():v for k,v in r.headers.items()}
 except urllib.error.HTTPError as e:e.add_note(e.read(4096).decode(errors='replace'));raise
def js(url,*args,**kwargs):return json.loads(http(url,*args,**kwargs)[0])
def stop_owned(name):
 p=subprocess.run(['docker','inspect',name],capture_output=True,text=True)
 if p.returncode:return
 c=json.loads(p.stdout)[0];assert c['Config']['Labels'].get('river.benchmark')==run_id
 subprocess.run(['docker','stop',name],check=True,stdout=subprocess.DEVNULL)
 subprocess.run(['docker','rm',name],check=True,stdout=subprocess.DEVNULL)
aliases=[]
for c in cases:
 for i in range(16):
  item=dict(c,name=c['name']+'-client'+str(i),file='/data/reports/fixed-gate-v2/'+c['file']);aliases.append(item)
(out/'origin-manifest.json').write_text(json.dumps(dict(cases=aliases),indent=2))
report=dict(passed=False,scope='Same698image,16CPU/8GiB. One shared connected source per cohort; distinct object keys alias the same hash-pinned input. Cold and exact-repeat timers are separate; pixel validation outside both. Three profiles, fixed network address, unshaped local origin with warm OS cache. Descriptive capacity screen, not release promotion or32corequalification.',image=template['Image'],profiles=profiles,rows=[])
report.update(phase=args.phase,runnerSha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),manifestSha256=hashlib.sha256((root/'reports/fixed-gate-v2/manifest.json').read_bytes()).hexdigest())
def save():(out/'results.json').write_text(json.dumps(report,indent=2))
save();network_created=False
try:
 run('docker','network','create','--label',label,network);network_created=True
 net=json.loads(run('docker','network','inspect',network))[0];subnet=ipaddress.ip_network(net['IPAM']['Config'][0]['Subnet']);origin_ip=str(subnet[2]);slot_ip=str(subnet[3])
 report['network']=net
 run('docker','run','-d','--name',origin,'--label',label,'--network',network,'--ip',origin_ip,'--cpus','4','--cpuset-cpus','16-19','--memory','2g','-p','127.0.0.1:18946:8000',
     '-v',str(root)+':/repo:ro','-v',str(out)+':/out:ro','-v','river-goal-fixed-native-data:/data:ro','--entrypoint','python3','river-reader-lab:marked-packed-m212','/repo/tools/serve-fixed-native-origin.py','--manifest','/out/origin-manifest.json','--tcp-nodelay')
 deadline=time.monotonic()+20
 while True:
  try:report['origin']=js(control+'/__manifest');break
  except OSError:
   if time.monotonic()>deadline:raise
   time.sleep(.1)
 before_net=js(control+'/__netstat');report['slotIp']=slot_ip;report['sourceSharedAcrossClients']=True;save()
 scenarios=([(next(c for c in cases if c['name']=='gb11-preview'),n,0) for n in [1,4,8,16]] if args.phase=='large'
            else [(c,4,0) for c in cases if c['name']!='gb11-preview'])
 if args.phase in ['warm','placement']:scenarios=[(next(c for c in cases if c['name']=='gb11-preview'),16,0)]
 started=time.monotonic()
 for case,n,round_ in scenarios:
  order=list(profiles)
  if args.phase in ['warm','placement']:order=['balanced']
  if round_%2:order.reverse()
  for profile in order:
   assert time.monotonic()-started<180,'Capacity screen exceeded180seconds; preserve partial report'
   slots,threads=profiles[profile];env=dict(x.split('=',1) for x in template['Config']['Env'])
   env.update(RIVER_DECODE_CONCURRENCY=str(slots),RIVER_OPENJPEG_THREADS_PER_DECODE=str(threads),RIVER_RASTER_CACHE_BYTES=str(256*1024*1024))
   cmd=['docker','run','-d','--name',slot,'--label',label,'--network',network,'--ip',slot_ip,'--cpus','16','--cpuset-cpus','0-15','--memory','8g','--memory-swap','12g','-p','127.0.0.1:18887:8080']
   for k,v in env.items():cmd+=['-e',k+'='+v]
   run(*cmd,template['Image'])
   deadline=time.monotonic()+20
   while True:
    try:http(api+'/health');break
    except OSError:
     if time.monotonic()>deadline:raise
     time.sleep(.1)
   cfg=json.loads(run('docker','inspect',slot))[0];assert cfg['NetworkSettings']['Networks'][network]['IPAddress']==slot_ip
   js(api+'/api/settings/read-policy',dict(mode='adaptive',minimumSamples=8,maxSourceRequests=8,maxOverfetchRatio=2,serialTransport='sdk'),'PUT')
   sid='capacity-'+uuid.uuid4().hex;endpoint='http://'+origin_ip+':8000/'+sid
   js(api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
   stats_url=control+'/__stats/'+sid;assert not any(js(stats_url)['wire'].values());cohort_net_before=js(control+'/__netstat')
   urls=[]
   for i in range(n):
    q=dict(image=endpoint+'/fixture/'+case['name']+'-client'+str(i)+'.jp2',driver='openjpeg',edge='crop',tileSize=case['tileSize'],minZoom=0,maxZoom=case['maxZoom'],tilePixelRatio=1)
    urls.append(api+f'/streamer/v1/{case["z"]}/{case["x"]}/{case["y"]}/tile.png?'+urllib.parse.urlencode(q))
   def batch():
    barrier=threading.Barrier(n)
    def request(url):
     barrier.wait(timeout=30);start=time.perf_counter();body,headers=http(url);end=time.perf_counter()
     return dict(start=start,end=end,ms=(end-start)*1000,body=body,headers=headers)
    with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:return list(pool.map(request,urls))
   cold=batch();origin_snapshot=js(stats_url);wire=origin_snapshot['wire'];cold_peak=int(run('docker','exec',slot,'cat','/sys/fs/cgroup/memory.peak'))
   gold_path=root/'reports/fixed-gate-v2'/case['reference'];assert hashlib.sha256(gold_path.read_bytes()).hexdigest()==case['referenceSha256'];gold=Image.open(gold_path).convert('RGB')
   for row in cold:
    image=Image.open(io.BytesIO(row['body'])).convert('RGB');assert image.size==gold.size and image.tobytes()==gold.tobytes()
    assert int(row['headers']['x-river-source-range-requests'])>0,'Cold alias silently reused source work'
   warm=batch();assert js(stats_url)['wire']==wire
   for a,b in zip(cold,warm):assert a['body']==b['body']
   def values(rows):return dict(batchMs=(max(r['end'] for r in rows)-min(r['start'] for r in rows))*1000,clientMs=[r['ms'] for r in rows],responseBytes=[len(r['body']) for r in rows],headers=[r['headers'] for r in rows])
   row=dict(case=case['name'],clients=n,round=round_,profile=profile,cold=values(cold),warm=values(warm),wire=wire,coldMemoryPeakBytes=cold_peak,memoryEvents=run('docker','exec',slot,'cat','/sys/fs/cgroup/memory.events'),observations=js(api+'/api/settings/read-policy')['sources'][sid],pixelsExact=True,repeatIoZero=True)
   row.update(resources={k:cfg['HostConfig'][k] for k in ['NanoCpus','CpusetCpus','Memory','MemorySwap']},environmentSha256=hashlib.sha256(json.dumps(sorted(cfg['Config']['Env'])).encode()).hexdigest(),publicSettings=dict(encoding=js(api+'/api/settings/encoding'),readPolicy=js(api+'/api/settings/read-policy')['settings']),originPressure=origin_snapshot['pressure'],originNetworkBefore=cohort_net_before,originNetworkAfter=js(control+'/__netstat'))
   assert all(row['originNetworkAfter'][k]==cohort_net_before[k] for k in ['ListenDrops','ListenOverflows'])
   assert row['originPressure']['peak']<=8,'Shared-source limit exceeded'
   assert 'oom_kill 0' in row['memoryEvents']
   if args.phase in ['warm','placement']:
    config=dict(urls=[u.replace(api,'http://'+slot_ip+':8080',1) for u in urls],hashes=[hashlib.sha256(r['body']).hexdigest() for r in cold])
    if args.phase=='placement':
     # Reset process and source identity; keep image, resources, request pixels
     # and the fixed network placement unchanged for the Linux-side cold burst.
     run('docker','restart',slot);deadline=time.monotonic()+20
     while True:
      try:http(api+'/health');break
      except OSError:
       if time.monotonic()>deadline:raise
       time.sleep(.1)
     http(api+'/api/s3/sources/'+sid,method='DELETE')
     linux_sid='capacity-'+uuid.uuid4().hex;linux_endpoint='http://'+origin_ip+':8000/'+linux_sid
     js(api+'/api/s3/sources',dict(id=linux_sid,name=linux_sid,bucket='fixture',region='us-east-1',endpoint=linux_endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
     config.update(urls=[u.replace(sid,linux_sid) for u in config['urls']],coldFirst=True,sourceStats='http://'+origin_ip+':8000/__stats/'+linux_sid)
    (out/'cached-client.json').write_text(json.dumps(config))
    run('docker','run','--name',client,'--label',label,'--network',network,'--cpus','2','--cpuset-cpus','20-21','--memory','1g',
        '-v',str(root)+':/repo:ro','-v',str(out)+':/out','--entrypoint','python3','river-reader-lab:marked-packed-m212',
        '/repo/tools/probe-cached-http.py','/out/cached-client.json','/out/linux-warm.json')
    row['linuxWarm']=json.loads((out/'linux-warm.json').read_text());assert row['linuxWarm']['passed']
    if args.phase=='warm':assert js(stats_url)['wire']==wire,'Linux cached probe added source IO'
    stop_owned(client)
   report['rows'].append(row);save();print(json.dumps({k:row[k] for k in ['case','clients','round','profile','wire','coldMemoryPeakBytes']}|dict(coldBatchMs=row['cold']['batchMs'],warmBatchMs=row['warm']['batchMs'])),flush=True)
   del cold,warm;stop_owned(slot)
 after_net=js(control+'/__netstat');assert all(after_net[k]==before_net[k] for k in ['ListenDrops','ListenOverflows'])
 report.update(passed=True,originNetworkBefore=before_net,originNetworkAfter=after_net);save()
except Exception as error:
 report['failure']=repr(error);save();raise
finally:
 stop_owned(client);stop_owned(slot);stop_owned(origin)
 if network_created:subprocess.run(['docker','network','rm',network],check=True,stdout=subprocess.DEVNULL)
