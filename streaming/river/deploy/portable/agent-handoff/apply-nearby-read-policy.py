"""Read/merge only read-policy settings. Never replace S3 configuration."""
import argparse,copy,datetime,json,urllib.request,urllib.error
from pathlib import Path

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--api',default='http://127.0.0.1:8080')
    parser.add_argument('--source-requests',type=int,choices=[4,8,16],default=8)
    parser.add_argument('--transport',choices=['sdk','auto'],default='sdk')
    parser.add_argument('--apply',action='store_true',help='Persist the displayed read-policy merge.')
    parser.add_argument('--record',type=Path,help='New JSON record file; existing files are never overwritten.')
    args=parser.parse_args()
    def call(body=None):
        request=urllib.request.Request(args.api.rstrip('/')+'/api/settings/read-policy',
            data=None if body is None else json.dumps(body).encode(),
            method='GET' if body is None else 'PUT',headers={'Content-Type':'application/json'})
        try:
            with urllib.request.urlopen(request,timeout=30) as response:
                if body is None:return json.load(response)
                response.read()
        except urllib.error.HTTPError as error:
            raise RuntimeError(f'Read-policy HTTP {error.code}: {error.read(8192).decode(errors="replace")}') from error
    before=call()['settings']
    patch=json.loads(Path(__file__).with_name('read-policy-nearby.patch.json').read_text())
    assert set(patch)=={'mode','minimumSamples','maxSourceRequests','maxOverfetchRatio','serialTransport','refreshPayloadCosts'}
    after=copy.deepcopy(before);after.update(patch)
    after['maxSourceRequests']=args.source_requests;after['serialTransport']=args.transport
    record=dict(applied=False,before=before,proposed=after)
    print(json.dumps(record,indent=2))
    if not args.apply:return
    stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    target=args.record or Path('river-read-policy-'+stamp+'.json')
    with target.open('x',encoding='utf-8') as output:json.dump(record,output,indent=2)
    call(after)
    result=call()['settings']
    assert all(result[key]==value for key,value in after.items()),'Effective settings differ; inspect API response.'
    record.update(applied=True,effective=result)
    target.write_text(json.dumps(record,indent=2),encoding='utf-8')
    print('Persisted read policy; S3 connections were not modified. Record: '+str(target))

if __name__=='__main__':main()
