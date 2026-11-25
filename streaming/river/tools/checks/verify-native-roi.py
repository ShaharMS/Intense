import importlib.util,json,os,time
from pathlib import Path
import numpy as np
spec=importlib.util.spec_from_file_location('native_experiment','/tmp/native-experiment.py');native=importlib.util.module_from_spec(spec);spec.loader.exec_module(native)
samples=[]
for path,w,h,maxreduce in [(Path('/app/corpus/Bretagne2.j2k'),2592,1944,5),(next(Path('/var/lib/river/s3-cache/local-sources/cold-study-before').glob('*-034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299.jp2')),40980,10980,6)]:
 base=native.Decoder();roi=native.Decoder()
 for reduction in [0,2,maxreduce]:
  edge=512*2**reduction;cols=(w+edge-1)//edge;rows=(h+edge-1)//edge
  for i in range(12):
   x=(i*7%cols)*edge;y=(i*3%rows)*edge;bounds=[(x,y,min(x+edge,w),min(y+edge,h))]
   os.environ['RIVER_NVJPEG2000_PRECISE_ROI']='false';start=time.perf_counter();a,at=base.decode(path,w,h,bounds,reduction,100000000);fullms=(time.perf_counter()-start)*1000
   os.environ['RIVER_NVJPEG2000_PRECISE_ROI']='true';start=time.perf_counter();b,bt=roi.decode(path,w,h,bounds,reduction,100000000);roims=(time.perf_counter()-start)*1000
   delta=np.abs(a[0].astype(np.int16)-b[0].astype(np.int16))
   samples.append(dict(asset=path.name,reduction=reduction,index=i,fullMs=fullms,roiMs=roims,equalPixels=bool(np.array_equal(a[0],b[0])),maxError=int(delta.max())))
 base.close();roi.close()
print(json.dumps(dict(samples=samples),indent=2))
if not all(s['equalPixels'] for s in samples):raise SystemExit(1)
