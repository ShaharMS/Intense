"""Short paired HTTP benchmark with a hash-pinned local Range origin. Requires Pillow.
Manifest: {primary: case-name, environment: description, cases: [{name,file,sha256,
reference,referenceSha256,z,x,y,tileSize,maxZoom,driver}]}. Paths relative to manifest.
Run only dedicated test containers; effective settings must match the manifest.
"""
import argparse, hashlib, io, json, platform, re, socket, statistics, subprocess, threading, time, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlencode, urlsplit
from urllib.request import Request, urlopen
from urllib.error import HTTPError
from http.client import HTTPConnection, HTTPSConnection
from PIL import Image

def sha(data): return hashlib.sha256(data).hexdigest()

def parse_effective_sizing(text):
    """Use only the latest sizing event; never combine restarts or event IDs."""
    keys={'decode_concurrency','per_viewport_decode_concurrency','openjpeg_threads_per_decode','max_native_tiles_per_job','preview_extra_reductions','refine_idle_delay_ms'}
    lines=re.sub(r'\x1b\[[0-9;]*m','',text).splitlines()
    markers=[i for i,line in enumerate(lines) if 'configured JPEG 2000 decode concurrency' in line]
    if not markers:return None
    latest=lines[markers[-1]]
    identity=re.search(r'\bt=(\d+)\s+\w+\s+e=(\d+)\b',latest)
    selected=[latest]
    if identity:
        selected=[line for line in lines if (match:=re.search(r'\bt=(\d+)\s+\w+\s+e=(\d+)\b',line)) and match.groups()==identity.groups()]
    values={}
    for line in selected:
        for key,value in re.findall(r'(?<!\w)([a-z_]+)="?([0-9]+)"?',line):
            if key in keys:
                if key in values and values[key]!=int(value):return None
                values[key]=int(value)
    return values if values.keys()==keys else None

def effective_sizing(container):
    """Read resolved Rust settings after the container entrypoint applies profiles."""
    info=json.loads(subprocess.check_output(['docker','inspect',container],text=True))[0]
    labels=info['Config'].get('Labels') or {}
    assert labels.get('com.docker.compose.project')=='river' and labels.get('com.docker.compose.service') in ['replica-1','replica-2','replica-3','replica-4'],'Use an existing River Compose replica slot'
    assert not any(e.startswith('RIVER_PER_VIEWPORT_DECODE_CONCURRENCY=') for e in info['Config']['Env']),'Unrecognized sizing variable: use RIVER_VIEWPORT_DECODE_CONCURRENCY and verify startup logs'
    started=subprocess.check_output(['docker','inspect','--format','{{.State.StartedAt}}',container],text=True).strip()
    for _ in range(30):
        log=subprocess.run(['docker','logs','--since',started,container],capture_output=True,text=True,check=True)
        values=parse_effective_sizing(log.stdout+'\n'+log.stderr)
        if values is not None:return values
        time.sleep(.1)
    raise AssertionError('Effective startup sizing was not found; do not infer it from container environment')
def http(url, data=None, method=None, include_headers=False):
    try:
        return _http(url,data,method,include_headers)
    except HTTPError as e:
        raise RuntimeError(f"HTTP {e.code}: {e.read().decode(errors='replace')}") from e
def _http(url,data,method,include_headers):
    with urlopen(Request(url, data=None if data is None else json.dumps(data).encode(),
                         headers={'Content-Type':'application/json'}, method=method), timeout=30) as r:
        body=r.read()
        return (body,dict(r.headers)) if include_headers else body

def runtime_settings(api):
    # Read only public tuning fields, never configuration/source credentials.
    policy=json.loads(http(api+'/api/settings/read-policy'))
    return dict(readPolicy=policy['settings'],explicitOverfetchCap=policy['explicitOverfetchCap'],effectiveAdaptiveOverfetchRatio=policy['effectiveAdaptiveOverfetchRatio'],webpMethod=json.loads(http(api+'/api/settings/encoding'))['webpMethod'])
def keepalive_repeat(url,expected):
    """Measure a cached response on a verified reused channel, not TCP setup.

    Cold and legacy fresh-connection repeat timers remain unchanged. The
    additional priming response is untimed; callers audit ALL source IO after
    both priming and measurement, so this cannot conceal extra range reads.
    """
    parsed=urlsplit(url);assert parsed.scheme in ('http','https')
    cls=HTTPSConnection if parsed.scheme=='https' else HTTPConnection
    connection=cls(parsed.hostname,parsed.port,timeout=30)
    target=parsed.path+('?' + parsed.query if parsed.query else '')
    try:
        connection.request('GET',target);response=connection.getresponse()
        assert response.status==200 and response.read()==expected,'Keepalive priming response differs'
        assert not response.will_close and connection.sock is not None,'Server did not retain the HTTP connection'
        local_port=connection.sock.getsockname()[1]
        started=time.perf_counter();connection.request('GET',target);response=connection.getresponse();headers_at=time.perf_counter()
        body=response.read();done=time.perf_counter()
        assert response.status==200 and body==expected,'Keepalive repeat response differs'
        assert not response.will_close and connection.sock is not None and connection.sock.getsockname()[1]==local_port,'Repeat reconnected instead of reusing TCP'
        return dict(totalMs=(done-started)*1000,headersMs=(headers_at-started)*1000,bodyMs=(done-headers_at)*1000,serverMs=float(response.getheader('x-river-server-ms')),connectionReused=True)
    finally:connection.close()

def make_origin(files,cases,manifest,counters,lock):
    signatures={k:(p.stat().st_size,p.stat().st_mtime_ns) for k,p in files.items()}
    class Origin(BaseHTTPRequestHandler):
        protocol_version="HTTP/1.1"
        wbufsize=262144
        def log_message(self,*args): pass
        def handle(self):
            try: super().handle()
            except ConnectionResetError:
                # A stopped River process closes pooled keepalive channels.
                # Decode failures still fail the client/status/pixel checks;
                # suppress only this expected transport teardown traceback.
                pass

        def do_HEAD(self): self.serve(False)
        def do_GET(self): self.serve(True)
        def serve(self,body):
            parts=urlsplit(self.path).path.strip('/').split('/')
            if len(parts)==3:parts[2]=parts[2].removesuffix('.jp2')
            if len(parts)!=3 or parts[2] not in files: self.send_error(404);return
            key=parts[0]; path=files[parts[2]]; stat=path.stat();size=stat.st_size
            if (size,stat.st_mtime_ns)!=signatures[parts[2]]:self.send_error(412,'Frozen source changed');return
            start,end=0,size-1; partial='Range' in self.headers
            if partial:
                try:
                    first,last=self.headers['Range'].removeprefix('bytes=').split('-')
                    start=int(first);end=min(int(last) if last else size-1,size-1)
                    assert 0<=start<=end<size
                except (ValueError,AssertionError): self.send_error(416);return
            with lock:
                c=counters.setdefault(key,dict(gets=0,heads=0,bytes=0));c['gets' if body else 'heads']+=1
            self.send_response(206 if partial else 200)
            self.send_header('Content-Length',str(end-start+1));self.send_header('Accept-Ranges','bytes')
            self.send_header('ETag','"'+manifest['cases'][next(i for i,c in enumerate(cases) if c['name']==parts[2])]['sha256']+'"')
            if partial:self.send_header('Content-Range',f'bytes {start}-{end}/{size}')
            self.end_headers()
            if body:
                with path.open('rb') as f:
                    f.seek(start);remaining=end-start+1
                    while remaining:
                        chunk=f.read(min(65536,remaining))
                        try:self.wfile.write(chunk)
                        except (BrokenPipeError,ConnectionResetError):break
                        with lock:counters[key]['bytes']+=len(chunk)
                        remaining-=len(chunk)
    return Origin

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--manifest',type=Path,required=True)
    p.add_argument('--baseline-api',required=True); p.add_argument('--candidate-api',required=True)
    p.add_argument('--baseline-container',required=True);p.add_argument('--candidate-container',required=True)
    p.add_argument('--port',type=int,default=18991);p.add_argument('--origin-host',default='host.docker.internal');p.add_argument('--external-origin');p.add_argument('--external-origin-control')
    p.add_argument('--source-startup-delay-ms',type=float,default=0);p.add_argument('--source-startup-delay-gets',type=int,default=0)
    p.add_argument('--origin-tcp-nodelay',action='store_true');p.add_argument('--origin-listen-backlog',type=int)
    p.add_argument('--comparison',choices=['stable','lts','control','config'],default='stable');p.add_argument('--rounds',type=int,default=4);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
    assert 0<=a.source_startup_delay_ms<=1000 and 0<=a.source_startup_delay_gets<=32
    assert bool(a.source_startup_delay_ms)==bool(a.source_startup_delay_gets),'Set both startup delay and count, or neither'
    assert a.origin_listen_backlog is None or 1<=a.origin_listen_backlog<=1024
    assert not a.external_origin or not (a.source_startup_delay_ms or a.origin_tcp_nodelay or a.origin_listen_backlog),'Local origin controls cannot change an external origin'
    assert bool(a.external_origin)==bool(a.external_origin_control), 'External origin requires its control URL'
    if a.output.exists():p.error('Report already exists; use a new output path to preserve benchmark evidence')
    assert a.external_origin,'Fixture origins must reuse a Compose replica slot; supply --external-origin and --external-origin-control' 
    assert a.baseline_container != a.candidate_container, 'Use separate instances'
    assert a.rounds>=4 and a.rounds%2==0, 'Use at least four balanced paired rounds'
    versions={v:dict(api=getattr(a,v+'_api').rstrip('/'),container=getattr(a,v+'_container')) for v in ['baseline','candidate']}
    for v in versions.values():
        v['inspect']=json.loads(subprocess.check_output(['docker','inspect',v['container']],text=True))[0]
        labels=v['inspect']['Config'].get('Labels') or {}
        assert labels.get('com.docker.compose.project')=='river' and labels.get('com.docker.compose.service') in ['replica-1','replica-2','replica-3','replica-4'],'Mutating benchmarks must use existing River Compose replica slots'
    baseline=versions['baseline']['inspect'];candidate=versions['candidate']['inspect']
    same_environment=sorted(baseline['Config']['Env'])==sorted(candidate['Config']['Env'])
    limit_keys=['Memory','NanoCpus','CpuQuota','CpuPeriod','CpusetCpus']
    same_resources=all(baseline['HostConfig'].get(k)==candidate['HostConfig'].get(k) for k in limit_keys)
    for v in versions.values():v['runtimeSettings']=runtime_settings(v['api'])
    same_runtime=versions['baseline']['runtimeSettings']==versions['candidate']['runtimeSettings']
    if a.comparison!='config':
        assert same_environment,'Server environments differ; clone the baseline exactly or explicitly use --comparison config'
        assert same_resources,'Resource limits differ; a code comparison requires identical limits'
        assert same_runtime,'Runtime settings differ; align persisted API settings or use an explicit settings comparison'
    if a.comparison=='control':
        assert baseline['Image']==candidate['Image'],'A/A control requires identical image IDs'
    else:
        channel='stable' if a.comparison=='config' else a.comparison
        expected=subprocess.check_output(['docker','image','inspect','river:'+channel,'--format','{{.Id}}'],text=True).strip()
        assert baseline['Image']==expected,'Baseline must be the CURRENT release alias, not a historical checkpoint'
        if a.comparison=='config':assert candidate['Image']==expected,'Settings comparisons require the same current stable image on both sides'
    raw=a.manifest.read_bytes(); manifest=json.loads(raw); cases=manifest['cases']
    assert manifest['primary'] in [c['name'] for c in cases]
    assert manifest['environment'], 'Record hardware, network and effective configuration'
    if manifest.get('schemaVersion',1)>=2:
        bounds={'50MB':(40_000_000,60_000_000),'900-1000MB':(900_000_000,1_000_000_000),'10-15GB':(10_000_000_000,15_000_000_000)}
        assert set(manifest['requiredSizeClasses'])==set(bounds), 'All three requested size classes are mandatory'
        for group,(lo,hi) in bounds.items():
            members=[c for c in cases if c.get('sizeClass')==group]
            assert {c['operation'] for c in members}=={'preview','deep-tile'}, 'Each size needs preview and deep tile'
            assert len({c['sha256'] for c in members})==1 and all(lo<=c['sourceBytes']<=hi for c in members), 'Wrong size or different source for preview/tile'
    files={}; refs={}; counters={}; lock=threading.Lock();verified={};source_signatures={}
    for c in cases:
        path=(a.manifest.parent/c['file']).resolve()
        signature=lambda path:(path.stat().st_size,path.stat().st_mtime_ns,path.stat().st_ctime_ns)
        if path not in verified:
            source_signatures[path]=signature(path)
            with path.open('rb') as f: verified[path]=hashlib.file_digest(f,'sha256').hexdigest()
        assert signature(path)==source_signatures[path], 'Source changed during validation'
        assert verified[path]==c['sha256'],f"Source changed: {path}"
        ref=(a.manifest.parent/c['reference']).read_bytes()
        assert sha(ref)==c['referenceSha256'], 'Reference changed'
        refs[c['name']]=Image.open(io.BytesIO(ref)).convert('RGB')
        assert any(lo<hi for lo,hi in refs[c['name']].getextrema()), 'Blank benchmark reference'
        files[c['name']]=path
    server=None;origin_description=None
    if a.external_origin:
        origin_description=json.loads(http(a.external_origin_control.rstrip('/')+'/__manifest'))
        probe="import json,sys,urllib.request;print(json.dumps(json.load(urllib.request.urlopen(sys.argv[1]+'/__manifest',timeout=3))))"
        reachable=json.loads(subprocess.check_output(['docker','exec',a.baseline_container,'python3','-c',probe,a.external_origin.rstrip('/')],text=True))
        assert reachable['sha256']==origin_description['sha256'] and reachable.get('responseDelayMs')==origin_description.get('responseDelayMs'),'Fixture network endpoint and control URL reach different origin instances' 
        assert origin_description['sha256']==sha(raw),'External origin must serve this exact frozen manifest'
        assert origin_description['files']=={c['name']:c['sourceBytes'] for c in cases},'External source size mismatch'
    else:
        BaseOrigin=make_origin(files,cases,manifest,counters,lock);arrivals={}
        class Origin(BaseOrigin):
            def setup(self):
                super().setup()
                if a.origin_tcp_nodelay:self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
            def serve(self,body):
                if body and a.source_startup_delay_gets:
                    sid=urlsplit(self.path).path.strip('/').split('/')[0]
                    with lock:n=arrivals.get(sid,0);arrivals[sid]=n+1
                    if n<a.source_startup_delay_gets:time.sleep(a.source_startup_delay_ms/1000)
                super().serve(body)
        class LocalOriginServer(ThreadingHTTPServer):
            request_queue_size=a.origin_listen_backlog or ThreadingHTTPServer.request_queue_size
        server=LocalOriginServer(('0.0.0.0',a.port),Origin)
        threading.Thread(target=server.serve_forever,daemon=True).start()
    def source_wire(sid):
        if a.external_origin:
            return json.loads(http(a.external_origin_control.rstrip('/')+'/__stats/'+sid))['wire']
        return dict(counters[sid])
    for v in versions.values():v['effectiveSizing']=effective_sizing(v['container'])
    if a.comparison=='config':
        envs=[dict(item.split('=',1) for item in versions[v]['inspect']['Config']['Env']) for v in ['baseline','candidate']]
        changed={k for k in envs[0].keys()|envs[1].keys() if envs[0].get(k)!=envs[1].get(k)}
        sizing={'RIVER_DECODE_CONCURRENCY':'decode_concurrency','RIVER_OPENJPEG_THREADS_PER_DECODE':'openjpeg_threads_per_decode','RIVER_VIEWPORT_DECODE_CONCURRENCY':'per_viewport_decode_concurrency','RIVER_MAX_NATIVE_TILES_PER_JOB':'max_native_tiles_per_job'}
        if changed and changed<=sizing.keys():
            assert any(versions['baseline']['effectiveSizing'][sizing[k]]!=versions['candidate']['effectiveSizing'][sizing[k]] for k in changed),'No-op sizing experiment after container profile resolution'
    runtime=json.loads(subprocess.check_output(['docker','info','--format','{"engine":"{{.ServerVersion}}","cpus":{{.NCPU}},"memoryBytes":{{.MemTotal}},"kernel":"{{.KernelVersion}}"}'],text=True))
    rows=[]; report=dict(passed=False,runtime=runtime,runnerSha256=sha(Path(__file__).read_bytes()),comparison=a.comparison,sameEnvironment=same_environment,sameResourceLimits=same_resources,manifestSha256=sha(raw),environment=manifest['environment'],host=platform.platform(),
        cacheState=('River process cold; origin OS cache not flushed; external HTTP Range control' if a.external_origin else 'River process cold; origin OS cache not explicitly flushed or warmed; local HTTP Range origin'),originControl=origin_description,originEndpoint=a.external_origin,versions=versions,rows=rows)
    report['measurementProtocolVersion']=4
    report['originProfile']=({'type':'external','details':'see originControl'} if a.external_origin else {'type':'local','startupDelayMs':a.source_startup_delay_ms,'startupDelayGets':a.source_startup_delay_gets,'tcpNoDelay':a.origin_tcp_nodelay,'listenBacklog':LocalOriginServer.request_queue_size})
    report['sameRuntimeSettings']=same_runtime
    report['connectionPolicy']={'coldMs':'fresh HTTP connection, unchanged legacy timer','repeatMs':'fresh HTTP connection, unchanged legacy timer','repeatKeepAlive':'one untimed cached response primes a new connection, then one timed response reuses that exact TCP channel; source IO audited across all phases'}
    a.output.parent.mkdir(parents=True,exist_ok=True);started=time.monotonic()
    try:
        for trial in range(a.rounds):
            for case in cases:
                for version in (['baseline','candidate'] if trial%2==0 else ['candidate','baseline']):
                    assert time.monotonic()-started<180,'Screen exceeded 180s; incomplete'
                    report['activeSample']=dict(case=case['name'],version=version,trial=trial)
                    report['activePhase']='restart'
                    v=versions[version];subprocess.run(['docker','restart',v['container']],check=True,stdout=subprocess.DEVNULL,timeout=20)
                    report['activePhase']='readiness'
                    for attempt in range(100):
                        try:http(v['api']+'/api/drivers');break
                        except OSError:time.sleep(.1)
                    else:raise RuntimeError('API readiness timeout')
                    report['activePhase']='runtimeSettings'
                    assert runtime_settings(v['api'])==v['runtimeSettings'],'Runtime settings changed across restart or during the benchmark'
                    sid='fixed-'+uuid.uuid4().hex; endpoint=(a.external_origin.rstrip('/') if a.external_origin else f'http://{a.origin_host}:{a.port}')+'/'+sid
                    counters[sid]=dict(gets=0,heads=0,bytes=0)
                    report['activeSample']['sourceId']=sid
                    try:
                        report['activePhase']='sourceRegistration'
                        http(v['api']+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
                        assert source_wire(sid)==dict(gets=0,heads=0,bytes=0),'Registration hid source IO'
                        query=dict(image=endpoint+'/fixture/'+case['name']+'.jp2',driver=case['driver'],edge='crop',tileSize=case['tileSize'],minZoom=0,maxZoom=case['maxZoom'],tilePixelRatio=1)
                        if 'layers' in case:query['layers']=case['layers']
                        url=v['api']+f"/streamer/v1/{case['z']}/{case['x']}/{case['y']}/tile.png?"+urlencode(query)
                        report['activePhase']='coldResponse'
                        t=time.perf_counter();body,headers=http(url,include_headers=True);cold=(time.perf_counter()-t)*1000
                        report['activePhase']='coldOriginCounters';wire=source_wire(sid)
                        report['activePhase']='repeatResponse';t=time.perf_counter();repeat,repeat_headers=http(url,include_headers=True);warm=(time.perf_counter()-t)*1000
                        report['activePhase']='keepaliveRepeat'
                        keepalive=keepalive_repeat(url,body)
                        report['activePhase']='repeatOriginCounters'
                        after_repeat=source_wire(sid)
                        assert after_repeat==wire,'Exact repeat performed source IO'
                        actual=Image.open(io.BytesIO(body)).convert('RGB');reference=refs[case['name']]
                        if actual.size!=reference.size or actual.tobytes()!=reference.tobytes():
                            actual.save(a.output.with_suffix('.failed-pixels.png'))
                            raise AssertionError(f"Incorrect pixels: case={case['name']} actual={actual.size} reference={reference.size}")
                        assert repeat==body,'Repeat differs'
                        row=dict(case=case['name'],trial=trial,version=version,coldMs=cold,repeatMs=warm,wire=wire,repeatWire={k:after_repeat[k]-wire[k] for k in wire},pixelsExact=True,repeatResponseTimingHeaders={k:v for k,v in repeat_headers.items() if k.lower()=='x-river-server-ms'},responseTimingHeaders={k:v for k,v in headers.items() if k.lower().startswith('x-river-') or k.lower()=='server-timing'})
                        row['repeatKeepAlive']=keepalive
                        rows.append(row);print(json.dumps(row),flush=True)
                    except Exception:
                        report['sampleFailurePhase']=report['activePhase'];raise
                    finally:
                        report['activePhase']='sourceCleanup';http(v['api']+'/api/s3/sources/'+sid,method='DELETE')
        report['summary']=[]
        for case in cases:
            medians={v:statistics.median(r['coldMs'] for r in rows if r['case']==case['name'] and r['version']==v) for v in versions}
            repeats={v:statistics.median(r['repeatMs'] for r in rows if r['case']==case['name'] and r['version']==v) for v in versions}
            persistent={v:statistics.median(r['repeatKeepAlive']['totalMs'] for r in rows if r['case']==case['name'] and r['version']==v) for v in versions}
            report['summary'].append(dict(case=case['name'],repeatMedianMs=repeats,repeatKeepAliveMedianMs=persistent,**medians,latencyReductionPercent=100*(1-medians['candidate']/medians['baseline'])))
        assert all(signature(path)==stamp for path,stamp in source_signatures.items()), 'A frozen source changed during the run'
        report['activeSample']=None
        report['activePhase']=None
        if a.external_origin and origin_description.get('networkCounters') is not None:
            report['originNetworkEnd']=json.loads(http(a.external_origin_control.rstrip('/')+'/__netstat'))
            report['originNetworkDelta']={k:report['originNetworkEnd'][k]-origin_description['networkCounters'][k] for k in report['originNetworkEnd']}
            assert report['originNetworkDelta']['ListenDrops']==0 and report['originNetworkDelta']['ListenOverflows']==0,'Fixture origin dropped connections; reject timings'
        report['passed']=True
        report['sameImageControl']=versions['baseline']['inspect']['Image']==versions['candidate']['inspect']['Image']
        report['sameConfigurationControl']=report['sameImageControl'] and same_environment and same_resources and same_runtime
        report['promotionEligible']=False
        report['promotionNote']='Screen only; same-build differences are noise. Changed images require reproducibility and correctness qualification under benchmark-policy.md.'
    except Exception as error:
        report['failure']=str(error)
        report['failedSample']=report.get('activeSample')
        report['failedPhase']=report.get('sampleFailurePhase',report.get('activePhase'))
        raise
    finally:
        report['elapsedSeconds']=time.monotonic()-started
        # Never serialize container environment: it may contain credentials.
        report['versions']={k:dict(api=v['api'],container=v['container'],imageId=v['inspect']['Image'],effectiveSizing=v.get('effectiveSizing'),runtimeSettings=v['runtimeSettings'], environmentSha256=sha(json.dumps(sorted(v['inspect']['Config']['Env'])).encode()), cpuQuota=v['inspect']['HostConfig']['CpuQuota'], nanoCpus=v['inspect']['HostConfig']['NanoCpus'], memoryLimit=v['inspect']['HostConfig']['Memory']) for k,v in versions.items()}
        a.output.write_text(json.dumps(report,indent=2)+'\n')
        if server:server.shutdown()
if __name__=='__main__':main()
