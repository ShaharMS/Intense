"""Read-only box-header audit. Seek over codestreams; never read image payloads."""
import argparse,json,struct
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--manifest',type=Path,required=True);p.add_argument('--max-boxes',type=int,default=4096);a=p.parse_args();assert 1<=a.max_boxes<=65536
manifest=json.loads(a.manifest.read_text());seen=set();rows=[]
superboxes={b'jp2h',b'res ',b'asoc',b'cidx',b'fidx',b'tpix',b'thix',b'ppix',b'phix'}
for c in manifest['cases']:
 path=(a.manifest.parent/c['file']).resolve()
 if path in seen:continue
 seen.add(path);row=dict(file=str(path),sourceBytes=path.stat().st_size,metadataBytesRead=0,boxes=[]);rows.append(row)
 with path.open('rb') as f:
  def read(at,n):
   assert row['metadataBytesRead']+n<=2*1024*1024,'metadata audit budget exceeded'
   f.seek(at);b=f.read(n);row['metadataBytesRead']+=len(b);assert len(b)==n,'truncated box header';return b
  def walk(start,end,depth):
   assert depth<=16,'box nesting exceeds audit limit'
   while start<end:
    assert len(row['boxes'])<a.max_boxes,'box count exceeds audit limit'
    n,kind=struct.unpack('>I4s',read(start,8));header=8
    if n==1:n=struct.unpack('>Q',read(start+8,8))[0];header=16
    if n==0:n=end-start
    assert n>=header and start+n<=end,'invalid box extent'
    record=dict(type=kind.decode('ascii',errors='replace'),offset=start,length=n,depth=depth);row['boxes'].append(record)
    if kind==b'iptr' and n-header==16:
     record['indexOffset'],record['indexLength']=struct.unpack('>QQ',read(start+header,16))
    if kind in superboxes:walk(start+header,start+n,depth+1)
    start+=n
  try:
   if read(0,2)==b'\xff\x4f':row['container']='raw_codestream'
   else:row['container']='boxed';walk(0,row['sourceBytes'],0)
   row['passed']=True
  except (AssertionError,ValueError) as e:row.update(passed=False,error=str(e))
print(json.dumps(dict(scope='Read-only box headers and16byte iptr payloads only; codestream and raster bytes skipped. No source-file changes.',files=rows),indent=2))
