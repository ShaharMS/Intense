"""Explicit local lookup, path confinement, concurrent opens and version invalidation.
The obsolete directory-discovery contract is intentionally no longer supported.
"""
import argparse,concurrent.futures,io,json,os,subprocess,tempfile,time,urllib.request,urllib.parse,urllib.error
from pathlib import Path
from PIL import Image
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,default=ROOT/'target/benchmarks/river-current');a=p.parse_args()
(ROOT/'target/tests').mkdir(parents=True,exist_ok=True)
with tempfile.TemporaryDirectory(dir=ROOT/'target/tests',prefix='local-') as directory:
 root=Path(directory);source=root/'image with spaces.jp2';Image.new('RGB',(256,256),(31,117,209)).save(source,format='JPEG2000',irreversible=False,num_resolutions=5)
 env={k:v for k,v in os.environ.items() if not k.startswith('RIVER_')};env.update(RIVER_BIND='127.0.0.1:18993',RIVER_CONFIG_MODE='env',RIVER_CORPUS_ROOT=str(root),RIVER_AUTO_ADAPTATION_MODE='no-disk',RIVER_MEMORY_BUDGET_BYTES=str(512<<20),RIVER_DECODE_CONCURRENCY='2',RIVER_OTEL_ENABLED='false',RIVER_RESOURCE_DISPLAY='off',RIVER_LOG_LEVEL='warn')
 with (ROOT/'target/tests/local-server.log').open('w') as log:
  process=subprocess.Popen([str(a.binary.resolve())],env=env,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT)
  def get(path):
   with urllib.request.urlopen('http://127.0.0.1:18993'+path,timeout=20) as r:return r.read()
  def status(path):
   try:get(path);return 200
   except urllib.error.HTTPError as e:return e.code
  try:
   for _ in range(200):
    try:get('/health');break
    except OSError:time.sleep(.05)
   else:raise RuntimeError('Local server did not start')
   (ROOT/'target/tests/local-startup-resources.json').write_text(get('/api/diagnostics/resources').decode())
   assert status('/api/assets')==404
   assert status('/api/s3/sources/unused/objects')==404
   assert b'<Layer>' not in get('/wmts?SERVICE=WMTS&REQUEST=GetCapabilities')
   key='local:'+source.name;info='/api/tiles/'+urllib.parse.quote(key,safe='');tile='/streamer/v1/0/0/0/tile.png?'+urllib.parse.urlencode(dict(image=key,driver='openjpeg'))
   with concurrent.futures.ThreadPoolExecutor(max_workers=16) as pool:responses=list(pool.map(lambda _:get(tile),range(16)))
   for data in responses:
    with Image.open(io.BytesIO(data)) as im:assert im.convert('RGB').tobytes()==bytes([31,117,209])*(256*256)
   first=json.loads(get(info))['assetId']
   Image.new('RGB',(256,256),(211,51,13)).save(source,format='JPEG2000',irreversible=False,num_resolutions=5)
   second=json.loads(get(info))['assetId'];assert first!=second
   with Image.open(io.BytesIO(get(tile))) as im:assert im.convert('RGB').tobytes()==bytes([211,51,13])*(256*256)
   for key in ['local:../Cargo.toml','local:/etc/passwd']:
    assert status('/api/tiles/'+urllib.parse.quote(key,safe=''))>=400
   (root/'escape.jp2').symlink_to(ROOT/'corpus/synthetic-terrain-plain-d4.jp2')
   assert status('/api/tiles/'+urllib.parse.quote('local:escape.jp2',safe=''))>=400
   resources=json.loads(get('/api/diagnostics/resources'))
   (ROOT/'target/tests/local-lookup.json').write_text(json.dumps(dict(passed=True,concurrentRequests=16,exactPixels=True,versionInvalidation=True,pathConfinement=True,removedRoutes=True,resources=resources),indent=2)+'\n')
   print('Local lookup, 16 concurrent opens, version invalidation, path confinement and removed routes passed')
  finally:
   process.terminate();process.wait(timeout=10)
