"""Balanced hot-response transport diagnostic on each frozen size; source wire counts must remain unchanged."""
import argparse,hashlib,importlib.util,json,statistics,time,uuid
from pathlib import Path
from urllib.parse import urlencode
from urllib.request import urlopen
p=argparse.ArgumentParser();p.add_argument('--baseline-api',required=True);p.add_argument('--candidate-api',required=True);p.add_argument('--origin',required=True);p.add_argument('--origin-control',required=True);p.add_argument('--manifest',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--samples',type=int,default=40);a=p.parse_args();assert not a.output.exists() and a.samples>=20
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
m=json.loads(a.manifest.read_bytes());rows=[];report=dict(passed=False,scope='Hot exact-repeat transport diagnostic; one cold seed then40 balanced alternating reads per image and version. Header/body/server times are separated. No cold speedup inference.',rows=rows)
try:
 for c in m['cases']:
    active={};sids=[]
    try:
     for version,api in [('baseline',a.baseline_api),('candidate',a.candidate_api)]:
      sid='repeat-'+uuid.uuid4().hex;sids.append((api,sid));endpoint=a.origin+'/'+sid
      h.http(api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
      q=dict(image=endpoint+'/fixture/'+c['name']+'.jp2',driver=c['driver'],edge='crop',tileSize=c['tileSize'],minZoom=0,maxZoom=c['maxZoom'],tilePixelRatio=1)
      url=api+f"/streamer/v1/{c['z']}/{c['x']}/{c['y']}/tile.png?"+urlencode(q);body=h.http(url)
      active[version]=dict(url=url,body=body,stats=json.loads(h.http(a.origin_control+'/__stats/'+sid)),sid=sid,samples=[])
     for n in range(a.samples):
      for version in (['baseline','candidate'] if n%2==0 else ['candidate','baseline']):
       v=active[version];t=time.perf_counter()
       with urlopen(v['url']) as response:
        headers_at=time.perf_counter();body=response.read();done=time.perf_counter();assert body==v['body'];headers=dict(response.headers)
       v['samples'].append(dict(totalMs=(done-t)*1000,headersMs=(headers_at-t)*1000,bodyMs=(done-headers_at)*1000,serverMs=float(headers.get('x-river-server-ms','nan'))))
     row=dict(case=c['name'],versions={})
     for version,v in active.items():
      assert json.loads(h.http(a.origin_control+'/__stats/'+v['sid']))==v['stats']
      row['versions'][version]=dict(bodyBytes=len(v['body']),wireUnchanged=True,median={field:statistics.median(s[field] for s in v['samples']) for field in v['samples'][0]},samples=v['samples'])
     rows.append(row);print(json.dumps(dict(case=c['name'],medians={k:v['median'] for k,v in row['versions'].items()})),flush=True)
    finally:
     for api,sid in sids:h.http(api+'/api/s3/sources/'+sid,method='DELETE')
 report['passed']=True
finally:a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
