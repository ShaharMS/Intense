"""Linux-side cached-response diagnostic; exact body hashes checked after timers."""
import concurrent.futures,hashlib,json,sys,threading,time,urllib.request,resource
from pathlib import Path
config=json.loads(Path(sys.argv[1]).read_text());rows=[]
def source_wire():
 with urllib.request.urlopen(config['sourceStats'],timeout=10) as response:return json.load(response)['wire']
before_wire=source_wire() if config.get('sourceStats') else None
for round_ in range(3):
 barrier=threading.Barrier(len(config['urls']))
 def get(url):
  barrier.wait(timeout=30);start=time.perf_counter()
  with urllib.request.urlopen(url,timeout=40) as r:body=r.read();headers=dict(r.headers)
  end=time.perf_counter();return dict(start=start,end=end,ms=(end-start)*1000,body=body,headers=headers)
 before=resource.getrusage(resource.RUSAGE_SELF)
 with concurrent.futures.ThreadPoolExecutor(max_workers=len(config['urls'])) as pool:responses=list(pool.map(get,config['urls']))
 after=resource.getrusage(resource.RUSAGE_SELF)
 for response,expected in zip(responses,config['hashes']):assert hashlib.sha256(response['body']).hexdigest()==expected
 rows.append(dict(round=round_,batchMs=(max(r['end'] for r in responses)-min(r['start'] for r in responses))*1000,
  processCpuMs=((after.ru_utime+after.ru_stime)-(before.ru_utime+before.ru_stime))*1000,
  clientMs=[r['ms'] for r in responses],responseBytes=[len(r['body']) for r in responses],headers=[r['headers'] for r in responses]))
 if before_wire is not None:
  after_wire=source_wire();rows[-1]['sourceWire']=after_wire
  if config.get('coldFirst') and round_==0:
   assert not any(before_wire.values()) and after_wire['gets']>0
   assert all(int(r['headers'].get('x-river-source-range-requests','0'))>0 for r in responses)
  else:assert after_wire==before_wire
  before_wire=after_wire
Path(sys.argv[2]).write_text(json.dumps(dict(passed=True,coldFirst=config.get('coldFirst',False),scope='Linux client2CPU, fresh HTTP connections, hashes outside timers. Optional first process/source-cold burst; later rounds are exact repeats. Not a maximum-throughput claim.',rows=rows),indent=2))
