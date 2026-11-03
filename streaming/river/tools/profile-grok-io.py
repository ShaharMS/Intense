import concurrent.futures,json,os,statistics,subprocess,tempfile,time
samples=[]
for location in ['bind','linux']:
 source='/app/sentinel-date-mosaics/2016-05-05.jp2' if location=='bind' else '/tmp/river-profile-source.jp2'
 for transport in ['file','stdout']:
  for concurrency in [1,8,32]:
   def one(i):
    x=(i*7%81)*512;y=(i%22)*512
    with tempfile.TemporaryDirectory() as folder:
     cmd=['grk_decompress','-i',source,'-d',f'{x},{y},{min(x+512,40980)},{min(y+512,10980)}','-r','0']
     cmd+=['-o',folder+'/region.pam'] if transport=='file' else ['-O','pnm']
     start=time.perf_counter();r=subprocess.run(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
     return dict(location=location,transport=transport,concurrency=concurrency,ms=(time.perf_counter()-start)*1000,ok=r.returncode==0,bytes=len(r.stdout) if transport=='stdout' else os.path.getsize(folder+'/region.pam') if r.returncode==0 else 0)
   one(0)
   with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:samples+=list(pool.map(one,range(32)))
summary=[]
for location in ['bind','linux']:
 for transport in ['file','stdout']:
  for concurrency in [1,8,32]:
   v=sorted(s['ms'] for s in samples if (s['location'],s['transport'],s['concurrency'])==(location,transport,concurrency))
   summary.append(dict(location=location,transport=transport,concurrency=concurrency,n=len(v),mean=statistics.mean(v),median=statistics.median(v),p95=v[30]))
print(json.dumps(dict(samples=samples,summary=summary),indent=2))
