"""Model bounded merges of a complete synthetic-origin prefix trace. No timing claim."""
import argparse,heapq,json
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('profile',type=Path);p.add_argument('--output',type=Path,required=True)
a=p.parse_args();assert not a.output.exists()
profile=json.loads(a.profile.read_text());rows=[]
def merge(original,target,ratio,max_range):
 ranges=[list(r) for r in original];n=len(ranges);nxt=list(range(1,n))+[-1];previous=[-1]+list(range(n-1));live=[True]*n
 useful=sum(b-a for a,b in ranges);total=useful;budget=useful*ratio
 heap=[(ranges[i+1][0]-ranges[i][1],i,i+1) for i in range(n-1)];heapq.heapify(heap);count=n
 while count>target and heap:
  gap,left,right=heapq.heappop(heap)
  if not live[left] or not live[right] or nxt[left]!=right:continue
  if ranges[right][1]-ranges[left][0]>max_range:continue
  if total+gap>budget:break
  total+=gap;ranges[left][1]=ranges[right][1];live[right]=False;nxt[left]=nxt[right];count-=1
  if nxt[left]>=0:
   after=nxt[left];previous[after]=left;heapq.heappush(heap,(ranges[after][0]-ranges[left][1],left,after))
  if previous[left]>=0:
   before=previous[left];heapq.heappush(heap,(ranges[left][0]-ranges[before][1],before,left))
 return dict(requests=count,bytes=total,originalBytes=useful,ratioLimit=ratio,maxRangeBytes=max_range)
for row in profile['rows']:
 assert len(row['originRanges'])==row['origin']['gets']
 ranges=sorted((int(e['range'].split('=')[1].split('-')[0]),int(e['range'].split('-')[1])+1) for e in row['originRanges'])
 # Remove only the contiguous initial-header run. Retain all remaining ranges,
 # which this fixture's earlier trace proves are the selected prefix wave.
 end=0;headers=[]
 while ranges and ranges[0][0]==end:
  headers.append(ranges.pop(0));end=headers[-1][1]
 assert all(ranges[i][1]<=ranges[i+1][0] for i in range(len(ranges)-1)),'Overlapping attempts need explicit accounting'
 rows.append(dict(fixture=row['fixture'],initialHeaderRequests=len(headers),prefixRequests=len(ranges),prefixBytes=sum(b-a for a,b in ranges),
  alternatives=[merge(ranges,target,ratio,cap) for ratio in [2,4] for cap in [1024**2,4*1024**2] for target in [4,8,16,32]],sourceObservation=row.get('sourceObservation')))
result=dict(diagnosticOnly=True,scope='Byte/range geometry only; no speed prediction or production guarantee',rows=rows)
a.output.write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
print(json.dumps(result,indent=2))
