"""Inspect River references and retire superseded named images; never force removal."""
import argparse,json,os,re,shutil,subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser();p.add_argument('--apply',action='store_true');p.add_argument('--report',type=Path);p.add_argument('--engine',default=os.environ.get('RIVER_CONTAINER_ENGINE') or ('docker' if shutil.which('docker') else 'podman'));a=p.parse_args()
def run(*args):return subprocess.check_output([a.engine,*args],text=True).strip()
def normalize(tag):return tag.removeprefix('docker.io/library/').removeprefix('localhost/')
release=json.loads((ROOT/'deploy/releases.json').read_text());policy=json.loads((ROOT/'deploy/image-retention.json').read_text())
keep_tags={release[k] for k in ['stable','lts','latest'] if release.get(k)}|set(policy.get('pinnedTags',[]))
keep_ids={json.loads(run('image','inspect',tag))[0]['Id'] for tag in keep_tags}
assert len(keep_ids)<=policy['maxVersions'],'Release pins exceed retention policy'
managed={}
for tag in run('image','ls','--format','{{.Repository}}:{{.Tag}}').splitlines():
 name=normalize(tag)
 if name.startswith('river:') and name!='river:<none>':managed[tag]=json.loads(run('image','inspect',tag))[0]['Id']
ids=run('ps','-aq').split();containers=json.loads(run('inspect',*ids)) if ids else []
references={c['Image'] for c in containers}
blocked=[c['Name'] for c in containers if c['Image'] in set(managed.values())-keep_ids]
remove=[tag for tag,identity in managed.items() if identity not in keep_ids|references]
report=dict(applied=a.apply,retainedTags=[tag for tag in managed if tag not in remove],removeTags=remove,blockedContainers=blocked,beforeVersions=len(set(managed.values())),withinLimit=len(set(managed.values())&references|keep_ids)<=policy['maxVersions'])
if a.apply and remove:run('image','rm',*remove)
if a.report:a.report.parent.mkdir(parents=True,exist_ok=True);a.report.write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report,indent=2))
if not report['withinLimit']:raise SystemExit('Referenced superseded images remain; no force removal attempted')
