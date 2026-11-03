import ctypes as C,json,sys,time
from pathlib import Path
import numpy as np
sys.path.insert(0,'/app');from native import Decoder
sys.path.insert(0,'/tmp');from rpc_codec import encode,lz4
asset='034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299'
path=next(Path('/var/lib/river/s3-cache/local-sources').glob('river-source-*-'+asset+'.jp2'))
decoder=Decoder();samples=[]
for position,(x,y) in enumerate([(0,0),(7168,512),(14000,1000),(32000,8192)]):
 rgba=decoder.decode(path,40980,10980,[(x,y,x+512,y+512)],0,1000000)[0][0]
 for codec in ['raw','lz4','delta-lz4']:
  for i in range(10):
   start=time.perf_counter();payload=encode(rgba,codec);elapsed=(time.perf_counter()-start)*1000
   if codec=='raw':same=True
   else:
    source=C.create_string_buffer(payload);out=C.create_string_buffer(rgba.nbytes)
    size=lz4.LZ4_decompress_safe(source,out,len(payload),rgba.nbytes)
    decoded=np.frombuffer(out,dtype=np.uint8).reshape(rgba.shape)
    if codec=='delta-lz4':decoded=np.cumsum(decoded,axis=1,dtype=np.uint8)
    same=size==rgba.nbytes and np.array_equal(decoded,rgba)
   samples.append(dict(position=position,codec=codec,encodeMs=elapsed,bytes=len(payload),rawBytes=rgba.nbytes,equalPixels=bool(same)))
decoder.close();print(json.dumps(dict(samples=samples),indent=2))
if not all(s['equalPixels'] for s in samples):raise SystemExit(1)
