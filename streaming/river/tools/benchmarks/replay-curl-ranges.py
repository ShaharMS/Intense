"""Replay captured fixture GETs through native curl; not a full River speed comparison."""
import argparse,hashlib,json,statistics,subprocess,uuid
from pathlib import Path
from urllib.request import urlopen
p=argparse.ArgumentParser();p.add_argument('--profile',type=Path,required=True);p.add_argument('--manifest',type=Path,required=True);p.add_argument('--binary',type=Path,required=True);p.add_argument('--origin',required=True);p.add_argument('--control',required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();assert not a.output.exists()
profile=json.loads(a.profile.read_text());raw=a.manifest.read_bytes();m=json.loads(raw);assert profile['passed'] and profile['manifestSha256']==hashlib.sha256(raw).hexdigest()
binary=json.loads(a.binary.read_text());assert binary['container'].startswith('river-goal-')
fetch=lambda path:json.load(urlopen(a.control+path,timeout=30))
origin=fetch('/__manifest');assert origin['sha256']==profile['manifestSha256']
assert {c['sizeClass'] for c in m['cases']}=={'50MB','900-1000MB','10-15GB'}
plans=[]
for c in m['cases']:
 row=next(r for r in profile['rows'] if r['case']==c['name']);ranges=[]
 for entry in row['originRanges']:
  assert entry['body'] and entry['range'].startswith('bytes=')
  start,end=map(int,entry['range'][6:].split('-'));assert 0<=start<=end<c['sourceBytes'];ranges.append((start,end))
 assert len(ranges)==row['wire']['gets'] and sum(b-a+1 for a,b in ranges)==row['wire']['bytes']
 digest=hashlib.sha256()
 with (a.manifest.parent/c['file']).resolve().open('rb') as f:
  for start,end in ranges:
   f.seek(start);data=f.read(end-start+1);assert len(data)==end-start+1;digest.update(data)
 plans.append((c,row,ranges,digest.hexdigest()))
modes={'curl':binary['path']}
if 'sdkPath' in binary:modes['sdk']=binary['sdkPath']
rows=[];report=dict(passed=False,profileEvidence=str(a.profile),sourceImageId=profile['imageId'],clientImageId=binary.get('imageId'),runnerSha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),manifestSha256=profile['manifestSha256'],rows=rows,scope='Sequential exact-range replay through native libcurl and, when supplied, the same object_store SDK version as River. Same container, balanced client order, signed fixture requests, If-Match, range/ETag/size validation and independent concatenated payload SHA256. Fixture origin does not validate AWS authentication. Original sources unchanged. JP2 planning, caching and decoding are excluded. Curl prototype lacks production cancellation/error handling. Actual River may overlap some ranges. These are transport microbenchmarks, not end-to-end speedups or production qualification.')
try:
 for trial in range(4):
  for c,source,ranges,expected in (plans if trial%2==0 else list(reversed(plans))):
   order=list(modes.items());order=order if trial%2==0 else list(reversed(order))
   for mode,path in order:
    sid='replay-'+uuid.uuid4().hex;url=a.origin+'/'+sid+'/fixture/'+c['name']+'.jp2'
    text=url+'\n"'+c['sha256']+'"\n'+str(c['sourceBytes'])+' '+str(len(ranges))+'\n'+''.join(f'{x} {y}\n' for x,y in ranges)
    run=subprocess.run(['docker','exec','-i',binary['container'],path],input=text.encode('utf-8'),capture_output=True,timeout=60)
    assert run.returncode==0,run.stderr.decode(errors='replace');result=json.loads(run.stdout)
    assert result['passed'] and result['sha256']==expected
    wire=fetch('/__stats/'+sid)['wire'];assert wire==source['wire'] and result['gets']==wire['gets'] and result['bytes']==wire['bytes']
    row=dict(case=c['name'],trial=trial,mode=mode,**result);rows.append(row);print(json.dumps(row),flush=True)
 report['summary']=[dict(case=c['name'],mode=mode,rawMedianMs=statistics.median(r['wallMs'] for r in rs),rawCpuMedianMs=statistics.median(r['processCpuMs'] for r in rs),riverPreparationProfileMs=next(s['ms'] for s in source['stages'] if s['stage']=='total'),gets=source['wire']['gets'],bytes=source['wire']['bytes']) for c,source,_,_ in plans for mode in modes if (rs:=[r for r in rows if r['case']==c['name'] and r['mode']==mode])]
 report['passed']=True
finally:a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
