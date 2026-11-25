"""Isolate Grok subprocess decode, thread count, and output transport costs."""
import concurrent.futures,json,os,statistics,subprocess,tempfile,time
source='/app/sentinel-date-mosaics/2016-05-05.jp2'
samples=[]
for threads in [0,1,2,4]:
 for concurrency in [1,8,32]:
  def one(i):
   x=(i*7%81)*512;y=(i%22)*512
   region=f'{x},{y},{min(x+512,40980)},{min(y+512,10980)}'
   with tempfile.TemporaryDirectory() as folder:
    cmd=['grk_decompress','-i',source,'-o',folder+'/region.pam','-d',region,'-r','0']
    if threads:cmd+=['-H',str(threads)]
    start=time.perf_counter();r=subprocess.run(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    return dict(threads=threads,concurrency=concurrency,ms=(time.perf_counter()-start)*1000,ok=r.returncode==0,bytes=os.path.getsize(folder+'/region.pam') if r.returncode==0 else 0)
  one(0)
  with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
   samples+=list(pool.map(one,range(32)))
summary=[]
for threads in [0,1,2,4]:
 for concurrency in [1,8,32]:
  v=sorted(s['ms'] for s in samples if s['threads']==threads and s['concurrency']==concurrency)
  summary.append(dict(threads=threads,concurrency=concurrency,n=len(v),mean=statistics.mean(v),median=statistics.median(v),p95=v[30]))
print(json.dumps(dict(samples=samples,summary=summary),indent=2))
