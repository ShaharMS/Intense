import json,sys,time
from pathlib import Path
import numpy as np
sys.path.insert(0,'/app');from native import Decoder
root=Path('/var/lib/river/s3-cache/local-sources/ht-study');samples=[]
for tid in [137,224,351]:
 for reduction in [0,2,6]:
  reference=None
  for codec in ['original','ht']:
   decoder=Decoder();path=root/(f'{tid}.j2k' if codec=='original' else f'{tid}-ht.jph')
   for i in range(11):
    begin=time.perf_counter();images,timing=decoder.decode(path,1024,1024,[(0,0,512 if reduction==0 else 1024,512 if reduction==0 else 1024)],reduction,1000000)
    if reference is None:reference=images[0].copy()
    samples.append(dict(tile=tid,reduction=reduction,codec=codec,warmup=i==0,e2eMs=(time.perf_counter()-begin)*1000,equalPixels=bool(np.array_equal(images[0],reference)),**timing))
   decoder.close()
print(json.dumps(dict(samples=samples),indent=2))
if not all(s['equalPixels'] for s in samples):raise SystemExit(1)
