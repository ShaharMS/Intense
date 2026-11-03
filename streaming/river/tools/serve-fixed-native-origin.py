"""Read-only six-file Range origin for transport controls inside the Docker VM."""
import argparse,json,socket,threading,time,hashlib
from pathlib import Path
from urllib.parse import urlsplit
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
p=argparse.ArgumentParser();p.add_argument('--manifest',type=Path,required=True);p.add_argument('--port',type=int,default=8000);p.add_argument('--tcp-nodelay',action='store_true');p.add_argument('--delay-ms',type=float,default=0);p.add_argument('--listen-backlog',type=int,default=256);p.add_argument('--body-bytes-per-second',type=int,default=0);p.add_argument('--switch-after-gets',type=int,default=0);p.add_argument('--after-get-body-rate',type=int,default=0);a=p.parse_args();assert 32<=a.listen_backlog<=4096 and min(a.body_bytes_per_second,a.switch_after_gets,a.after_get_body_rate)>=0
runner_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
raw=a.manifest.read_bytes();manifest=json.loads(raw);files={c['name']:((a.manifest.parent/c['file']).resolve(),c) for c in manifest['cases']};signatures={k:(v[0].stat().st_size,v[0].stat().st_mtime_ns) for k,v in files.items()}
counters={};traces={};pressure={};lock=threading.Lock()
rate_lock=threading.Lock();rate_next=0.0
def throttle_body(count,rate):
 global rate_next
 with rate_lock:
  now=time.monotonic();rate_next=max(now,rate_next)+count/rate;delay=rate_next-now
 if delay>0:time.sleep(delay)
def network_counters():
 lines=Path('/proc/net/netstat').read_text().splitlines();rows=[x.split()[1:] for x in lines if x.startswith('TcpExt:')];values=dict(zip(rows[0],rows[1]))
 return {k:int(values.get(k,0)) for k in ['ListenOverflows','ListenDrops','TCPSynRetrans','TCPTimeouts']}
class OriginServer(ThreadingHTTPServer):request_queue_size=a.listen_backlog
class Origin(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1';wbufsize=262144
 def setup(self):
  super().setup()
  if a.tcp_nodelay:self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
 def log_message(self,*args):pass
 def response(self,value):
  data=json.dumps(value).encode();self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
 def do_HEAD(self):self.serve(False)
 def do_GET(self):self.serve(True)
 def serve(self,body):
  parts=urlsplit(self.path).path.strip('/').split('/')
  if parts==['__netstat']:return self.response(network_counters())
  if parts==['__manifest']:return self.response({'sha256':hashlib.sha256(raw).hexdigest(),'transport':'Linux native Docker bridge','tcpNoDelay':a.tcp_nodelay,'listenBacklog':a.listen_backlog,'responseDelayMs':a.delay_ms,'sharedBodyBytesPerSecond':a.body_bytes_per_second,'runnerSha256':runner_sha256,'networkCounters':network_counters(),'materialization':json.loads(Path('/data/materialized.json').read_text()) if Path('/data/materialized.json').exists() else None,'files':{k:v[0].stat().st_size for k,v in files.items()}})
  if len(parts)==2 and parts[0]=='__stats':
   with lock:value={'wire':dict(counters.get(parts[1],dict(gets=0,heads=0,bytes=0))),'ranges':[dict(x) for x in traces.get(parts[1],[])],'pressure':dict(pressure.get(parts[1],dict(active=0,peak=0)))}
   return self.response(value)
  if len(parts)!=3 or parts[1]!='fixture' or parts[2].removesuffix('.jp2') not in files:return self.send_error(404)
  sid=parts[0];name=parts[2].removesuffix('.jp2');path,case=files[name];stat=path.stat();size=stat.st_size
  if (size,stat.st_mtime_ns)!=signatures[name]:return self.send_error(412,'Frozen source changed')
  start,end=0,size-1;partial='Range' in self.headers
  if partial:
   try:
    first,last=self.headers['Range'].removeprefix('bytes=').split('-');start=int(first);end=min(int(last) if last else size-1,size-1);assert 0<=start<=end<size
   except (ValueError,AssertionError):return self.send_error(416)
  entry={'range':self.headers.get('Range'),'body':body,'start':time.monotonic()}
  with lock:
   c=counters.setdefault(sid,dict(gets=0,heads=0,bytes=0));c['gets' if body else 'heads']+=1;traces.setdefault(sid,[]).append(entry)
   rate=a.after_get_body_rate if a.switch_after_gets and c['gets']>a.switch_after_gets else a.body_bytes_per_second
   entry['bodyRate']=rate
  with lock:
   q=pressure.setdefault(sid,dict(active=0,peak=0));q['active']+=1;q['peak']=max(q['peak'],q['active'])
  try:
   if a.delay_ms:time.sleep(a.delay_ms/1000)
   self.send_response(206 if partial else 200);self.send_header('Content-Length',str(end-start+1));self.send_header('Accept-Ranges','bytes');self.send_header('ETag','"'+case['sha256']+'"')
   if partial:self.send_header('Content-Range',f'bytes {start}-{end}/{size}')
   self.end_headers()
   if rate:self.wfile.flush()
   if body:
    with path.open('rb') as f:
     f.seek(start);remaining=end-start+1
     while remaining:
      chunk=f.read(min(65536,remaining))
      try:
       if rate:throttle_body(len(chunk),rate)
       self.wfile.write(chunk)
       if rate:self.wfile.flush()
      except (BrokenPipeError,ConnectionResetError):break
      with lock:counters[sid]['bytes']+=len(chunk)
      remaining-=len(chunk)
   # Unlike the earlier trace, include the final buffered flush in origin timing.
   self.wfile.flush()
  finally:
   entry['end']=time.monotonic();entry['durationMs']=(entry['end']-entry['start'])*1000
   with lock:pressure[sid]['active']-=1
OriginServer(('0.0.0.0',a.port),Origin).serve_forever()
