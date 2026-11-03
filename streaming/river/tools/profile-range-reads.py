"""Reconcile opt-in River range traces against an independent fixture origin.
Diagnostic only: one cold sample per case, never a release speedup claim.
"""
import argparse,collections,datetime,hashlib,importlib.util,io,json,subprocess,time,uuid
from pathlib import Path
from urllib.parse import urlencode
from PIL import Image
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--api',required=True);p.add_argument('--container',required=True);p.add_argument('--manifest',type=Path,required=True);p.add_argument('--origin',required=True);p.add_argument('--origin-control',required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
assert a.container.startswith('river-goal-') and not a.output.exists()
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
raw=a.manifest.read_bytes();manifest=json.loads(raw);assert {c['sizeClass'] for c in manifest['cases']}=={'50MB','900-1000MB','10-15GB'}
origin=json.loads(h.http(a.origin_control+'/__manifest'));assert origin['sha256']==h.sha(raw)
rows=[];report=dict(passed=False,scope='One cold request per case; source process/OS cache may be warm. Independent wire reconciliation, exact pixels and zero-source-IO repeats. Not a speedup benchmark.',manifestSha256=h.sha(raw),imageId=json.loads(subprocess.check_output(['docker','inspect',a.container]))[0]['Image'],rows=rows)
# Resolve recorded callsites to enclosing Rust functions for a readable explanation.
def caller_function(caller):
    path,line=caller.rsplit(':',1);path=Path(path.replace('\\','/'));local=Path(__file__).resolve().parents[1]/path
    if not local.exists():return None
    import re
    names=[m[1] for text in local.read_text(encoding='utf-8').splitlines()[:int(line)] if (m:=re.search(r'\bfn\s+(\w+)',text))]
    return names[-1] if names else None
try:
 for c in manifest['cases']:
    subprocess.run(['docker','restart',a.container],check=True,stdout=subprocess.DEVNULL)
    for _ in range(100):
        try:h.http(a.api+'/api/drivers');break
        except OSError:time.sleep(.1)
    else:raise RuntimeError('Readiness timeout')
    h.http(a.api+'/api/diagnostics/source-reads',dict(enabled=True),'PUT');h.http(a.api+'/api/diagnostics/source-reads',method='DELETE')
    sid='trace-'+uuid.uuid4().hex;endpoint=a.origin+'/'+sid
    try:
        h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
        wire=lambda:json.loads(h.http(a.origin_control+'/__stats/'+sid))
        assert wire()['wire']==dict(gets=0,heads=0,bytes=0)
        query=dict(image=endpoint+'/fixture/'+c['name']+'.jp2',driver=c['driver'],edge='crop',tileSize=c['tileSize'],minZoom=0,maxZoom=c['maxZoom'],tilePixelRatio=1)
        url=a.api+f"/streamer/v1/{c['z']}/{c['x']}/{c['y']}/tile.png?"+urlencode(query)
        start=time.perf_counter();body=h.http(url);elapsed=(time.perf_counter()-start)*1000
        before=wire();trace=json.loads(h.http(a.api+'/api/diagnostics/source-reads'));assert trace['evicted']==0
        events=trace['events'];reads=[e for e in events if e['kind']=='rangeRead'];assert all(e['outcome']=='ok' for e in reads)
        attempts=[e for e in events if e['kind']=='wireAttempt'];assert all(e['httpStatus']==206 for e in attempts)
        native=[e for e in reads if e['purpose']=='nativeAttempt' and e['transport']=='curl']
        assert len(attempts)+sum(e['physicalAttempts'] for e in native)==before['wire']['gets']
        assert len(reads)==before['wire']['gets'],(c['name'],len(reads),before['wire'])
        assert sum(e['bytes'] for e in reads)==before['wire']['bytes']
        summary=trace['summary'];assert not summary['partial'];assert summary['physicalAttempts']==before['wire']['gets'];assert summary['logicalReads']==len(reads);assert summary['recordedBodyBytes']==before['wire']['bytes'];assert sum(g['physicalAttempts'] for g in summary['groups'])==before['wire']['gets']
        # Independent byte-range multiset comparison catches omitted and duplicate operations.
        observed=collections.Counter((e['start'],e['endExclusive']) for e in reads)
        ranges=before['ranges']
        # Native fixture origin returns a list of inclusive Range strings or structured records.
        actual=collections.Counter()
        for r in ranges:
            if isinstance(r,dict):
                lo,hi=r['range'].removeprefix('bytes=').split('-');actual[(int(lo),int(hi)+1)]+=1
            else:
                lo,hi=r.removeprefix('bytes=').split('-');actual[(int(lo),int(hi)+1)]+=1
        assert observed==actual,(observed-actual,actual-observed)
        groups=collections.defaultdict(lambda:dict(reads=0,bytes=0,elapsedMs=0.0))
        for e in events:e['callerFunction']=caller_function(e['caller'])
        for e in reads:
            g=groups[e['purpose']+'/'+str(e['callerFunction'])];g['reads']+=1;g['bytes']+=e['bytes'];g['elapsedMs']+=e['elapsedMs']
        reference=(a.manifest.parent/c['reference']).read_bytes();assert h.sha(reference)==c['referenceSha256'];actual_pixels=Image.open(io.BytesIO(body)).convert('RGB');ref=Image.open(io.BytesIO(reference)).convert('RGB');assert actual_pixels.size==ref.size and actual_pixels.tobytes()==ref.tobytes()
        assert h.http(url)==body and wire()==before
        after=json.loads(h.http(a.api+'/api/diagnostics/source-reads'));assert len(after['events'])==len(events)
        policy=json.loads(h.http(a.api+'/api/settings/read-policy'))
        row=dict(case=c['name'],coldMs=elapsed,wire=before['wire'],groups=groups,serverSummary=summary,events=events,originRanges=ranges,pixelsExact=True,repeatSourceIoZero=True,traceMatchesWire=True,serialTransport=policy['settings']['serialTransport'],sdkWireAttempts=len(attempts),nativeWireAttempts=sum(e['physicalAttempts'] for e in native),sourceObservation=policy['sources'].get(sid));rows.append(row);print(json.dumps({k:v for k,v in row.items() if k not in ('events','originRanges')}),flush=True)
    finally:h.http(a.api+'/api/s3/sources/'+sid,method='DELETE')
 report['passed']=True
finally:
    try:
        h.http(a.api+'/api/diagnostics/source-reads',dict(enabled=False),'PUT')
        report['traceCleanupSucceeded']=True
    except Exception as error:
        report['traceCleanupSucceeded']=False;report['traceCleanupError']=str(error)
    finally:
        a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
