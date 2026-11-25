"""Materialize byte-identical benchmark sources on the VM; never overwrite originals."""
import hashlib,json,os,shutil,time
from pathlib import Path
root=Path('/river/reports');dest=Path('/data/reports');manifest=root/'fixed-gate-v2/manifest.json';raw=manifest.read_bytes();m=json.loads(raw);rows=[];unique={}
for c in m['cases']:unique[(manifest.parent/c['file']).resolve()]=c
needed=sum(c['sourceBytes'] for c in unique.values());assert shutil.disk_usage('/data').free>needed+1024**3,'Insufficient VM fixture space'
for source,c in unique.items():
 target=dest/source.relative_to(root);target.parent.mkdir(parents=True,exist_ok=True);started=time.monotonic()
 if target.exists():
  assert target.stat().st_size==c['sourceBytes']
  with target.open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==c['sha256']
  rows.append({'file':str(target),'reused':True,'sha256':c['sha256']});continue
 temporary=target.with_suffix(target.suffix+'.copying');assert not temporary.exists(),'Partial copy exists; inspect before retry'
 before=source.stat();digest=hashlib.sha256();written=0
 with source.open('rb') as src,temporary.open('xb') as dst:
  while chunk:=src.read(8*1024*1024):dst.write(chunk);digest.update(chunk);written+=len(chunk)
  dst.flush();os.fsync(dst.fileno())
 after=source.stat();assert (before.st_size,before.st_mtime_ns)==(after.st_size,after.st_mtime_ns)
 assert written==c['sourceBytes'] and digest.hexdigest()==c['sha256']
 with temporary.open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==c['sha256']
 temporary.rename(target);row={'file':str(target),'bytes':written,'sha256':c['sha256'],'independentlyReadBack':True,'elapsedSeconds':time.monotonic()-started};rows.append(row);print(json.dumps(row),flush=True)
new_manifest=dest/'fixed-gate-v2/manifest.json';new_manifest.parent.mkdir(parents=True,exist_ok=True)
if new_manifest.exists():assert new_manifest.read_bytes()==raw
else:new_manifest.write_bytes(raw)
Path('/data/materialized.json').write_text(json.dumps({'manifestSha256':hashlib.sha256(raw).hexdigest(),'files':rows},indent=2)+'\n')
