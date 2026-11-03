"""Export immutable deployment images without text-shell binary redirection."""
import gzip, hashlib, json, shutil, subprocess, tarfile, datetime
from pathlib import Path
root = Path(__file__).resolve().parents[1]
package = root/'dist/river-portable-2026-10-12'
records = []
for kind in ['backend','ui']:
    tag = f'river/{kind}:2026.10.12'
    name = f'river-{kind}-linux-amd64.tar.gz'
    info = json.loads(subprocess.check_output(['docker','image','inspect',tag]))[0]
    assert info['Architecture']=='amd64' and info['Os']=='linux'
    archive = package/name
    process = subprocess.Popen(['docker','image','save',tag],stdout=subprocess.PIPE)
    with gzip.open(archive,'wb',compresslevel=6) as target:
        shutil.copyfileobj(process.stdout,target,1024*1024)
    process.stdout.close()
    if process.wait(): raise RuntimeError(f'Docker export failed: {tag}')
    with archive.open('rb') as source: digest = hashlib.file_digest(source,'sha256').hexdigest()
    records.append(dict(tag=tag,imageId=info['Id'],dockerReportedBytes=info['Size'],archive=name,archiveBytes=archive.stat().st_size,sha256=digest,platform='linux/amd64'))
    print(kind,archive.stat().st_size,'bytes',flush=True)

# Include the actual app/build source snapshot for reproducibility; no media
# corpus, Cargo build outputs, node_modules, credentials or prepared caches.
snapshot = package/'river-source.tar.gz'
with tarfile.open(snapshot,'w:gz') as tar:
    for name in ['Cargo.toml','Cargo.lock','Dockerfile.backend','Dockerfile.ui','crates','web','licenses','nvjpeg2000-sidecar','deploy']:
        def include(member):
            parts = Path(member.name).parts
            if any(p in ['__pycache__','target','node_modules'] or p=='.env' for p in parts): return None
            return member
        tar.add(root/name,arcname='river-source/'+name,filter=include)
    tar.add(Path(__file__),arcname='river-source/tools/export-portable.py')
with snapshot.open('rb') as source: source_digest=hashlib.file_digest(source,'sha256').hexdigest()
manifest = dict(version='2026.10.12',createdUtc=datetime.datetime.now(datetime.timezone.utc).isoformat(),images=records,sourceSnapshot=dict(file=snapshot.name,bytes=snapshot.stat().st_size,sha256=source_digest),validation=dict(cpu=json.loads((root/'reports/portable-cpu-validation.json').read_text()),gpu=json.loads((root/'reports/portable-gpu-pixels.json').read_text()),mosaic=json.loads((root/'reports/portable-mosaic-pixels.json').read_text())),dataIncluded='SHA-verified Bretagne2.j2k fixture only; large imagery and caches are external')
(package/'BUILD-MANIFEST.json').write_text(json.dumps(manifest,indent=2))
(package/'SHA256SUMS').write_text(''.join(f"{r['sha256']}  {r['archive']}\n" for r in records)+f'{source_digest}  {snapshot.name}\n')
subprocess.run(['docker','cp','river-portable-verify-river-1:/usr/share/doc/river/licenses/.',str(package/'licenses')],check=True)
for record in records:
    subprocess.run(['docker','image','load','--input',str(package/record['archive'])],check=True)
    loaded=json.loads(subprocess.check_output(['docker','image','inspect',record['tag']]))[0]
    assert loaded['Id']==record['imageId'], 'Export/load changed image identity'
bundle = shutil.make_archive(str(package),'zip',root_dir=package.parent,base_dir=package.name)
with open(bundle,'rb') as source: bundle_digest=hashlib.file_digest(source,'sha256').hexdigest()
Path(bundle+'.sha256').write_text(bundle_digest+'  '+Path(bundle).name+'\n')
print(json.dumps(dict(bundle=bundle,bundleBytes=Path(bundle).stat().st_size,sha256=bundle_digest,images=records),indent=2))
