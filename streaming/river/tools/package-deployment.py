"""Package one verified API-only image, deployment templates and upgrade guidance."""
import argparse,gzip,hashlib,json,os,shutil,subprocess,zipfile,tarfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
def digest(path):
 h=hashlib.sha256()
 with path.open('rb') as f:
  while chunk:=f.read(1024*1024):h.update(chunk)
 return h.hexdigest()
def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('version');p.add_argument('--image',required=True);p.add_argument('--evidence',type=Path,required=True);p.add_argument('--engine',default=os.environ.get('RIVER_CONTAINER_ENGINE','docker'));p.add_argument('--channel-tags',action='store_true',help='Include the three verified local release aliases in the image archive');p.add_argument('--comparison',type=Path,help='HTML comparison for this release');p.add_argument('--package-only',action='store_true',help='Export a digest-bound build record without release tests or a benchmark report');a=p.parse_args()
 if not a.version or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_' for c in a.version):p.error('version must be a simple release identifier')
 proof=json.loads(a.evidence.read_text())
 if a.package_only and (proof.get('qualifiedForRelease') is not False or proof.get('releaseTests')!='skipped' or a.comparison):p.error('Package-only requires a skipped-test record and no comparison')
 if not a.package_only and not proof.get('qualifiedForRelease'):p.error('Evidence must qualify the exact image before packaging')
 info=json.loads(subprocess.check_output([a.engine,'image','inspect',a.image],text=True))[0]
 image_id='sha256:'+info.get('Id',info.get('ID')).removeprefix('sha256:')
 if proof.get('imageId')!=image_id:p.error('Evidence image digest does not match the packaged image')
 source_manifest=ROOT/'target/builds'/a.version/'manifest.json'
 if not source_manifest.exists() or proof.get('sourceManifestSha256')!=digest(source_manifest):p.error('Evidence must identify the frozen source manifest')
 folder=ROOT/'target/deploy'/a.version;folder.mkdir(parents=True,exist_ok=False)
 refs=[a.image]
 if a.channel_tags:
  repository=a.image.rsplit(':',1)[0]
  for channel in ['stable','lts','latest']:
   ref=repository+':'+channel
   alias=json.loads(subprocess.check_output([a.engine,'image','inspect',ref],text=True))[0]
   if alias.get('Id',alias.get('ID')).removeprefix('sha256:')!=image_id.removeprefix('sha256:'):raise RuntimeError('Channel image mismatch: '+ref)
   refs.append(ref)
 image=folder/'river-image.tar.gz'
 with image.open('wb') as raw,gzip.GzipFile(fileobj=raw,mode='wb',mtime=0) as output:
  proc=subprocess.Popen([a.engine,'save','--format','docker-archive',*refs] if a.engine=='podman' else [a.engine,'save',*refs],stdout=subprocess.PIPE)
  shutil.copyfileobj(proc.stdout,output)
  if proc.wait():raise RuntimeError('Image export failed')
 with tarfile.open(image,'r:gz') as exported:
  manifest=json.load(exported.extractfile('manifest.json'))
  tags=sorted({tag for entry in manifest for tag in entry.get('RepoTags',[])})
  if len({entry['Config'] for entry in manifest})!=1:raise RuntimeError('Export includes multiple images')
  if a.channel_tags and set(refs)!=set(tags):raise RuntimeError(f'Exported tags {tags} differ from requested {refs}')
  deployment_image=next((tag for tag in tags if tag.endswith(':'+a.version)),None)
  if not deployment_image:raise RuntimeError('Exported archive has no deployable image tag')
  config=exported.extractfile(manifest[0]['Config']).read()
  if 'sha256:'+hashlib.sha256(config).hexdigest()!=image_id:raise RuntimeError('Exported image config does not match qualified image')
 shutil.copyfile(source_manifest,folder/'SOURCE-MANIFEST.json')
 compose=f'''name: river
services:
  replica-1:
    image: {deployment_image}
    pull_policy: never
    container_name: replica-1
    env_file: .env
    ports:
      - "${{RIVER_HTTP_PORT:-8080}}:8080"
      - "${{RIVER_HTTPS_PORT:-8443}}:8443"
      - "${{RIVER_HTTPS_PORT:-8443}}:8443/udp"
    volumes:
      - ./config:/var/lib/river/config:z
      - ./corpus:/opt/river/corpus:ro,z
      - ./certificates:/run/river-tls:ro,z
    shm_size: ${{RIVER_SHM_SIZE:-512m}}
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "python3", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/health', timeout=4)"]
      interval: 10s
      timeout: 5s
      start_period: 180s
      retries: 6
'''
 (folder/'compose.yaml').write_text(compose)
 (folder/'compose.cache.yaml').write_text('services:\n  replica-1:\n    volumes:\n      - river-cache:/var/lib/river/s3-cache\nvolumes:\n  river-cache: {}\n')
 (folder/'compose.gpu.yaml').write_text('services:\n  replica-1:\n    gpus: all\n    environment:\n      RIVER_ENABLE_NVIDIA: "true"\n')
 (folder/'.env.example').write_text('RIVER_CONFIG_MODE=json\nRIVER_CONFIGURATION_FILE=/var/lib/river/config/configuration.json\nRIVER_AUTO_ADAPTATION_MODE=liberal\nRIVER_CACHE_LEARNING=true\nRIVER_ENABLE_NVIDIA=auto\nRIVER_ADMIN_TOKEN=\nRIVER_S3_CACHE_ROOT=/var/lib/river/s3-cache\n')
 (folder/'start.sh').write_text('#!/bin/sh\nset -eu\ncd "$(dirname "$0")"\nengine=${RIVER_CONTAINER_ENGINE:-docker}\n[ -f .env ] || cp .env.example .env\nmkdir -p config corpus certificates\n"$engine" load -i river-image.tar.gz\n"$engine" compose -p river -f compose.yaml up -d\n')
 (folder/'start.ps1').write_text('$ErrorActionPreference="Stop"\nSet-Location $PSScriptRoot\nif (!(Test-Path .env)) {Copy-Item .env.example .env}\nNew-Item -ItemType Directory -Force config,corpus,certificates | Out-Null\ndocker load -i river-image.tar.gz\nif ($LASTEXITCODE) {throw "Image load failed"}\ndocker compose -p river -f compose.yaml up -d\nif ($LASTEXITCODE) {throw "Deployment failed"}\n')
 for guide in sorted((ROOT/'docs').glob('UPGRADE-FROM-*.md')):
  shutil.copyfile(guide,folder/guide.name)
 for name in ['CACHE-GUIDE.md','DEPLOYMENT.md','SETTINGS.md']:
  shutil.copyfile(ROOT/'docs'/name,folder/name)
 shutil.copyfile(ROOT/'deploy/portable/agent-handoff/AGENTS.md',folder/'AGENTS.md')
 shutil.copyfile(source_manifest.parent/'source/deploy/openapi.json',folder/'openapi.json')
 shutil.copyfile(a.evidence,folder/'QUALIFICATION.json')
 if not a.package_only:
  comparison=a.comparison or ROOT/'target/benchmarks/comparison.html'
  if not comparison.exists():raise RuntimeError('Completed before/after comparison report is required')
  shutil.copyfile(comparison,folder/'COMPARISON.html')
 (folder/'BUILD-MANIFEST.json').write_text(json.dumps({'version':a.version,'image':deployment_image,'imageId':image_id,'exportedTags':tags,'archiveSha256':digest(image),'channels':['stable','lts','latest'],'includes':['API','Swagger','CPU codecs','optional NVIDIA codecs']},indent=2)+'\n')
 (folder/'SHA256SUMS').write_text(''.join(digest(f)+'  '+f.name+'\n' for f in sorted(folder.iterdir()) if f.is_file()))
 archive=ROOT/'target/dist'/('river-deploy-'+a.version+'.zip');archive.parent.mkdir(parents=True,exist_ok=True)
 with zipfile.ZipFile(archive,'w',compression=zipfile.ZIP_STORED) as z:
  for f in sorted(folder.iterdir()):z.write(f,f.name)
 with zipfile.ZipFile(archive) as z:
  if z.testzip():raise RuntimeError('ZIP verification failed')
 archive.with_suffix('.zip.sha256').write_text(digest(archive)+'  '+archive.name+'\n')
 print(json.dumps({'archive':str(archive),'sha256':digest(archive),'imageId':image_id}))
if __name__=='__main__':main()
