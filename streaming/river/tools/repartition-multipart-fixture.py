"""Synthetic-only adversarial fixture: split packets inside quality-layer runs.

Never use this to change served source files. Keeps entropy/packet order exact,
removes PLT, and rebuilds TLM/SOT extents for arbitrary packet boundaries.
"""
import argparse,struct
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('source',type=Path);p.add_argument('output',type=Path)
a=p.parse_args();assert not a.output.exists();b=a.source.read_bytes();box=0
while b[box+4:box+8]!=b'jp2c':box+=struct.unpack_from('>I',b,box)[0]
at=box+10;main=bytearray(b'\xff\x4f')
while b[at:at+2]!=b'\xff\x90':
 marker,size=struct.unpack_from('>HH',b,at)
 if marker!=0xff55:main+=b[at:at+size+2]
 at+=size+2
tiles={}
while b[at:at+2]==b'\xff\x90':
 tile=struct.unpack_from('>H',b,at+4)[0];size=struct.unpack_from('>I',b,at+6)[0]
 cur=at+12;lengths=[]
 while b[cur:cur+2]!=b'\xff\x93':
  marker,n=struct.unpack_from('>HH',b,cur);assert marker==0xff58,'Fixture must have PLT only'
  value=0
  for c in b[cur+5:cur+n+2]:
   value=(value<<7)|(c&127)
   if c<128:lengths.append(value);value=0
  assert value==0;cur+=n+2
 cur+=2;assert sum(lengths)==at+size-cur
 packets=tiles.setdefault(tile,[])
 for n in lengths:packets.append(b[cur:cur+n]);cur+=n
 at+=size
parts=[]
for tile,packets in sorted(tiles.items()):
 assert len(packets)==210
 boundaries=[0,7,21,53,84,123,167,210]
 for ordinal,(lo,hi) in enumerate(zip(boundaries,boundaries[1:])):
  entropy=b''.join(packets[lo:hi]);part=struct.pack('>HHHIBB',0xff90,10,tile,len(entropy)+14,ordinal,7)+b'\xff\x93'+entropy
  parts.append((tile,part))
tlm=struct.pack('>HHBB',0xff55,4+len(parts)*6,0,0x60)+b''.join(struct.pack('>HI',tile,len(part)) for tile,part in parts)
stream=bytes(main)+tlm+b''.join(part for _,part in parts)+b'\xff\xd9'
a.output.write_bytes(b[:box]+struct.pack('>I4s',len(stream)+8,b'jp2c')+stream)
print(a.output)
