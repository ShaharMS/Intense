"""Bound discovery GET savings using validated SOTs; not a runtime benchmark."""
import argparse,hashlib,json,pathlib,struct
p=argparse.ArgumentParser();p.add_argument('--source',type=pathlib.Path,required=True);p.add_argument('--parts',type=pathlib.Path,required=True);p.add_argument('--seeds',type=pathlib.Path,required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();assert not a.output.exists()
parts=json.loads(a.parts.read_bytes());proof=json.loads(a.seeds.read_bytes());assert len(parts)==proof['tileParts']
with a.source.open('rb') as f:
 digest=hashlib.file_digest(f,'sha256').hexdigest();assert digest==proof['sourceFileSha256']
 for part in parts:
  f.seek(part['offset']);b=f.read(12);assert b[:4]==b'\xff\x90\x00\x0a';tile,length,ordinal,total=struct.unpack('>HIBB',b[4:]);assert (tile,length,ordinal,total)==(part['tile'],part['length'],part['ordinal'],part['total'])
seeds=sorted(proof['seedHeaderIndices']);rows=[]
for width in [8192,16384,32768,65536,131072,262144,524288,1048576]:
 gets=bytes_read=0;longest=0
 for i,seed in enumerate(seeds):
  stop=seeds[i+1] if i+1<len(seeds) else len(parts);covered=0;lane_gets=0
  for part in parts[seed+1:stop]:
   at=part['offset']
   if at+12<=covered:continue
   lo=at//1024*1024;covered=min(a.source.stat().st_size,((at+width+1023)//1024)*1024);bytes_read+=covered-lo;gets+=1;lane_gets+=1
  longest=max(longest,lane_gets)
 rows.append(dict(windowBytes=width,discoveryGets=gets,discoveryBytes=bytes_read,longestDependentChain=longest,dependencyAt20ms=longest*20))
gaps=sorted((b['offset']-(x['offset']+12) for x,b in zip(parts,parts[1:])),reverse=True)
span=parts[-1]['offset']+12-parts[0]['offset']
pareto=[dict(gets=k,minimumBytesToCoverEverySot=span-sum(gaps[:k-1])) for k in [1,2,5,10,20,50,100,200,350,451]]
report=dict(oracleRangeCover=pareto,oracleScope='Mathematical optimum for covering all451 twelve-byte SOT headers usingK contiguous HTTP ranges with all offsets known in advance. Excludes payload/JP2 headers. Not a universal JPEG2000 lower bound: applies to this traversal requirement and immutable fixture.',scope='Optimistic offline model, not measured speed. Independently validates every SOT and full source hash. Existing speculative seeds assumed free and all retained; excludes their initial16MiB, payload reads, cache eviction, concurrency contention, and transfer time. Wider windows cannot prove universal behavior.',sourceSha256=digest,parts=len(parts),rows=rows)
a.output.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(rows))
