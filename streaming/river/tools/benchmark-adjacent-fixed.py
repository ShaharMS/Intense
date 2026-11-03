"""Four-pair same-source adjacent-tile benchmark; real independent PNG references."""
import argparse,hashlib,importlib.util,io,json,statistics,subprocess,time,uuid
from pathlib import Path
from urllib.parse import urlencode
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--baseline-api',required=True);p.add_argument('--candidate-api',required=True);p.add_argument('--baseline-container',required=True);p.add_argument('--candidate-container',required=True);p.add_argument('--origin',required=True);p.add_argument('--origin-control',required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();assert not a.output.exists()
sp=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(sp);sp.loader.exec_module(h)
root=Path('reports/fixed-gate-v2');m=json.loads((root/'manifest.json').read_text());case=next(c for c in m['cases'] if c['name']=='gb11-deep');gold={68:Image.open(root/'references/eleven-deep.png').convert('RGB'),69:Image.open(root/'references/eleven-neighbor.png').convert('RGB')};versions={v:dict(api=getattr(a,v+'_api'),container=getattr(a,v+'_container')) for v in ['baseline','candidate']}
for v in versions.values():assert v['container'].startswith('river-goal-');v['imageId']=json.loads(subprocess.check_output(['docker','inspect',v['container']]))[0]['Image']
cfgs={v:json.loads(subprocess.check_output(['docker','inspect',versions[v]['container']]))[0] for v in versions}
assert sorted(cfgs['baseline']['Config']['Env'])==sorted(cfgs['candidate']['Config']['Env']),'Server environments differ'
for key in ['Memory','NanoCpus','CpuQuota','CpuPeriod','CpusetCpus']:assert cfgs['baseline']['HostConfig'][key]==cfgs['candidate']['HostConfig'][key],key
for v in versions:versions[v]['readPolicy']=json.loads(h.http(versions[v]['api']+'/api/settings/read-policy'))['settings']
assert versions['baseline']['readPolicy']==versions['candidate']['readPolicy'],'Read policies differ'
raw=(root/'manifest.json').read_bytes();origin=json.loads(h.http(a.origin_control+'/__manifest'));assert origin['sha256']==h.sha(raw)
probe="import json,sys,urllib.request;print(json.dumps(json.load(urllib.request.urlopen(sys.argv[1]+'/__manifest',timeout=3))))"
reachable=json.loads(subprocess.check_output(['docker','exec',a.baseline_container,'python3','-c',probe,a.origin],text=True));assert reachable['sha256']==origin['sha256'] and reachable.get('responseDelayMs')==origin.get('responseDelayMs')
with (root/case['file']).resolve().open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==case['sha256']
assert versions['baseline']['imageId']==subprocess.check_output(['docker','image','inspect','river:stable','--format','{{.Id}}'],text=True).strip()
rows=[];report=dict(passed=False,versions=versions,rows=rows,manifestSha256=h.sha(raw),runnerSha256=h.sha(Path(__file__).read_bytes()),origin=origin,scope='Four balanced process-cold cohorts per version. First deep tile followed by adjacent tile on same object/connection; both independent refs checked outside timers. Repeated exact responses require zero source IO. Origin OS cache warm; not physical-disk cold.')
try:
 for trial in range(4):
  for label in (['baseline','candidate'] if trial%2==0 else ['candidate','baseline']):
   v=versions[label];subprocess.run(['docker','restart',v['container']],check=True,stdout=subprocess.DEVNULL)
   for _ in range(100):
    try:h.http(v['api']+'/api/drivers');break
    except OSError:time.sleep(.1)
   assert json.loads(h.http(v['api']+'/api/settings/read-policy'))['settings']==v['readPolicy'],'Runtime read policy changed'
   sid='adjacent-'+uuid.uuid4().hex;endpoint=a.origin+'/'+sid
   try:
    h.http(v['api']+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
    wire=lambda:json.loads(h.http(a.origin_control+'/__stats/'+sid))['wire'];assert wire()==dict(gets=0,heads=0,bytes=0)
    jobs=[]
    for x in [68,69]:
     q=dict(image=endpoint+'/fixture/gb11-deep.jp2',driver='openjpeg',edge='crop',tileSize=512,minZoom=0,maxZoom=6,tilePixelRatio=1);url=v['api']+f'/streamer/v1/6/{x}/160/tile.png?'+urlencode(q);before=wire();t=time.perf_counter();body,headers=h.http(url,include_headers=True);elapsed=(time.perf_counter()-t)*1000;after=wire();im=Image.open(io.BytesIO(body)).convert('RGB');assert im.size==gold[x].size and im.tobytes()==gold[x].tobytes();jobs.append(dict(x=x,ms=elapsed,wire={k:after[k]-before[k] for k in before},headers={k:z for k,z in headers.items() if k.startswith('x-river-')},url=url,body=body))
    before=wire()
    for j in jobs:assert h.http(j['url'])==j['body'];del j['url'];del j['body']
    assert wire()==before;row=dict(version=label,trial=trial,tiles=jobs,pixelsExact=True,exactRepeatsSourceIoZero=True);rows.append(row);print(json.dumps(row),flush=True)
   finally:h.http(v['api']+'/api/s3/sources/'+sid,method='DELETE')
 report['summary']=[dict(x=x,**{v:statistics.median(j['ms'] for r in rows if r['version']==v for j in r['tiles'] if j['x']==x) for v in versions}) for x in [68,69]];report['passed']=True
finally:a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
