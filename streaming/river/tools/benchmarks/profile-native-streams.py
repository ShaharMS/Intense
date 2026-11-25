import importlib.util,json,os,time
from pathlib import Path
import numpy as np
spec=importlib.util.spec_from_file_location('native_experiment','/tmp/native-experiment.py');native=importlib.util.module_from_spec(spec);spec.loader.exec_module(native)
root=Path('/var/lib/river/s3-cache/local-sources')
path=next(root.rglob('river-source-*-034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299.jp2'))
samples=[];baseline={}
for streams in [1,2,4,8]:
 os.environ['RIVER_NVJPEG2000_TILE_STREAMS']=str(streams)
 decoder=native.Decoder()
 for reduction,bounds in [(0,[(0,0,512,512)]),(2,[(0,0,2048,2048)]),(6,[(0,0,32768,10980)])]:
  for i in range(11):
   start=time.perf_counter();images,timing=decoder.decode(path,40980,10980,bounds,reduction,100000000);elapsed=(time.perf_counter()-start)*1000
   if streams==1 and i==0:baseline[reduction]=images[0].copy()
   samples.append(dict(streams=streams,reduction=reduction,warmup=i==0,e2eMs=elapsed,**timing,equalPixels=bool(np.array_equal(images[0],baseline[reduction]))))
 decoder.close()
print(json.dumps(dict(samples=samples),indent=2))
if not all(s['equalPixels'] for s in samples):raise SystemExit(1)
