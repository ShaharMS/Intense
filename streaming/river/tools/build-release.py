"""Freeze and build a single River release; never promotes an unqualified image."""
import argparse,hashlib,json,os,shutil,subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('version');p.add_argument('--offline',action='store_true',help='Use an already populated Cargo cache without registry network access');p.add_argument('--engine',default=os.environ.get('RIVER_CONTAINER_ENGINE','docker'));p.add_argument('--source-root',type=Path,default=ROOT);a=p.parse_args()
 if not a.version or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_' for c in a.version):p.error('Invalid version')
 build=ROOT/'target/builds'/a.version;source=build/'source';source.mkdir(parents=True,exist_ok=False)
 for name in ['Cargo.toml','Cargo.lock','Dockerfile.backend','.dockerignore','rust-toolchain.toml','crates','licenses','nvjpeg2000-sidecar']:
  src=a.source_root/name;dst=source/name
  if src.is_dir():shutil.copytree(src,dst,ignore=shutil.ignore_patterns('__pycache__','target','.env','.env.*'))
  else:shutil.copyfile(src,dst)
 for name in ['backend','swagger','openapi.json']:
  src=a.source_root/'deploy'/name;dst=source/'deploy'/name;dst.parent.mkdir(parents=True,exist_ok=True)
  if src.is_dir():shutil.copytree(src,dst,ignore=shutil.ignore_patterns('__pycache__'))
  else:shutil.copyfile(src,dst)
 spec=source/'deploy/openapi.json';document=json.loads(spec.read_text());document['info']['version']=a.version;spec.write_text(json.dumps(document,indent=2)+'\n')
 files=[dict(path=f.relative_to(source).as_posix(),sha256=hashlib.sha256(f.read_bytes()).hexdigest()) for f in sorted(source.rglob('*')) if f.is_file()]
 tag='river:'+a.version
 (build/'manifest.json').write_text(json.dumps(dict(version=a.version,tag=tag,sources=files),indent=2)+'\n')
 status=build/'status.json';status.write_text(json.dumps(dict(state='building',tag=tag)))
 with (build/'build.log').open('w') as log:
  result=subprocess.run([a.engine,'build','--layers','--platform','linux/amd64','--build-arg','RIVER_BUILD_VERSION='+a.version,'--build-arg','RIVER_CARGO_OFFLINE='+str(a.offline).lower(),'-f','Dockerfile.backend','-t',tag,'.'],cwd=source,stdout=log,stderr=subprocess.STDOUT)
 if result.returncode:
  status.write_text(json.dumps(dict(state='failed',tag=tag,exitCode=result.returncode)));raise SystemExit(result.returncode)
 image=json.loads(subprocess.check_output([a.engine,'image','inspect',tag],text=True))[0]
 status.write_text(json.dumps(dict(state='built',tag=tag,imageId='sha256:'+image.get('Id',image.get('ID')).removeprefix('sha256:'),qualification='pending'),indent=2)+'\n')
 print(status.read_text())
if __name__=='__main__':main()
