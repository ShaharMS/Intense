import json,mmap,struct,subprocess,time
from pathlib import Path
asset='034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299'
path=next(Path('/var/lib/river/s3-cache/local-sources').glob('river-source-*-'+asset+'.jp2'))
root=Path('/var/lib/river/s3-cache/local-sources/ht-study');root.mkdir(exist_ok=True)
samples=[]
with path.open('rb') as f,mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as b:
 start=0
 while b[start+4:start+8]!=b'jp2c':start+=struct.unpack_from('>I',b,start)[0]
 start+=8;pos=start+2;headers=[]
 while b[pos:pos+2]!=b'\xff\x90':
  marker,length=struct.unpack_from('>HH',b,pos);headers.append((marker,b[pos+4:pos+length+2]));pos+=length+2
 parts={}
 while b[pos:pos+2]!=b'\xff\xd9':
  tid,size=struct.unpack_from('>HI',b,pos+4);parts[tid]=(pos,size);pos+=size
 for tid in [137,224,351]:
  data=bytearray(b'\xffO')
  for marker,payload in headers:
   if marker==0xff51:payload=struct.pack('>H8IH',0,1024,1024,0,0,1024,1024,0,0,3)+payload[36:]
   data+=struct.pack('>HH',marker,len(payload)+2)+payload
  offset,size=parts[tid];tile=bytearray(b[offset:offset+size]);struct.pack_into('>H',tile,4,0);data+=tile+b'\xff\xd9'
  original=root/f'{tid}.j2k';original.write_bytes(data);raw=root/f'{tid}.ppm';ht=root/f'{tid}-ht.jph';begin=time.perf_counter()
  subprocess.run(['grk_decompress','-i',str(original),'-o',str(raw),'-H','1'],check=True,capture_output=True)
  decode_ms=(time.perf_counter()-begin)*1000;begin=time.perf_counter()
  result=subprocess.run(['grk_compress','-i',str(raw),'-o',str(ht),'-n','7','-t','1024,1024','-b','64,64','-Y','0','-M','64','-Z','16384','-H','1'],capture_output=True)
  samples.append(dict(tile=tid,decodeMs=decode_ms,encodeMs=(time.perf_counter()-begin)*1000,status=result.returncode,error=(result.stdout+result.stderr).decode()[-1000:],originalBytes=len(data),htBytes=ht.stat().st_size if ht.exists() else 0))
print(json.dumps(dict(samples=samples),indent=2))
if any(s['status'] for s in samples):raise SystemExit(1)
