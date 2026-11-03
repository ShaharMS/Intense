"""One-time authorized cleanup; archive writable files, never delete volumes."""
import gzip,json,posixpath,shutil,subprocess,tarfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1];BASE=ROOT/'reports/container-consolidation';BASE.mkdir(exist_ok=True)
ids=subprocess.check_output(['docker','ps','-aq','--filter','name=river-goal-'],text=True).split()
cs=json.loads(subprocess.check_output(['docker','inspect',*ids])) if ids else []
assert all(c['Name'].startswith('/river-goal-') and not (c['Config'].get('Labels') or {}).get('com.docker.compose.project') for c in cs)
running=[c['Id'] for c in cs if c['State']['Running']]
if running:subprocess.run(['docker','stop','--time','3',*running],check=True,stdout=subprocess.DEVNULL)
# Carry the two protected roles' existing configuration into their new slots.
for name,slot in [('river-goal-refresh830-candidate','stable'),('river-goal-learned811-candidate','lts')]:
 if any(c['Name']=='/'+name for c in cs):
  target=ROOT/'deploy/local-state'/slot/'config';target.mkdir(parents=True,exist_ok=True)
  subprocess.run(['docker','cp',name+':/var/lib/river/config/.',str(target)],check=True)
for n in range(1,5):
 target=ROOT/'deploy/local-state'/f'replica-{n}'/'config'
 if not target.exists():shutil.copytree(ROOT/'deploy/local-state/stable/config',target)
report=dict(removed=[],volumesDeleted=0,mountsPreserved=True,archives=[])
for c in cs:
 name=c['Name'].lstrip('/');folder=BASE/'state'/name;folder.mkdir(parents=True,exist_ok=False)
 (folder/'inspect.json').write_text(json.dumps(c,indent=2));(folder/'logs.txt').write_text(subprocess.check_output(['docker','logs',name],text=True,stderr=subprocess.STDOUT))
 diff=subprocess.check_output(['docker','diff',c['Id']],text=True);(folder/'diff.txt').write_text(diff)
 entries=[(line[0],line[2:]) for line in diff.splitlines() if line]
 paths=[p for _,p in entries];added=[p for kind,p in entries if kind=='A']
 roots=[p for p in added if not any(p.startswith(a+'/') for a in added if a!=p)]
 roots += [p for kind,p in entries if kind=='C' and not any(a.startswith(p+'/') for a in paths if a!=p)]
 mounts=[m['Destination'].rstrip('/') for m in c['Mounts']]
 roots=[p for p in roots if not any(p==m or p.startswith(m+'/') for m in mounts)]
 if name=='river-goal-openht-runtime':roots=[p for p in roots if not (p=='/tmp' or p.startswith('/tmp/'))]+['/tmp']
 roots=sorted(set(roots));(folder/'archived-paths.json').write_text(json.dumps(roots,indent=2))
 if roots:
  archive=folder/'writable-files.tar.gz'
  with archive.open('wb') as raw,gzip.GzipFile(fileobj=raw,mode='wb',compresslevel=1) as gz,tarfile.open(fileobj=gz,mode='w|') as dest:
   for path in roots:
    with (folder/'copy-errors.log').open('ab') as errors:
     child=subprocess.Popen(['docker','cp',name+':'+path,'-'],stdout=subprocess.PIPE,stderr=errors)
     with tarfile.open(fileobj=child.stdout,mode='r|') as source:
      for member in source:
       parent=posixpath.dirname(path).lstrip('/')
       while member.name.startswith('./'):member.name=member.name[2:]
       member.name=posixpath.join(parent,member.name.lstrip('/'))
       assert '..' not in member.name.split('/')
       if member.islnk() and not member.linkname.startswith('/'):member.linkname=posixpath.join(parent,member.linkname)
       stream=source.extractfile(member) if member.isfile() else None
       dest.addfile(member,stream)
     assert child.wait()==0,(name,path)
  report['archives'].append(dict(name=name,compressedBytes=archive.stat().st_size,paths=len(roots)))
 subprocess.run(['docker','rm',c['Id']],check=True,stdout=subprocess.DEVNULL)
 report['removed'].append(name);(BASE/'cleanup.json').write_text(json.dumps(report,indent=2));print(json.dumps(dict(removed=name,remaining=len(cs)-len(report['removed']))),flush=True)
print(json.dumps(dict(removedContainers=len(report['removed']),volumesDeleted=0,archives=len(report['archives']))),flush=True)
