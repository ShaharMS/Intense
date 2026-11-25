"""Matched native-binary comparison against generated local Range fixtures.
No source listing, remote imagery, Docker containers, or persisted credentials.
"""
import argparse, hashlib, io, json, os, subprocess, time, urllib.parse, urllib.request
from pathlib import Path
from PIL import Image
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--pairs',type=int,default=3);p.add_argument('--limit',type=int,default=15);a=p.parse_args()
fixtures=json.loads((ROOT/'target/fixtures/corpus.json').read_text())['fixtures'][:a.limit]
out=ROOT/'target/benchmarks/consolidation.json';rows=[]
def request(url):
 with urllib.request.urlopen(url,timeout=120) as r:return r.read(),dict(r.headers)
def stats(sid):return json.loads(request('http://127.0.0.1:18990/__stats/'+sid)[0])['wire']
base={k:v for k,v in os.environ.items() if not k.startswith('RIVER_')}
base.update(RIVER_BIND='127.0.0.1:18991',RIVER_CONFIG_MODE='env',RIVER_OTEL_ENABLED='false',RIVER_RESOURCE_DISPLAY='off',RIVER_MEMORY_BUDGET_BYTES=str(2<<30),RIVER_DECODE_CONCURRENCY='2',RIVER_SOURCE_IO_CONCURRENCY='4',RIVER_S3_SOURCE_MAX_REQUESTS='8',RIVER_OPENJPEG_THREADS_PER_DECODE='2',RIVER_AUTO_ADAPTATION_MODE='no-disk',RIVER_ENABLED_DRIVERS='openjpeg',RIVER_LOG_LEVEL='warn',RIVER_S3_ADAPTIVE_READS='adaptive',RIVER_S3_SERIAL_TRANSPORT='sdk',RIVER_S3_SOURCE_COUNT='1',RIVER_S3_SOURCE_1_ID='fixture',RIVER_S3_SOURCE_1_NAME='Synthetic benchmark',RIVER_S3_SOURCE_1_BUCKET='fixture',RIVER_S3_SOURCE_1_REGION='us-east-1',RIVER_S3_SOURCE_1_ACCESS_KEY_ID='fixture',RIVER_S3_SOURCE_1_SECRET_ACCESS_KEY='fixture',RIVER_S3_SOURCE_1_ALLOW_HTTP='true',RIVER_S3_SOURCE_1_VIRTUAL_HOSTED_STYLE='false')
for key in ['MULTIPART_PLT','MULTIPART_PREFIX','SINGLE_PREFIX','INDEX_ONLY_PREFIX','STREAMED_PLT_ROI','PRECINCT_ROI','EXACT_SUPPORT']:base['RIVER_S3_'+key]='true'
def run(fixture,version,pair,flow):
 sid=f'{version}-{pair}-{flow}-{Path(fixture["file"]).stem}'
 env=dict(base,RIVER_S3_SOURCE_1_ENDPOINT='http://127.0.0.1:18990/'+sid)
 binary=ROOT/'target/benchmarks'/('river-'+version)
 log=(ROOT/'target/benchmarks'/f'{version}-server.log').open('a')
 server=subprocess.Popen(['taskset','-c','0-3',str(binary)],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT)
 try:
  for _ in range(200):
   if server.poll() is not None:raise RuntimeError(f'{version} exited: see {log.name}')
   try:request('http://127.0.0.1:18991/health');break
   except OSError:time.sleep(.05)
  else:raise RuntimeError('Server startup timed out')
  with Image.open(ROOT/'corpus'/fixture['source']) as ref:expected=ref.convert('RGB').crop((0,0,256,256)).tobytes()
  for phase,zoom in ([('cold_zoom',4)] if flow=='cold' else [('cold_preview',0),('zoom_after_preview',4),('repeated_zoom',4)]):
   before=stats(sid);start=time.perf_counter()
   image='s3://fixture/'+fixture['file'];url='http://127.0.0.1:18991/streamer/v1/'+str(zoom)+'/0/0/tile.png?'+urllib.parse.urlencode({'image':image,'driver':'openjpeg'})
   try:
    data,headers=request(url);elapsed=(time.perf_counter()-start)*1000;after=stats(sid)
    with Image.open(io.BytesIO(data)) as decoded:
     pixels=decoded.convert('RGB').tobytes();size=decoded.size
    exact=pixels==expected if zoom==4 else None
    if exact is False:raise RuntimeError('Full-resolution output differs from independently generated reference pixels')
    row=dict(image=fixture['file'],version=version,pair=pair,phase=phase,ms=elapsed,wire={k:after[k]-before[k] for k in after},size=size,pixelSha256=hashlib.sha256(pixels).hexdigest(),referencePixelsExact=exact,headers={k:v for k,v in headers.items() if k.lower().startswith('x-river-')})
   except Exception as e:
    row=dict(image=fixture['file'],version=version,pair=pair,phase=phase,error=str(e));rows.append(row);out.write_text(json.dumps({'complete':False,'rows':rows},indent=2));raise
   rows.append(row);out.write_text(json.dumps({'complete':False,'rows':rows},indent=2));print(json.dumps({k:row[k] for k in ['image','version','pair','phase','ms','wire','referencePixelsExact']}),flush=True)
 finally:
  server.terminate()
  try:server.wait(timeout=10)
  except subprocess.TimeoutExpired:server.kill();server.wait()
  log.close()
for fixture in fixtures:
 for pair in range(a.pairs):
  for version in (['before','current'] if pair%2==0 else ['current','before']):
   for flow in ['cold','reuse']:run(fixture,version,pair,flow)
# Compare reduced-resolution pixels between releases separately from exact source crop checks.
for fixture in fixtures:
 hashes={r['pixelSha256'] for r in rows if r['image']==fixture['file'] and r['phase']=='cold_preview'}
 if len(hashes)!=1:raise RuntimeError('Preview pixel regression: '+fixture['file'])
out.write_text(json.dumps({'complete':True,'baseline':'git 980cd5b rebuilt locally; not the original m1227 binary','driver':'OpenJPEG','cpuAffinity':'0-3','memoryBudgetBytes':2<<30,'originDelayMs':7,'originBytesPerSecond':300000000,'sourceDiskCold':False,'rows':rows},indent=2))
