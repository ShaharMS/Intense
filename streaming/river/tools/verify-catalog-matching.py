"""Verify matching-result pages across connected S3s without reading image bodies."""
import argparse,json,threading,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.parse import parse_qs,urlparse,urlencode
from urllib.request import Request,urlopen
from urllib.error import HTTPError

p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output');a=p.parse_args()
calls=[];bodies=[]
keys=[f'folder/000-noise-{i:04}.txt' for i in range(625)]+[f'folder/needle-{i:03}.jp2' for i in range(60)]
class S3(BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_HEAD(self):bodies.append(self.path);self.send_error(500)
    def do_GET(self):
        q=parse_qs(urlparse(self.path).query)
        if 'list-type' not in q:bodies.append(self.path);self.send_error(500);return
        limit=int(q['max-keys'][0]);assert 1<=limit<=50
        start=int(q.get('continuation-token',['0'])[0]);prefix=q.get('prefix',[''])[0]
        selected=[key for key in keys if key.startswith(prefix)];page=selected[start:start+limit];end=start+len(page)
        calls.append((urlparse(self.path).path,limit,start))
        contents=''.join(f'<Contents><Key>{key}</Key><LastModified>2026-10-15T00:00:00Z</LastModified><ETag>"fixture"</ETag><Size>12345</Size></Contents>' for key in page)
        more=end<len(selected);continuation=f'<NextContinuationToken>{end}</NextContinuationToken>' if more else ''
        body=f'<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">{contents}<IsTruncated>{str(more).lower()}</IsTruncated>{continuation}</ListBucketResult>'.encode()
        self.send_response(200);self.send_header('Content-Type','application/xml');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
server=ThreadingHTTPServer(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
def request(path,body=None,method=None):
    headers={'Authorization':'Bearer fixture-admin','Content-Type':'application/json'}
    with urlopen(Request(a.api+path,data=None if body is None else json.dumps(body).encode(),headers=headers,method=method),timeout=30) as r:
        content=r.read();return json.loads(content) if content else None
ids=['search-'+uuid.uuid4().hex[:12]+suffix for suffix in ['a','b']];created=[]
try:
    for index,source in enumerate(ids):
        request('/api/s3/sources',dict(id=source,name=source,endpoint=f'http://host.docker.internal:{server.server_port}',bucket=f'fixture{index}',region='us-east-1',prefix='folder',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True,virtualHostedStyle=False));created.append(source)
    pages=[];cursor=None;current=[];all_results=[];responses=[];first_cursor=None
    while True:
        query=dict(source='all',q='NEEDLE',limit=50-len(current))
        if cursor:query['cursor']=cursor
        before=len(calls);body=request('/api/assets?'+urlencode(query));delta=len(calls)-before
        assert delta<=10 and body['providerRequests']==delta
        assert len(body['objects'])<=int(query['limit'])
        if not first_cursor:first_cursor=body.get('nextCursor')
        current+=body['objects'];cursor=body.get('nextCursor');responses.append(dict(matches=len(body['objects']),providerRequests=delta))
        if len(current)==50 or not cursor:pages.append(len(current));all_results+=current;current=[]
        if not cursor:break
    assert pages==[50,50,20],pages
    assert len({item['assetId'] for item in all_results})==120
    assert {item['sourceId'] for item in all_results}==set(ids)
    assert not bodies,bodies
    try:request('/api/assets?'+urlencode(dict(source='all',q='different',cursor=first_cursor)))
    except HTTPError as error:assert error.code==400
    else:raise AssertionError('A cursor was accepted with a different query')
    result=dict(pages=pages,totalMatches=len(all_results),providerCalls=len(calls),imageBodyRequests=len(bodies),responses=responses,passed=True)
    if a.output:
        from pathlib import Path
        Path(a.output).write_text(json.dumps(result,indent=2))
    print(json.dumps(result))
finally:
    for source in created:request('/api/s3/sources/'+source,method='DELETE')
    server.shutdown()
