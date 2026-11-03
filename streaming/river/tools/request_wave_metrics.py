"""Observed request intervals, not an inferred causal dependency graph."""
import math

def audit_requests(ranges):
    reads=[r for r in ranges if r['body']]
    spans=sorted((r['start'],r['end']) for r in reads)
    assert all(math.isfinite(a) and math.isfinite(b) and b>=a for a,b in spans)
    if not spans:return dict(requests=0,spanMs=0,ioBusyMs=0,idleGapMs=0,maximumNonOverlappingRequests=0,peak=0,timeByInFlightMs={})
    merged=[]
    for a,b in spans:
        if merged and a<=merged[-1][1]:merged[-1][1]=max(b,merged[-1][1])
        else:merged.append([a,b])
    selected=[];last=-math.inf
    for a,b in sorted(spans,key=lambda s:s[1]):
        if a>=last:selected.append((a,b));last=b
    events=sorted([(a,1) for a,b in spans]+[(b,-1) for a,b in spans])
    active=peak=0;previous=events[0][0];hist={}
    for at,delta in events:
        hist[str(active)]=hist.get(str(active),0)+(at-previous)*1000
        active+=delta;peak=max(peak,active);previous=at
    span=(max(b for a,b in spans)-spans[0][0])*1000;busy=sum(b-a for a,b in merged)*1000
    assert abs(sum(hist.values())-span)<.001
    assert abs(sum(int(k)*v for k,v in hist.items())-sum(b-a for a,b in spans)*1000)<.001
    return dict(requests=len(reads),spanMs=span,ioBusyMs=busy,idleGapMs=span-busy,maximumNonOverlappingRequests=len(selected),peak=peak,timeByInFlightMs=hist,scope='Observed temporal serialization, including admission rounds; not proof of causal dependencies or an equivalent latency saving.')
