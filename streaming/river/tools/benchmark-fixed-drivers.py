"""Driver screen on unchanged frozen cases; same image/settings, rotating driver order."""
import argparse,hashlib,importlib.util,io,json,statistics,subprocess,time,uuid
from pathlib import Path
from urllib.parse import urlencode
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--manifest',type=Path,required=True);p.add_argument('--api',required=True)
p.add_argument('--container',required=True);p.add_argument('--origin',required=True);p.add_argument('--origin-control',required=True)
p.add_argument('--output',type=Path,required=True);a=p.parse_args();assert not a.output.exists();assert a.container.startswith('river-goal-')
spec=importlib.util.spec_from_file_location('h',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
cfg=json.loads(subprocess.check_output(['docker','inspect',a.container]))[0]
assert cfg['Image']==subprocess.check_output(['docker','image','inspect','river:stable','--format','{{.Id}}'],text=True).strip()
raw=a.manifest.read_bytes();m=json.loads(raw);assert {c['sizeClass'] for c in m['cases']}=={'50MB','900-1000MB','10-15GB'}
origin=json.loads(h.http(a.origin_control+'/__manifest'));assert origin['sha256']==h.sha(raw)
checked=set();refs={}
for c in m['cases']:
 f=(a.manifest.parent/c['file']).resolve()
 if f not in checked:
  with f.open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==c['sha256']
  checked.add(f)
 data=(a.manifest.parent/c['reference']).read_bytes();assert h.sha(data)==c['referenceSha256'];refs[c['name']]=Image.open(io.BytesIO(data)).convert('RGB')
drivers=['openjpeg','jp2grok','openhtj2k'];rows=[]
report=dict(passed=False,screenOnly=True,imageId=cfg['Image'],environmentSha256=h.sha(json.dumps(sorted(cfg['Config']['Env'])).encode()),manifestSha256=h.sha(raw),runnerSha256=h.sha(Path(__file__).read_bytes()),origin=origin,rows=rows,scope='Three rotations balance driver position. Fresh River process and unique source identity per cold request; complete body timed; OS caches not flushed. Same frozen files, geometry, settings and native origin. Driver is deliberately varied; this is not a code-versus-code release comparison.')
try:
 for trial in range(3):
  for c in m['cases']:
   for driver in drivers[trial:]+drivers[:trial]:
    subprocess.run(['docker','restart',a.container],check=True,stdout=subprocess.DEVNULL)
    for _ in range(100):
     try:h.http(a.api+'/api/drivers');break
     except OSError:time.sleep(.1)
    else:raise RuntimeError('Readiness timeout')
    sid='driver-'+uuid.uuid4().hex;endpoint=a.origin+'/'+sid
    try:
     h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
     wire=lambda:json.loads(h.http(a.origin_control+'/__stats/'+sid))['wire']
     assert wire()==dict(gets=0,heads=0,bytes=0)
     q=dict(image=endpoint+'/fixture/'+c['name']+'.jp2',driver=driver,edge='crop',tileSize=c['tileSize'],minZoom=0,maxZoom=c['maxZoom'],tilePixelRatio=1)
     url=a.api+f"/streamer/v1/{c['z']}/{c['x']}/{c['y']}/tile.png?"+urlencode(q)
     t=time.perf_counter();body,headers=h.http(url,include_headers=True);cold=(time.perf_counter()-t)*1000
     assert headers.get('x-river-decoder-route')!='encoded-raster-cache'
     before=wire();t=time.perf_counter();repeat=h.http(url);warm=(time.perf_counter()-t)*1000;assert wire()==before and repeat==body
     actual=Image.open(io.BytesIO(body)).convert('RGB');ref=refs[c['name']];assert actual.size==ref.size and actual.tobytes()==ref.tobytes()
     row=dict(case=c['name'],trial=trial,driver=driver,coldMs=cold,repeatMs=warm,wire=before,pixelsExact=True,repeatSourceIoZero=True,responseTimingHeaders={k:v for k,v in headers.items() if k.lower().startswith('x-river-')});rows.append(row);print(json.dumps(row),flush=True)
    finally:h.http(a.api+'/api/s3/sources/'+sid,method='DELETE')
 report['originNetworkEnd']=json.loads(h.http(a.origin_control+'/__netstat'))
 assert all(report['originNetworkEnd'][k]==origin['networkCounters'][k] for k in ['ListenDrops','ListenOverflows'])
 report['summary']=[dict(case=c['name'],driver=d,coldMedianMs=statistics.median(r['coldMs'] for r in rs),repeatMedianMs=statistics.median(r['repeatMs'] for r in rs),decodeMedianMs=statistics.median(float(r['responseTimingHeaders']['x-river-decode-ms']) for r in rs),serverMedianMs=statistics.median(float(r['responseTimingHeaders']['x-river-server-ms']) for r in rs),sourceMedianMs=statistics.median(float(r['responseTimingHeaders']['x-river-source-cache-ms']) for r in rs),routes=sorted({r['responseTimingHeaders']['x-river-decoder-route'] for r in rs})) for c in m['cases'] for d in drivers if (rs:=[r for r in rows if r['case']==c['name'] and r['driver']==d])]
 report['passed']=True
finally:a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
