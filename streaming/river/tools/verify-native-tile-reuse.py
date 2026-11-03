"""Exact local WMTS pixels and cold/repeat timing with native tile reuse.
Run on a fresh instance with RIVER_RASTER_CACHE_BYTES=0 to isolate native reuse.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import io
import math
import json
from pathlib import Path
import time
from urllib.parse import urlencode
from urllib.request import urlopen
from urllib.request import Request
import atexit
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from PIL import Image

parser=argparse.ArgumentParser()
parser.add_argument('--api',required=True)
parser.add_argument('--driver',choices=('openjpeg','jp2grok','nvjpeg2000'),default='openjpeg')
parser.add_argument('--workers',type=int,default=2)
parser.add_argument('--format',choices=('png','webp'),default='png')
parser.add_argument('--output',type=Path)
parser.add_argument('--s3',action='store_true')
parser.add_argument('--endpoint-host',default='host.docker.internal')
args=parser.parse_args()
root=Path(__file__).resolve().parents[1]
asset='94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290'
reference=Image.open(root/'corpus/cache/Bretagne2.j2k').convert('RGBA')
traffic={'bytes':0,'ranges':0}
if args.s3:
    data=(root/'corpus/cache/Bretagne2.j2k').read_bytes()
    traffic_lock=threading.Lock()
    class S3(BaseHTTPRequestHandler):
        def log_message(self,*_): pass
        def do_HEAD(self):
            self.send_response(200)
            self.send_header('Content-Length',str(len(data)))
            self.send_header('ETag','"reuse-fixture"')
            self.send_header('Last-Modified','Wed, 14 Oct 2026 00:00:00 GMT')
            self.end_headers()
        def do_GET(self):
            if 'list-type=' in self.path:
                body=('<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><IsTruncated>false</IsTruncated>'
                    '<Contents><Key>Bretagne2.j2k</Key><LastModified>2026-10-14T00:00:00Z</LastModified>'
                    '<ETag>"reuse-fixture"</ETag><Size>'+str(len(data))+'</Size></Contents></ListBucketResult>').encode()
                self.send_response(200)
            else:
                assert self.headers.get('Range') and self.headers.get('If-Match')=='"reuse-fixture"'
                lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'))
                body=data[lo:hi+1]
                with traffic_lock:
                    traffic['bytes']+=len(body); traffic['ranges']+=1
                self.send_response(206)
                self.send_header('Content-Range',f'bytes {lo}-{hi}/{len(data)}')
            self.send_header('Content-Length',str(len(body)))
            self.send_header('ETag','"reuse-fixture"')
            self.end_headers(); self.wfile.write(body)
    server=ThreadingHTTPServer(('0.0.0.0',0),S3)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    source_id='native-reuse-'+uuid.uuid4().hex
    headers={'Content-Type':'application/json','Authorization':'Bearer fixture-admin'}
    config=dict(id=source_id,name='Native reuse fixture',endpoint=f'http://{args.endpoint_host}:{server.server_port}',
        bucket='fixture',accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True)
    with urlopen(Request(args.api+'/api/s3/sources',data=json.dumps(config).encode(),headers=headers)) as response:
        assert response.status==200
    def cleanup():
        try:
            with urlopen(Request(args.api+'/api/s3/sources/'+source_id,method='DELETE',headers=headers)): pass
        finally: server.shutdown()
    atexit.register(cleanup)
    with urlopen(args.api+'/api/assets?'+urlencode(dict(source=source_id,limit=1))) as response:
        locator=json.load(response)['objects'][0]['assetId']
    with urlopen(args.api+'/api/assets/'+locator+'/manifest') as response:
        asset=json.load(response)['assetId']
jobs=[(x,y) for y in range((reference.height+511)//512) for x in range((reference.width+511)//512)]
def one(job):
    x,y=job
    query=urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,
        STYLE='default',FORMAT='image/'+args.format,TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX='5',
        TILEROW=y,TILECOL=x,DRIVER=args.driver))
    started=time.perf_counter()
    with urlopen(args.api+'/wmts?'+query,timeout=30) as response:
        body=response.read()
        route=response.headers['X-River-Decoder-Route']
        encode_ms=float(response.headers['X-River-Encode-Ms'])
    elapsed=(time.perf_counter()-started)*1000
    actual=Image.open(io.BytesIO(body)).convert('RGBA')
    expected=reference.crop((x*512,y*512,min((x+1)*512,reference.width),min((y+1)*512,reference.height)))
    assert actual.size==expected.size,job
    squared=0.0; samples=0
    if args.format=='png': assert actual.tobytes()==expected.tobytes(),job
    else:
        import numpy as np
        a=np.asarray(actual); e=np.asarray(expected)
        assert np.array_equal(a[:,:,3],e[:,:,3]),'alpha changed'
        delta=a[:,:,:3].astype(np.float64)-e[:,:,:3]
        squared=float(np.square(delta).sum()); samples=delta.size
    assert route!='encoded-raster-cache','disable encoded cache to isolate native reuse'
    return dict(elapsed=elapsed,bytes=len(body),encodeMs=encode_ms,squared=squared,samples=samples)
results=[]
for state in ('cold','repeat'):
    before=dict(traffic)
    started=time.perf_counter()
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        times=list(pool.map(one,jobs))
    result=dict(state=state,tiles=len(jobs),workers=args.workers,format=args.format,
        pixels='exact' if args.format=='png' else 'lossy RGB, exact alpha',
        wallMs=(time.perf_counter()-started)*1000,p95Ms=sorted(t['elapsed'] for t in times)[-2],
        encodedBytes=sum(t['bytes'] for t in times),encodeMs=sum(t['encodeMs'] for t in times))
    if args.format=='webp':
        mse=sum(t['squared'] for t in times)/sum(t['samples'] for t in times)
        result['psnrDb']=10*math.log10(255**2/mse) if mse else None
        assert result['psnrDb'] is None or result['psnrDb']>=35,result
    if args.s3:
        result.update(sourceBytes=traffic['bytes']-before['bytes'],rangeRequests=traffic['ranges']-before['ranges'])
        if state=='repeat': assert result['sourceBytes']==0 and result['rangeRequests']==0,result
    results.append(result)
    print(json.dumps(result),flush=True)
if args.output: args.output.write_text(json.dumps(results,indent=2),encoding='utf-8')
