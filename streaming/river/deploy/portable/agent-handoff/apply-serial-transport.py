"""Review/apply only the effective serial transport; never touch S3 definitions."""
import argparse,copy,datetime,json,urllib.request,urllib.error
from pathlib import Path

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--api',default='http://127.0.0.1:8080')
    choice=parser.add_mutually_exclusive_group(required=True)
    choice.add_argument('--transport',choices=['sdk','auto'])
    choice.add_argument('--restore',type=Path,help='Restore only the transport from a previous record, preserving current other settings.')
    parser.add_argument('--apply',action='store_true')
    parser.add_argument('--record',type=Path,help='New public-settings rollback record; existing files are never overwritten.')
    args=parser.parse_args()
    url=args.api.rstrip('/')+'/api/settings/read-policy'
    def call(value=None):
        request=urllib.request.Request(url,data=None if value is None else json.dumps(value).encode(),method='GET' if value is None else 'PUT',headers={'Content-Type':'application/json'})
        try:
            with urllib.request.urlopen(request,timeout=30) as response:
                if value is None:return json.load(response)['settings']
                response.read()
        except urllib.error.HTTPError as error:
            raise RuntimeError(f'Read-policy HTTP {error.code}: {error.read(8192).decode(errors="replace")}') from error
    before=call();transport=args.transport
    if args.restore:
        transport=json.loads(args.restore.read_text(encoding='utf-8'))['before']['serialTransport']
        if transport not in ['sdk','auto','curl']:raise ValueError('Invalid recorded serial transport')
    proposed=copy.deepcopy(before);proposed['serialTransport']=transport
    changed=sorted(k for k in set(before)|set(proposed) if before.get(k)!=proposed.get(k))
    assert set(changed)<=set(['serialTransport'])
    record=dict(applied=False,before=before,proposed=proposed,changedFields=changed)
    print(json.dumps(record,indent=2))
    if not args.apply or not changed:return
    stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    path=args.record or Path('river-serial-transport-'+stamp+'.json')
    with path.open('x',encoding='utf-8') as output:json.dump(record,output,indent=2)
    call(proposed)
    effective=call()
    assert effective==proposed,'Effective settings differ; inspect the API and rollback record.'
    record.update(applied=True,effective=effective)
    path.write_text(json.dumps(record,indent=2),encoding='utf-8')
    print('Transport updated. Other effective read-policy settings and all S3 definitions were left unchanged. Record: '+str(path))

if __name__=='__main__':main()
