"""Replay validated offsets to bound history-guided short-part read-ahead.

Uses no guessed offset as an index entry. This models only Range geometry, not
decoder speed; all part starts/lengths come from the complete reconciled trace.
"""
import argparse,json
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('profile',type=Path);p.add_argument('--output',type=Path,required=True)
a=p.parse_args();assert not a.output.exists();profile=json.loads(a.profile.read_text());rows=[]
def replay(parts,threshold,window,minimum_run):
 cache={0,1024,2048};gets=0;count_bytes=0;run=0
 for part in parts:
  start=part['start'];needed=set(range(start//1024*1024,(start+12+1023)//1024*1024,1024))
  if not needed<=cache:
   end=(start+(window if run>=minimum_run else 12)+1023)//1024*1024
   cursor=start//1024*1024
   while cursor<end:
    if cursor in cache:cursor+=1024;continue
    stop=cursor+1024
    while stop<end and stop not in cache:stop+=1024
    cache.update(range(cursor,stop,1024));gets+=1;count_bytes+=stop-cursor;cursor=stop
  run=run+1 if part['endExclusive']-start<=threshold else 0
 return dict(gets=gets,bytes=count_bytes)
for row in profile['rows']:
 if row['case']!='mb979-deep':continue
 parts=sorted((e for e in row['events'] if e['kind']=='indexEntry'),key=lambda e:e['tilePart'])
 assert len(parts)==226 and all(parts[i]['endExclusive']==parts[i+1]['start'] for i in range(len(parts)-1))
 baseline=replay(parts,0,12,1);actual=row['groups']['sotDiscovery/selected_parts']
 assert baseline==dict(gets=actual['reads'],bytes=actual['bytes']),(baseline,actual)
 models=[]
 for threshold in [512,1024,2048,4096,8192]:
  for window in [2048,4096,8192,16384]:
   for minimum_run in [1,2,3]:models.append(dict(threshold=threshold,window=window,minimumRun=minimum_run,**replay(parts,threshold,window,minimum_run)))
 models.sort(key=lambda x:(x['gets'],x['bytes']))
 rows.append(dict(case=row['case'],baseline=baseline,models=models))
result=dict(diagnosticOnly=True,scope='Validated Psot-chain replay; metadata-only GET/byte estimates, not latency or release evidence.',rows=rows)
a.output.write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
print(json.dumps({r['case']:dict(baseline=r['baseline'],best=r['models'][:8]) for r in rows},indent=2))
