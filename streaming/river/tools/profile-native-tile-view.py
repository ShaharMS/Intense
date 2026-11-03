import json,mmap,struct,tempfile,time,sys
from pathlib import Path
import numpy as np
sys.path.insert(0,'/app');from native import Decoder
asset='034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299'
path=next(Path('/var/lib/river/s3-cache/local-sources').glob('river-source-*-'+asset+'.jp2'))
samples=[]
with path.open('rb') as f,mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as b,tempfile.TemporaryDirectory() as directory:
 start=0
 while b[start+4:start+8]!=b'jp2c':start+=struct.unpack_from('>I',b,start)[0]
 start+=8;pos=start+2;headers=[]
 while b[pos:pos+2]!=b'\xff\x90':
  marker,length=struct.unpack_from('>HH',b,pos);headers.append((marker,b[pos+4:pos+length+2]));pos+=length+2
 parts={}
 while b[pos:pos+2]!=b'\xff\xd9':
  tid,size=struct.unpack_from('>HI',b,pos+4);parts[tid]=(pos,size);pos+=size
 for tid in [0,137,450]:
  tx,ty=tid%41,tid//41;w,h=min(1024,40980-tx*1024),min(1024,10980-ty*1024)
  data=bytearray(b'\xffO')
  for marker,payload in headers:
   if marker==0xff51:payload=struct.pack('>H8IH',0,w,h,0,0,1024,1024,0,0,3)+payload[36:]
   data+=struct.pack('>HH',marker,len(payload)+2)+payload
  offset,size=parts[tid];tile=bytearray(b[offset:offset+size]);struct.pack_into('>H',tile,4,0);data+=tile+b'\xff\xd9'
  proxy=Path(directory)/f'{tid}.j2k';proxy.write_bytes(data)
  for reduction in [0,2]:
   bw,bh=min(w,512*(1<<reduction)),min(h,512*(1<<reduction));bounds=[(tx*1024,ty*1024,tx*1024+bw,ty*1024+bh)]
   reference=None
   for mode in ['original','compact']:
    decoder=Decoder()
    for i in range(11):
     begin=time.perf_counter()
     output,timing=decoder.decode(path if mode=='original' else proxy,40980 if mode=='original' else w,10980 if mode=='original' else h,bounds if mode=='original' else [(0,0,bw,bh)],reduction,100000000)
     if reference is None:reference=output[0].copy()
     samples.append(dict(mode=mode,tile=tid,reduction=reduction,warmup=i==0,e2eMs=(time.perf_counter()-begin)*1000,equalPixels=bool(np.array_equal(output[0],reference)),**timing))
    decoder.close()
print(json.dumps(dict(samples=samples),indent=2))
if not all(s['equalPixels'] for s in samples):raise SystemExit(1)
