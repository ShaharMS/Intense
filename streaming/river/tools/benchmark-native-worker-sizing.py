"""Compare native GPU worker counts under identical decode admission."""
import argparse,concurrent.futures,hashlib,io,json,math,pathlib,subprocess,time,urllib.parse,urllib.request,uuid
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--counts',nargs='+',type=int,default=[32,8]);p.add_argument('--runs',type=int,default=3)
p.add_argument('--clients',type=int,default=32);p.add_argument('--decode-slots',type=int,default=8)
p.add_argument('--candidate-image',help='For count 0, use this image with its default native worker setting')
p.add_argument('--report',default='native-worker-sizing-m88');a=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports'/a.report;out.mkdir(exist_ok=True)
corpus=root/'corpus/cache/sentinel-date-mosaics';entry=next(e for e in json.loads((corpus/'river-assets.json').read_text())['assets'] if e['source']=='2016-09-12.jp2')
(out/'catalog.json').write_text(json.dumps(dict(assets=[{k:entry[k] for k in ['assetId','width','height','bitDepth','source']}])) )
cases=[('small','94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290',5,[(x,y) for y in range(4) for x in range(6)]),
       ('large',entry['assetId'],6,list(dict.fromkeys([(0,0),(80,0),(0,21),(80,21)]+[(i*7%81,i*3%22) for i in range(32)]))[:32])]
oracles={};rows=[];run_id=uuid.uuid4().hex[:8]
def read_json(url):return json.load(urllib.request.urlopen(url,timeout=120))
def resource(name):
    script="from pathlib import Path;import json;v=dict(line.split() for line in Path('/sys/fs/cgroup/memory.stat').read_text().splitlines());print(json.dumps({'anon':int(v['anon']),'file':int(v['file']),'current':int(Path('/sys/fs/cgroup/memory.current').read_text()),'peak':int(Path('/sys/fs/cgroup/memory.peak').read_text())}))"
    return json.loads(subprocess.check_output(['docker','exec',name,'python3','-c',script],text=True))
def tile(api,asset,matrix,position,driver):
    x,y=position;q=urllib.parse.urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,STYLE='default',FORMAT='image/png',DRIVER=driver,TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=matrix,TILEROW=y,TILECOL=x))
    start=time.perf_counter()
    with urllib.request.urlopen(api+'/wmts?'+q,timeout=120) as r:
        body=r.read();route=r.headers['x-river-decoder-route'];decode=float(r.headers['x-river-decode-ms'])
    elapsed=(time.perf_counter()-start)*1000
    image=Image.open(io.BytesIO(body)).convert('RGBA');fingerprint=(image.size,hashlib.sha256(image.tobytes()).hexdigest())
    return dict(elapsed=elapsed,decode=decode,route=route,fingerprint=fingerprint)
oracle_name=f'river-goal-oracle-{run_id}';oracle_api='http://127.0.0.1:18580'
subprocess.run(['docker','run','-d','--name',oracle_name,'-p','127.0.0.1:18580:8080',
    '-e','RIVER_ENABLE_NVIDIA=false','-e','RIVER_OTEL_ENABLED=false','-e','RIVER_PROFILE=throughput16',
    '-e','RIVER_RASTER_CACHE_BYTES=0','-e','RIVER_RASTER_DISK_CACHE_BYTES=0','-e','RIVER_ASSET_CATALOG=/study/catalog.json',
    '--mount',f'type=bind,source={corpus},target=/data,readonly',
    '--mount',f'type=bind,source={out/"catalog.json"},target=/study/catalog.json,readonly',
    'river:2026.10.15-gpu-read-combo-m87'],check=True,capture_output=True)
try:
    deadline=time.monotonic()+30
    while True:
        try:read_json(oracle_api+'/api/drivers');break
        except OSError:
            if time.monotonic()>deadline:raise
            time.sleep(.05)
    for case,asset,matrix,positions in cases:
        read_json(oracle_api+'/api/assets/'+asset+'/manifest')
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            reference=list(pool.map(lambda pos:tile(oracle_api,asset,matrix,pos,'jp2grok'),positions))
        oracles[case]={pos:r['fingerprint'] for pos,r in zip(positions,reference)}
finally:subprocess.run(['docker','stop',oracle_name],check=True,capture_output=True)
for run in range(a.runs):
    for count in (a.counts if run%2==0 else list(reversed(a.counts))):
        name=f'river-goal-worker-{run_id}-{count}-{run}';api='http://127.0.0.1:18579'
        command=['docker','run','-d','--gpus','all','--name',name,'-p','127.0.0.1:18579:8080',
            '-e','RIVER_ENABLE_NVIDIA=true','-e','RIVER_ADMIN_TOKEN=fixture-admin','-e','RIVER_OTEL_ENABLED=false',
            '-e','RIVER_PROFILE=throughput16','-e','RIVER_RASTER_CACHE_BYTES=0','-e','RIVER_RASTER_DISK_CACHE_BYTES=0',
            '-e',f'RIVER_DECODE_CONCURRENCY={a.decode_slots}',
            '-e','RIVER_ASSET_CATALOG=/study/catalog.json',
            '--mount',f'type=bind,source={corpus},target=/data,readonly',
            '--mount',f'type=bind,source={out/"catalog.json"},target=/study/catalog.json,readonly',
            a.candidate_image if count==0 else 'river:2026.10.15-gpu-read-combo-m87']
        if count!=0:command[-1:-1]=['-e',f'RIVER_NVJPEG2000_NATIVE_WORKERS={count}']
        started=time.perf_counter();subprocess.run(command,check=True,capture_output=True)
        try:
            deadline=time.monotonic()+60
            while True:
                try:read_json(api+'/api/drivers');break
                except OSError:
                    if time.monotonic()>deadline:raise
                    time.sleep(.05)
            ready_ms=(time.perf_counter()-started)*1000;before=resource(name)
            for case,asset,matrix,positions in cases:
                read_json(api+'/api/assets/'+asset+'/manifest')
                for state in ['cold','repeat']:
                    def one(pos):
                        result=tile(api,asset,matrix,pos,'nvjpeg2000')
                        assert result['route'].startswith('nvjpeg2000'),result
                        assert result['fingerprint']==oracles[case][pos],(count,case,pos,result['fingerprint'],oracles[case][pos])
                        return result
                    started_batch=time.perf_counter()
                    with concurrent.futures.ThreadPoolExecutor(max_workers=a.clients) as pool:results=list(pool.map(one,positions))
                    times=sorted(r['elapsed'] for r in results)
                    row=dict(run=run,nativeWorkers=count if count else 32,configuration='candidate' if count==0 else f'workers{count}',case=case,state=state,clients=a.clients,tiles=len(positions),
                        wallMs=(time.perf_counter()-started_batch)*1000,p95Ms=times[math.ceil(.95*len(times))-1],
                        apiReadyMs=ready_ms,pixels='exact',before=before,after=resource(name))
                    rows.append(row);print(json.dumps(row),flush=True)
        finally:subprocess.run(['docker','stop',name],check=True,capture_output=True)
        (out/'results.json').write_text(json.dumps(rows,indent=2))
