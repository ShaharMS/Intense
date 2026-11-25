"""Paired local-file control; failed cold requests never enter latency medians."""
import argparse, hashlib, importlib.util, io, json, statistics, subprocess, time
from pathlib import Path
from urllib.parse import urlencode
from PIL import Image

p=argparse.ArgumentParser()
p.add_argument('--manifest',type=Path,required=True)
p.add_argument('--output',type=Path,required=True)
p.add_argument('--resume',action='store_true')
p.add_argument('--rounds',type=int,choices=[1,4],default=4,help='One round is a functional screen, not a balanced performance comparison')
for label in ('baseline','candidate'):
    p.add_argument('--'+label+'-container',required=True)
    p.add_argument('--'+label+'-api',required=True)
a=p.parse_args()
assert a.output.exists()==a.resume
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'))
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
def inspect(name):
    return json.loads(subprocess.check_output(['docker','inspect',name]))[0]
configs={v:inspect(getattr(a,v+'_container')) for v in ('baseline','candidate')}
assert all(getattr(a,v+'_container').startswith('river-goal-local-') for v in configs)
assert configs['baseline']['Image']==subprocess.check_output(['docker','image','inspect','river:stable','--format','{{.Id}}'],text=True).strip()
assert sorted(configs['baseline']['Config']['Env'])==sorted(configs['candidate']['Config']['Env'])
assert configs['baseline']['Mounts']==configs['candidate']['Mounts']
assert all(not m['RW'] for m in configs['baseline']['Mounts'])
limits=('Memory','MemorySwap','NanoCpus','CpuQuota','CpuPeriod','CpusetCpus')
assert all(configs['baseline']['HostConfig'][k]==configs['candidate']['HostConfig'][k] for k in limits)
raw=a.manifest.read_bytes();manifest=json.loads(raw)
assert {c['sizeClass'] for c in manifest['cases']}=={'50MB','900-1000MB','10-15GB'}
checked=set()
for c in manifest['cases']:
    path=(a.manifest.parent/c['file']).resolve()
    if path not in checked:
        with path.open('rb') as f: assert hashlib.file_digest(f,'sha256').hexdigest()==c['sha256']
        checked.add(path)
rows=[];failed=set()
report=dict(passed=False,manifestSha256=h.sha(raw),runnerSha256=h.sha(Path(__file__).read_bytes()),
    imageIds={v:c['Image'] for v,c in configs.items()},resourceLimits={k:configs['baseline']['HostConfig'][k] for k in limits},
    rounds=a.rounds,screenOnly=a.rounds==1,scope=f'{a.rounds} fresh-container rounds with empty writable layers, unchanged read-only fixtures, OS caches not flushed. Four rounds balance timing order; one round is functional only. First catalog resolution included. Cache-hit cold responses rejected. Repeat byte equality and server telemetry checked; telemetry is not an independent OS file-IO audit. Failed cases are retained once and excluded from timing summaries.',rows=rows)
def save(): a.output.write_text(json.dumps(report,indent=2)+'\n')
if a.resume:
    prior=json.loads(a.output.read_text())
    assert not prior.get('completed') and prior['manifestSha256']==report['manifestSha256'] and prior['imageIds']==report['imageIds']
    assert prior['resourceLimits']==report['resourceLimits']
    assert prior.get('rounds',4)==a.rounds,'Cannot change the number of rounds during resume'
    rows.extend(prior['rows'])
    failed.update((r['version'],r['case']) for r in rows if 'failure' in r)
    report['resumedFromRunnerSha256']=prior['runnerSha256']
slot='river-goal-local-paired-slot'
assert subprocess.run(['docker','inspect',slot],capture_output=True).returncode!=0,'Inspect existing slot before reuse'
for trial in range(a.rounds):
    for c in manifest['cases']:
        for label in (('baseline','candidate') if trial%2==0 else ('candidate','baseline')):
            if (label,c['name']) in failed: continue
            if any(r['version']==label and r['case']==c['name'] and r['trial']==trial for r in rows):continue
            port=18889
            reference_container=getattr(a,label+'_container');name=slot;api=f'http://127.0.0.1:{port}'
            cfg=configs[label]
            command=['docker','run','-d','--name',slot,'--label','river.benchmark.placement=local-fixed-v1','--volumes-from',reference_container+':ro','-p',f'127.0.0.1:{port}:8080','--memory',str(cfg['HostConfig']['Memory']),'--memory-swap',str(cfg['HostConfig']['MemorySwap'])]
            assert not any(cfg['HostConfig'][k] for k in ('NanoCpus','CpuQuota','CpusetCpus'))
            for env in cfg['Config']['Env']:command+=['-e',env]
            subprocess.run(command+[cfg['Image']],check=True,stdout=subprocess.DEVNULL)
            for _ in range(150):
                try: h.http(api+'/api/drivers');break
                except OSError: time.sleep(.1)
            else: raise RuntimeError('Readiness timeout')
            q=dict(image=c['sha256'],driver=c['driver'],edge='crop',tileSize=c['tileSize'],minZoom=0,maxZoom=c['maxZoom'],tilePixelRatio=1)
            url=api+f"/streamer/v1/{c['z']}/{c['x']}/{c['y']}/tile.png?"+urlencode(q)
            row=dict(case=c['name'],version=label,trial=trial);rows.append(row)
            try:
                t=time.perf_counter();body,headers=h.http(url,include_headers=True);elapsed=(time.perf_counter()-t)*1000
                assert headers.get('x-river-source-cache') not in ('encoded-raster-hit','s3-range-hit','file-range-hit'),'Cold response reused a disk cache'
                assert headers.get('x-river-decoder-route')!='encoded-raster-cache','Cold response reused encoded pixels'
                reference=(a.manifest.parent/c['reference']).read_bytes();assert h.sha(reference)==c['referenceSha256']
                ref=Image.open(io.BytesIO(reference)).convert('RGB');actual=Image.open(io.BytesIO(body)).convert('RGB')
                assert actual.size==ref.size and actual.tobytes()==ref.tobytes(),'Pixel mismatch'
                t=time.perf_counter();repeat,rheaders=h.http(url,include_headers=True);warm=(time.perf_counter()-t)*1000
                assert repeat==body,'Repeat body mismatch'
                for k in ('x-river-source-read-bytes','x-river-source-range-requests'):
                    assert int(rheaders.get(k,'0'))==0,'Repeat reports source reads'
                row.update(coldMs=elapsed,repeatMs=warm,pixelsExact=True,repeatExact=True,
                    responseTimingHeaders={k:v for k,v in headers.items() if k.lower().startswith('x-river-')},
                    repeatResponseTimingHeaders={k:v for k,v in rheaders.items() if k.lower().startswith('x-river-')},
                    peakMemoryBytes=int(subprocess.check_output(['docker','exec',name,'cat','/sys/fs/cgroup/memory.peak'])))
            except Exception as error:
                row.update(failure=str(error),containerState=inspect(name)['State']);failed.add((label,c['name']))
            print(json.dumps(row),flush=True);save()
            subprocess.run(['docker','stop',slot],check=True,stdout=subprocess.DEVNULL)
            subprocess.run(['docker','rm',slot],check=True,stdout=subprocess.DEVNULL)
report['summary']=[dict(case=c['name'],version=v,samples=len(rs),coldMedianMs=statistics.median(r['coldMs'] for r in rs),repeatMedianMs=statistics.median(r['repeatMs'] for r in rs)) for c in manifest['cases'] for v in configs if (rs:=[r for r in rows if r['case']==c['name'] and r['version']==v and 'coldMs' in r])]
report['passed']=not failed;report['completed']=True;save()
