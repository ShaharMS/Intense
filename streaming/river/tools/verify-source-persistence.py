"""Restart a task-owned container and verify private S3 configuration survives."""
import argparse,json,subprocess,time
from urllib.request import Request,urlopen
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--container',required=True);a=p.parse_args()
assert a.container.startswith('river-goal-')
def request(path,body=None,method=None):
    with urlopen(Request(a.api+path,data=json.dumps(body).encode() if body else None,method=method,
                         headers={'Authorization':'Bearer fixture-admin','Content-Type':'application/json'})) as r:
        return json.loads(r.read()) if r.status!=204 else None
def restart():
    subprocess.run(['docker','restart',a.container],check=True,capture_output=True)
    deadline=time.monotonic()+30
    while True:
        try:
            with urlopen(a.api+'/health',timeout=1):return
        except OSError:
            if time.monotonic()>deadline:raise
            time.sleep(.1)
source={'id':'persist-check','name':'persist-check','endpoint':'http://host.docker.internal:19999','allowHttp':True,
        'bucket':'fixture','accessKeyId':'private-key-for-test','secretAccessKey':'private-secret-for-test','prefix':'first'}
request('/api/s3/sources',source);restart()
values=request('/api/s3/sources');record=next(v for v in values if v['id']==source['id'])
assert record['prefix']=='first' and 'private-secret-for-test' not in json.dumps(values) and 'accessKeyId' not in record
source['prefix']='updated';request('/api/s3/sources',source);restart()
assert next(v for v in request('/api/s3/sources') if v['id']==source['id'])['prefix']=='updated'
mode=subprocess.check_output(['docker','exec',a.container,'stat','-c','%a','/var/lib/river/config/s3-connections.json'],text=True).strip()
assert mode=='600',mode
request('/api/s3/sources/'+source['id'],method='DELETE');restart()
assert not any(v['id']==source['id'] for v in request('/api/s3/sources'))
print('PASS: connection create/update/delete survives restart, API hides credentials, config mode 0600')
