"""Inspect immutable ready-corpus objects through actual River header loading.

No rendering, source writes or catalog installation. Cached structure inspection
must add zero origin IO. This is a diagnostic, not a latency benchmark.
"""
import argparse,hashlib,importlib.util,json,socket,threading,uuid
from pathlib import Path
from http.server import ThreadingHTTPServer
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--inventory',type=Path,required=True)
p.add_argument('--output',type=Path,required=True);p.add_argument('--origin-host',default='127.0.0.1')
p.add_argument('--expect-index-state',choices=['resident','metadataEvicted'])
a=p.parse_args();assert not a.output.exists();root=a.inventory.parent.resolve();inventory=json.loads(a.inventory.read_text(encoding='utf-8'))
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
cases=[];files={}
for n,row in enumerate(inventory):
 file=(root/row['file']).resolve();assert file.parent==root and file.stat().st_size==row['bytes']
 with file.open('rb') as stream:assert hashlib.file_digest(stream,'sha256').hexdigest()==row['sha256']
 name='image-'+str(n);cases.append(dict(name=name,sha256=row['sha256']));files[name]=file
counters={};lock=threading.Lock();base=h.make_origin(files,cases,dict(cases=cases),counters,lock)
class Origin(base):
 def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
server=ThreadingHTTPServer(('0.0.0.0',18997),Origin);threading.Thread(target=server.serve_forever,daemon=True).start()
report=dict(passed=False,scope='Actual metadata open plus cached opt-in structure; no render and no source writes.',rows=[])
sid='structure-'+uuid.uuid4().hex;endpoint='http://'+a.origin_host+':18997/'+sid
try:
 h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
 for case,entry in zip(cases,inventory):
  before=dict(counters.get(sid,dict(gets=0,heads=0,bytes=0)))
  try:
   loaded=json.loads(h.http(a.api+'/api/s3/sources/'+sid+'/assets',dict(key=case['name']+'.jp2'),'POST'))
   baseline=dict(counters[sid]);url=a.api+'/api/assets/'+loaded['assetId']+'/manifest'
   structure=json.loads(h.http(url+'?includeStructure=true'))['codestreamStructure']
   assert counters[sid]==baseline,'Diagnostic fetched additional bytes'
   assert structure['width']==entry['width'] and structure['height']==entry['height']
   if a.expect_index_state:assert structure['tilePartIndex']['state']==a.expect_index_state
   if a.expect_index_state=='metadataEvicted':assert 'usableTlmEntries' not in structure['tilePartIndex']
   result=dict(file=entry['file'],sha256=entry['sha256'],structure=structure,inspectionIoZero=True,openWire={k:baseline[k]-before[k] for k in before})
   report['rows'].append(result);print(json.dumps(result),flush=True)
  except RuntimeError as error:report['rows'].append(dict(file=entry['file'],error=str(error)));print(str(error),flush=True)
 report['passed']=all(r.get('inspectionIoZero') for r in report['rows'])
finally:
 h.http(a.api+'/api/s3/sources/'+sid,method='DELETE');server.shutdown();a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
