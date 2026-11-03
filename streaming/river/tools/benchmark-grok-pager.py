"""Experimental isolated demand-paged Grok: real pinned HTTP ranges, never zero-filled input."""
import argparse,importlib.util,io,json,pathlib,socket,subprocess,threading,time,urllib.request
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from PIL import Image
parser=argparse.ArgumentParser();parser.add_argument('--reset-fixture',action='store_true');parser.add_argument('--read-kib',type=int,default=0);parser.add_argument('--latency-ms',type=float,default=0);a=parser.parse_args();assert a.latency_ms>=0
root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/native-grok-reader-m189';file=(out/'reset-style.jp2') if a.reset_fixture else (root/'corpus/cache/sentinel-date-mosaics/2016-09-12.jp2');stat=file.stat();events=[];phase=''
class Origin(ThreadingHTTPServer):request_queue_size=256
class S3(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
 def log_message(self,*_):pass
 def do_GET(self):
  assert self.headers.get('If-Match')=='"stable"';assert self.headers.get('Range');lo,hi=map(int,self.headers['Range'].removeprefix('bytes=').split('-'));assert 0<=lo<=hi<stat.st_size and hi-lo+1<=262144
  assert file.stat().st_mtime_ns==stat.st_mtime_ns
  with file.open('rb') as f:f.seek(lo);body=f.read(hi-lo+1)
  time.sleep(a.latency_ms/1000);events.append(dict(phase=phase,start=lo,end=hi,bytes=len(body)));self.send_response(206);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Range',f'bytes {lo}-{hi}/{stat.st_size}');self.send_header('ETag','"stable"');self.end_headers();self.wfile.write(body)
server=Origin(('0.0.0.0',0),S3);threading.Thread(target=server.serve_forever,daemon=True).start()
spec=importlib.util.spec_from_file_location('reference',root/'tools/benchmark-s3-corpus.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
refs={'tile':Image.open(root/'reports/gdal-random-m143/full-detail-core-timed.gdal.png').convert('RGBA'),'preview':m.load_preview_reference(file,6,1,root/'reports/grok-large-reference-m54.ppm')};results=[]
if a.reset_fixture:refs={'tile':Image.open(out/'reset-tile.reference.png').convert('RGBA'),'preview':Image.open(out/'reset-preview.reference.png').convert('RGBA')}
try:
 cases=[('tile',[2048,2048,2560,2560],0),('preview',[0,0,5000,5000],5)] if a.reset_fixture else [('tile',[28160,6656,28672,7168],0),('preview',[0,0,40980,10980],6)]
 for case,region,reduce in cases:
  phase=case;name=('http-reset-paged-' if a.reset_fixture else 'http-paged-')+case+(f'-rtt{a.latency_ms:g}' if a.latency_ms else '')+(f'-read{a.read_kib}' if a.read_kib else '');start=time.perf_counter();args=['docker','run','--rm','--name','river-goal-grok-http-'+case,'--mount',f'type=bind,source={out},target=/study','--mount',f'type=bind,source={file.resolve()},target=/input.jp2,readonly','-e',f'PAGER_READ_KIB={a.read_kib}' if a.read_kib else 'PAGER_READ_KIB=0','-e',f'HTTP_SOURCE=http://host.docker.internal:{server.server_port}/fixture/image.jp2','river-reader-lab:2026.10.19-grok-callbacks','/study/http_pager_probe-next','/input.jp2',str(reduce),*map(str,region),f'/study/{name}.rgba','4096',f'/study/{name}.json'];child=subprocess.run(args,capture_output=True,text=True,timeout=120);assert child.returncode==0,child.stdout+child.stderr
  d=json.loads((out/(name+'.json')).read_text());assert d['ok'];im=Image.frombytes('RGBA',(d['width'],d['height']),(out/(name+'.rgba')).read_bytes());assert im.size==refs[case].size and im.tobytes()==refs[case].tobytes();traffic=[e for e in events if e['phase']==case];assert sum(e['bytes'] for e in traffic)==d['bytes'];d.update(readKiB=a.read_kib,injectedLatencyMs=a.latency_ms,exactPixels=True,independentReference=(case=='tile' or not a.reset_fixture),independentPreviewReference=not a.reset_fixture,httpRequests=len(traffic),wallIncludingDockerMs=(time.perf_counter()-start)*1000,scope='Isolated native worker prototype using normal Docker permissions. All compressed bytes came from recorded HTTP range requests with If-Match. Source length obtained from read-only file metadata; no source content read directly by HTTP worker. Decoder ms excludes container startup and source metadata lookup. No production integration or throughput claim.');(out/(name+'.json')).write_text(json.dumps(d,indent=2));results.append({k:v for k,v in d.items() if k!='reads'});print(json.dumps(results[-1]),flush=True)
 (out/(('http-reset-summary' if a.reset_fixture else 'http-summary')+(f'-rtt{a.latency_ms:g}' if a.latency_ms else '')+(f'-read{a.read_kib}' if a.read_kib else '')+'.json')).write_text(json.dumps(dict(results=results,events=events,sourceImagesUnchanged=True),indent=2))
finally:server.shutdown()
