"""Isolated source-stage CPU diagnostic; profiling timings are not release comparisons."""
import argparse,datetime,hashlib,importlib.util,io,json,re,subprocess,time,uuid
from pathlib import Path
from urllib.parse import urlencode
from PIL import Image

p=argparse.ArgumentParser();p.add_argument('--manifest',type=Path,required=True)
p.add_argument('--api',required=True);p.add_argument('--container',required=True)
p.add_argument('--origin',required=True);p.add_argument('--origin-control',required=True)
p.add_argument('--output',type=Path,required=True);a=p.parse_args();assert not a.output.exists()
assert a.container.startswith('river-goal-')
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'))
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
cfg=json.loads(subprocess.check_output(['docker','inspect',a.container]))[0]
assert 'RIVER_PROFILE_SOURCE=1' in cfg['Config']['Env']
raw=a.manifest.read_bytes();manifest=json.loads(raw)
assert {c['sizeClass'] for c in manifest['cases']}=={'50MB','900-1000MB','10-15GB'}
origin=json.loads(h.http(a.origin_control+'/__manifest'));assert origin['sha256']==h.sha(raw)
checked=set()
for c in manifest['cases']:
    source=(a.manifest.parent/c['file']).resolve()
    if source not in checked:
        with source.open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==c['sha256']
        checked.add(source)
rows=[];report=dict(passed=False,imageId=cfg['Image'],manifestSha256=h.sha(raw),runnerSha256=h.sha(Path(__file__).read_bytes()),origin=origin,
    scope='Opt-in isolated source preparation stage wall/CPU counters on all six frozen cases. Caller CPU is thread-local; cgroup CPU includes all container threads and nginx. Counters cover instrumented preparation phases, not complete request CPU. OS caches not flushed. No release speedup inference.',rows=rows)
try:
    for c in manifest['cases']:
        subprocess.run(['docker','restart',a.container],check=True,stdout=subprocess.DEVNULL)
        for _ in range(100):
            try:h.http(a.api+'/api/drivers');break
            except OSError:time.sleep(.1)
        else:raise RuntimeError('Readiness timeout')
        sid='cpu-'+uuid.uuid4().hex;endpoint=a.origin+'/'+sid
        try:
            h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
            wire=lambda:json.loads(h.http(a.origin_control+'/__stats/'+sid))['wire']
            assert wire()==dict(gets=0,heads=0,bytes=0)
            query=dict(image=endpoint+'/fixture/'+c['name']+'.jp2',driver=c['driver'],edge='crop',tileSize=c['tileSize'],minZoom=0,maxZoom=c['maxZoom'],tilePixelRatio=1)
            url=a.api+f"/streamer/v1/{c['z']}/{c['x']}/{c['y']}/tile.png?"+urlencode(query)
            since=datetime.datetime.now(datetime.timezone.utc).isoformat()
            t=time.perf_counter();body,headers=h.http(url,include_headers=True);elapsed=(time.perf_counter()-t)*1000
            assert headers.get('x-river-decoder-route')!='encoded-raster-cache'
            log=subprocess.run(['docker','logs','--since',since,a.container],capture_output=True,text=True,check=True)
            stages=[]
            for line in (log.stdout+'\n'+log.stderr).splitlines():
                line=re.sub(r'\x1b\[[0-9;]*m','',line)
                if 'S3 preparation stage;' not in line:continue
                name=re.search(r'\bstage="?([^" ]+)',line);assert name,line
                fields={k:float(v) for k,v in re.findall(r'(?<!\w)([a-z_]+)=(-?[0-9]+(?:\.[0-9]+)?)',line)}
                stages.append(dict(stage=name[1],**fields))
            reference=(a.manifest.parent/c['reference']).read_bytes();assert h.sha(reference)==c['referenceSha256']
            ref=Image.open(io.BytesIO(reference)).convert('RGB');actual=Image.open(io.BytesIO(body)).convert('RGB')
            assert actual.size==ref.size and actual.tobytes()==ref.tobytes()
            before=wire();assert h.http(url)==body;assert wire()==before
            trace=json.loads(h.http(a.origin_control+'/__stats/'+sid))
            row=dict(case=c['name'],sourceId=sid,originRanges=trace['ranges'],coldMs=elapsed,wire=before,stages=stages,pixelsExact=True,repeatExact=True,repeatSourceIoZero=True,responseTimingHeaders={k:v for k,v in headers.items() if k.lower().startswith('x-river-')})
            assert any(s['stage']=='total' and s['caller_cpu_ms']>=0 and s['cgroup_cpu_ms']>=0 for s in stages),'Missing total preparation CPU counters'
            if c['name']=='mb979-preview':assert any(s['stage']=='index' and s['caller_cpu_ms']>=0 and s['cgroup_cpu_ms']>=0 for s in stages)
            rows.append(row);print(json.dumps(row),flush=True)
        finally:h.http(a.api+'/api/s3/sources/'+sid,method='DELETE')
    report['passed']=True
finally:a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
