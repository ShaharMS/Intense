"""Lossless TLM-only derivative experiment; original input is never changed."""
import concurrent.futures,hashlib,json,mmap,os,shutil,statistics,struct,subprocess,tempfile,time
from pathlib import Path
source=next(Path('/var/lib/river/s3-cache/local-sources/cold-study-before').glob('*-034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299.jp2'))
output=Path('/tmp/river-tlm-index.jp2')
start=time.perf_counter()
with source.open('rb') as f,mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as data:
 box=0
 while True:
  size,kind=struct.unpack_from('>I4s',data,box)
  if kind==b'jp2c':break
  if size<8:raise ValueError('unsupported JP2 box layout')
  box+=size
 cursor=box+10
 while struct.unpack_from('>H',data,cursor)[0]!=0xff90:cursor+=2+struct.unpack_from('>H',data,cursor+2)[0]
 insert=cursor;parts=[]
 while struct.unpack_from('>H',data,cursor)[0]==0xff90:
  tile=struct.unpack_from('>H',data,cursor+4)[0];length=struct.unpack_from('>I',data,cursor+6)[0]
  if length<12:raise ValueError('unsupported open-ended tile part')
  parts.append((tile,length));cursor+=length
 segment=b'\xff\x55'+struct.pack('>HBB',4+6*len(parts),0,0x60)+b''.join(struct.pack('>HI',tile,length) for tile,length in parts)
 prefix=bytearray(data[:insert]);struct.pack_into('>I',prefix,box,size+len(segment))
 with output.open('wb') as out:
  out.write(prefix);out.write(segment);f.seek(insert);shutil.copyfileobj(f,out,1024*1024)
created_ms=(time.perf_counter()-start)*1000
samples=[];references={};errors=[]
for indexed,path in [(False,source),(True,output)]:
 for concurrency in [1,8]:
  def run(i):
   x=(i*7%81)*512;y=(i%22)*512
   with tempfile.TemporaryDirectory() as folder:
    cmd=['grk_decompress','-i',str(path),'-o',folder+'/tile.pam','-d',f'{x},{y},{min(x+512,40980)},{min(y+512,10980)}','-r','0']
    started=time.perf_counter();r=subprocess.run(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE);elapsed=(time.perf_counter()-started)*1000
    if r.returncode:errors.append(r.stderr.decode()[:300]);return
    digest=hashlib.sha256(Path(folder+'/tile.pam').read_bytes()).hexdigest()
    if not indexed:references[(concurrency,i)]=digest
    samples.append(dict(indexed=indexed,concurrency=concurrency,ms=elapsed,equalRaster=not indexed or references[(concurrency,i)]==digest))
  with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:list(pool.map(run,range(32)))
print(json.dumps(dict(source=str(source),output=str(output),tlmEntries=len(parts),indexBytes=len(segment),createdMs=created_ms,samples=samples,errors=errors),indent=2))
if errors or any(not s['equalRaster'] for s in samples):raise SystemExit(1)
