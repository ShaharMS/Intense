"""Read-only browser CORS contract checks against an existing River backend/proxy."""
import argparse,json,urllib.error,urllib.request
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--restricted',action='store_true');a=p.parse_args();assert not a.output.exists();report=dict(passed=False,api=a.api,checks=[])
def request(path,origin,method='GET',headers=None):
 values={'Origin':origin};values.update(headers or {})
 try:
  with urllib.request.urlopen(urllib.request.Request(a.api.rstrip('/')+path,headers=values,method=method),timeout=15) as r:return r.status,dict((k.lower(),v) for k,v in r.headers.items())
 except urllib.error.HTTPError as r:return r.code,dict((k.lower(),v) for k,v in r.headers.items())
def save():a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2)+'\n')
def record(name,**values):report['checks'].append(dict(name=name,**values));save()
try:
 origins=['https://crystal.hosted.example','http://localhost:19074','http://127.0.0.1:18765','https://other-host.example:8443','null']
 if a.restricted:origins=origins[:1]
 for origin in origins:
  for method in ['GET','HEAD','POST','PUT','DELETE','PATCH','OPTIONS']:
   status,h=request('/api/settings/read-policy',origin,'OPTIONS',{'Access-Control-Request-Method':method,'Access-Control-Request-Headers':'authorization,content-type,range,if-none-match,x-requested-with,traceparent,x-client-version','Access-Control-Request-Private-Network':'true'})
   assert 200<=status<300,(status,h);assert h.get('access-control-allow-origin')==origin,h
   assert h.get('access-control-allow-credentials')=='true',h;assert method in h.get('access-control-allow-methods','').split(','),h
   assert set(['authorization','content-type','range','if-none-match','x-requested-with','traceparent','x-client-version'])<=set(x.strip().lower() for x in h.get('access-control-allow-headers','').split(',')),h
   assert h.get('access-control-allow-private-network')=='true',h
   assert set(['origin','access-control-request-method','access-control-request-headers','access-control-request-private-network'])<=set(x.strip().lower() for x in h.get('vary','').split(',')),h
   assert h.get('access-control-max-age')=='600',h;record('preflight',origin=origin,method=method,status=status)
  for path,method,expected in [('/health','GET',200),('/health','HEAD',200),('/openapi.json','GET',200),('/api/settings/read-policy','GET',200),('/health','PATCH',405),('/api/assets/not-a-real-asset/manifest','GET',404),('/api/tiles/https%3A%2F%2Funconnected.invalid%2Ftest.jp2','GET',400),('/streamer/v1/0/0/0/tile.png?image=https%3A%2F%2Funconnected.invalid%2Ftest.jp2','GET',400)]:
   status,h=request(path,origin,method);assert status==expected,(path,status,h)
   assert h.get('access-control-allow-origin')==origin,(path,h);assert h.get('access-control-allow-credentials')=='true',h
   exposed=set(x.strip().lower() for x in h.get('access-control-expose-headers','').split(','))
   assert all(k in exposed for k in h if k.startswith('x-river-') and k!='x-river-backend'),(path,h)
   if status>=400:assert 'x-river-request-id' in exposed,(path,h)
   record('response',origin=origin,path=path,method=method,status=status,exposed=sorted(exposed))
 if a.restricted:
  for method,headers in [('GET',{}),('OPTIONS',{'Access-Control-Request-Method':'GET'})]:
   status,h=request('/health','https://blocked.example',method,headers);assert 'access-control-allow-origin' not in h,h;record('explicit-allowlist-blocks-other-origin',method=method)
 report['passed']=True;save();print(json.dumps(dict(passed=True,checks=len(report['checks']),api=a.api)))
except Exception as e:report['failure']=repr(e);save();raise
