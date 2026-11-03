"""Offline placement model, never used as a source index or pixel reference.
Predictions are evaluated on independent real Psot chains. Only probes and
already traversed headers become available to the modeled walkers.
"""
import bisect,json,pathlib,statistics
root=pathlib.Path(__file__).resolve().parents[1];out=root/'reports/index-placement-m187';out.mkdir(exist_ok=True)
def parts(path):
 with path.open('rb') as f:
  pos=0
  while True:
   f.seek(pos);h=f.read(16);n=int.from_bytes(h[:4],'big');kind=h[4:8];head=8
   if n==1:n=int.from_bytes(h[8:16],'big');head=16
   if not n:n=path.stat().st_size-pos
   if kind==b'jp2c':start=pos+head;end=pos+n;break
   pos+=n
  pos=start+2
  while True:
   f.seek(pos);b=f.read(4)
   if b[:2]==b'\xff\x90':break
   pos+=2+int.from_bytes(b[2:4],'big')
  rows=[]
  while pos<end-2:
   f.seek(pos);b=f.read(12);assert b[:4]==b'\xff\x90\x00\x0a';n=int.from_bytes(b[6:10],'big');assert n>=14
   rows.append(dict(offset=pos,length=n,tile=int.from_bytes(b[4:6],'big')));pos+=n
  return rows,end

def simulate(rows,end,target,count,budget,power=1.0,area=False,whole=False):
 offsets=[v['offset'] for v in rows];root=64 if whole else 0;initial=root
 if area and not whole:
  k=rows[target]['tile'];tx,ty=k%41,k//41
  fraction=(ty*1024*40980+min(1024,10980-ty*1024)*min(40980,(tx+1)*1024))/(40980*10980)
 else:fraction=1 if whole else (rows[target]['tile']+1)/451
 hinted=offsets[0]+int((end-2-offsets[0])*fraction)
 window=min(4<<20,(budget-(64<<10))//count)//1024*1024;spans=[];lanes=[]
 for n in range(1,count+1):
  center=offsets[root]+int((hinted-offsets[root])*(n/(count+1))**power)
  lo=max(offsets[root],min(center-window//2,end-2-window))//1024*1024;hi=lo+window;spans.append((lo,hi))
  seed=bisect.bisect_left(offsets,lo)
  if seed<len(rows) and offsets[seed]+12<=hi:lanes.append([seed,seed+1])
 blocks=set(range(3));waves=0
 def cached(i):
  a=offsets[i]//1024;b=(offsets[i]+11)//1024
  return all(k in blocks or any(lo<=k*1024 and (k+1)*1024<=hi for lo,hi in spans) for k in range(a,b+1))
 def fetch(i):
  n=2048 if whole else 12
  blocks.update(range(offsets[i]//1024,(min(end,offsets[i]+n)+1023)//1024))
 while root<=target:
  connected=[(i,x) for i,x in enumerate(lanes) if x[0]<=root<x[1]]
  if connected:
   i,x=max(connected,key=lambda q:q[1][1]);root=x[1];lanes.pop(i);continue
  if cached(root):root+=1;continue
  waves+=1;fetch(root)
  for start,cursor in lanes:
   if cursor<len(rows) and cursor-start<128:fetch(cursor)
  for lane in lanes:
   while lane[1]<len(rows) and lane[1]-lane[0]<128 and cached(lane[1]):lane[1]+=1
  root+=1
 return dict(waves=waves,probeBytes=window*count,estimatedSourceMs=waves*12+window*count/(40*1024*1024)*1000)
records=[]
for name in ['2016-09-12','2015-12-07']:
 rows,end=parts(root/f'corpus/cache/sentinel-date-mosaics/{name}.jp2');assert len(rows)==451
 for count in range(2,9):
  for budget in [8<<20,12<<20,16<<20]:
   for power in [0.85,1.0,1.15]:
    for area in [False,True]:
     cases=[simulate(rows,end,t,count,budget,power,area) for t in [100,200,273,350,400]]+[simulate(rows,end,450,count,budget,power,area,True)]
     records.append(dict(source=name,count=count,budgetMiB=budget>>20,power=power,area=area,score=statistics.mean(c['estimatedSourceMs'] for c in cases),tile=cases[2],preview=cases[-1]))
best=[]
for n in [2,3,4,5,6,7,8]:
 for b in [8,12,16]:
  for power in [.85,1.,1.15]:
   for area in [False,True]:
    group=[v for v in records if (v['count'],v['budgetMiB'],v['power'],v['area'])==(n,b,power,area)]
    best.append(dict(count=n,budgetMiB=b,power=power,area=area,score=statistics.mean(v['score'] for v in group),cases=group))
report=dict(scope='Offline approximate latency bound, not a network speedup claim. Reads original files only at actual SOT offsets; source images unchanged. Uses observed 40 MiB/s probe cost and 12 ms header waves. Candidates must pass real cold IO and pixels before deployment.',top=sorted(best,key=lambda v:v['score'])[:10],baseline=next(v for v in best if (v['count'],v['budgetMiB'],v['power'],v['area'])==(4,16,1.,False)))
(out/'model.json').write_text(json.dumps(report,indent=2));print(json.dumps(dict(top=[{k:v for k,v in a.items() if k!='cases'} for a in report['top'][:5]],baseline={k:v for k,v in report['baseline'].items() if k!='cases'})))
