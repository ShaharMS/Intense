"""Scoped, non-forced River checkpoint retention; dry-run unless --apply."""
import argparse,json,pathlib,re,subprocess
p=argparse.ArgumentParser();p.add_argument('--apply',action='store_true');p.add_argument('--report',type=pathlib.Path);a=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[1];policy=json.loads((root/'deploy/image-retention.json').read_text());repo=policy['repository']
assert repo=='river' and policy['ownedTestContainerPrefix']=='river-goal-','This helper only manages River checkpoints and its stopped test containers'
def docker(*args):return subprocess.check_output(['docker',*args],text=True).strip()
rows=[json.loads(line) for line in docker('image','ls','--no-trunc','--format','{{json .}}',repo).splitlines() if line]
owned_tag=re.compile(r'(?:20\d{2}\..*-m|(?:lts|stable)-)(\d+)(?:[-.].*)?')
channel_tags={'river:'+channel for channel in ['latest','stable','lts']}
def managed(row):
    return row['Repository']==repo and (owned_tag.fullmatch(row['Tag']) or row['Repository']+':'+row['Tag'] in channel_tags)
tags={row['Repository']+':'+row['Tag']:row['ID'] for row in rows if managed(row)}
ids=set(tags.values());details=json.loads(docker('image','inspect',*sorted(ids))) if ids else []
# Cached builds may retain an older image's Created timestamp. Checkpoint tags
# record release order, including different images with the same timestamp.
def release_order(image):
    # Versioned channel aliases must not retain obsolete image IDs forever.
    versions=[int(match[1]) for tag,identity in tags.items() if identity==image['Id'] and (match:=owned_tag.fullmatch(tag.split(':',1)[1]))]
    return max(versions,default=-1),image['Created']
ordered=sorted(details,key=release_order,reverse=True);minimum_supported=int(policy.get('minimumSupportedCheckpoint',0));eligible=[image for image in ordered if release_order(image)[0]>=minimum_supported];keep={image['Id'] for image in eligible[:policy['keepNewest']]}
orders={image['Id']:release_order(image)[0] for image in details}
missing=[tag for tag in policy['pinnedTags'] if tag not in tags];assert not missing,f'Missing pinned images: {missing}'
keep.update(tags[tag] for tag in policy['pinnedTags']);assert len(keep)<=policy['maxVersions'],'Retention pins exceed maximum versions'
assert all(orders[identity]>=minimum_supported for identity in keep),'Retention pin predates the required shared compatibility contract'
release_path=root/'deploy/releases.json'
if release_path.exists():
    release=json.loads(release_path.read_text())
    for channel in ['latest','stable','lts']:
        tag=release.get(channel)
        if tag:
            assert tag in tags,f'Missing {channel} image: {tag}'
            assert orders[tags[tag]]>=minimum_supported,f'{channel} predates the required compatibility floor; update channels before cleanup'
            keep.add(tags[tag])
            alias='river:'+channel
            if alias in tags:assert tags[alias]==tags[tag],f'{alias} differs from deploy/releases.json; update channels before cleanup'
    rollback = tags.get(release.get('previousStable'))
    if rollback and orders[rollback] >= minimum_supported and len(keep) < policy['maxVersions']:
        keep.add(rollback)
assert len(keep)<=policy['maxVersions'],'Release channels and retention pins exceed maximum versions'
containers=docker('ps','-aq').splitlines();refs=[]
if containers:
    template='{"id":"{{.Id}}","image":"{{.Image}}","name":"{{.Name}}","running":{{.State.Running}}}'
    refs=[json.loads(line) for line in docker('inspect','--format',template,*containers).splitlines()]
obsolete=ids-keep;owned=[c for c in refs if c['image'] in obsolete and not c['running'] and c['name'].lstrip('/').startswith(policy['ownedTestContainerPrefix'])]
owned_ids={c['id'] for c in owned};blocked=[c for c in refs if c['image'] in obsolete and c['id'] not in owned_ids];blocked_images={c['image'] for c in blocked}
remove_tags=sorted(tag for tag,image in tags.items() if image in obsolete-blocked_images)
report={'applied':a.apply,'repository':repo,'beforeVersions':len(ids),'retainedTags':sorted(tag for tag,image in tags.items() if image in keep|blocked_images),'removeTags':remove_tags,'removeStoppedOwnedContainers':[c['name'] for c in owned],'blockedContainers':[c['name'] for c in blocked]}
if a.apply:
    if owned:docker('rm',*(c['id'] for c in owned))
    if remove_tags:docker('image','rm',*remove_tags)
    after=[json.loads(line) for line in docker('image','ls','--no-trunc','--format','{{json .}}',repo).splitlines() if line]
    retained={row['Repository']+':'+row['Tag']:row['ID'] for row in after if managed(row)}
    report['afterVersions']=len(set(retained.values()));report['retainedTags']=sorted(retained)
report['withinLimit']=report.get('afterVersions',len(keep|blocked_images))<=policy['maxVersions']
if a.report:
    a.report.parent.mkdir(parents=True,exist_ok=True);a.report.write_text(json.dumps(report,indent=2))
    print(json.dumps({k:report[k] for k in ['applied','beforeVersions','retainedTags','blockedContainers','withinLimit']}|{'removedTags':len(remove_tags),'stoppedOwnedContainers':len(owned),'afterVersions':report.get('afterVersions'),'report':str(a.report)},indent=2))
else:print(json.dumps(report,indent=2))
if not report['withinLimit']:raise SystemExit('Image retention blocked by container references; no force removal attempted')
