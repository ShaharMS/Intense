"""Optimistic byte-cost bound for predicting SOT positions from earlier lengths.

Consumes independently parsed part metadata, not image bytes. Predictions never
become trusted offsets: this models only speculative cache windows that a real
reader would still validate through the ordinary source chain.
"""
import argparse,json,pathlib
p=argparse.ArgumentParser();p.add_argument('--parts',type=pathlib.Path,required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args()
parts=json.loads(a.parts.read_text());end=parts[-1]['offset']+parts[-1]['length'];rows=[]
for window in [4096,16384,65536,262144]:
    for depth in [1,2,4,8]:
        cached=[];saved=0;extra=0;issued=0;history=[]
        for part in parts[:-1]:
            offset=part['offset'];hit=any(lo<=offset and offset+12<=hi for lo,hi in cached)
            if hit:saved+=1
            if part['length']<16384:continue
            history.append(part['length'])
            if len(history)<2:continue
            guess=sum(history[-2:])//2;next_offset=offset+part['length']
            if not hit:
                for step in range(1,depth+1):
                    center=next_offset+step*guess;lo=max(next_offset,center-window//2);hi=min(end,center+window//2)
                    if hi>lo:cached.append((lo,hi));extra+=hi-lo;issued+=1
        rows.append(dict(windowBytes=window,depth=depth,serialReadsAvoidedUpperBound=saved,extraBytes=extra,extraGets=issued))
result=dict(scope='Offline upper-bound simulation. Predictor sees only earlier lengths; assumes free parallel speculative GETs and unlimited retention. Counts may include headers already covered by ordinary probes. No measured network speedup claimed.',decision='Reject this predictor: narrow windows add hundreds of requests for few potential hits; wider windows amplify bytes substantially and reduce per-instance capacity.',results=rows)
a.output.write_text(json.dumps(result,indent=2));print(json.dumps(result,indent=2))
