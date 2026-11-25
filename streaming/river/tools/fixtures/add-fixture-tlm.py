"""Add chained TLM only to a new synthetic qualification derivative."""
import argparse,struct
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('source',type=Path);p.add_argument('output',type=Path)
a=p.parse_args();assert not a.output.exists();b=a.source.read_bytes();box=0
while b[box+4:box+8]!=b'jp2c':
 size=struct.unpack_from('>I',b,box)[0];assert size>=8;box+=size
size=struct.unpack_from('>I',b,box)[0];assert size==len(b)-box
at=box+10
while b[at:at+2]!=b'\xff\x90':
 marker,n=struct.unpack_from('>HH',b,at);assert marker!=0xff55;at+=n+2
first=at;entries=[]
while b[at:at+2]==b'\xff\x90':
 tile=struct.unpack_from('>H',b,at+4)[0];length=struct.unpack_from('>I',b,at+6)[0]
 assert length>=14;entries.append((tile,length));at+=length
assert b[at:]==b'\xff\xd9'
tlm=bytearray()
for z,start in enumerate(range(0,len(entries),10921)):
 assert z<256;part=entries[start:start+10921]
 tlm+=struct.pack('>HHBB',0xff55,4+6*len(part),z,0x60)
 tlm+=b''.join(struct.pack('>HI',tile,length) for tile,length in part)
prefix=bytearray(b[:first]);struct.pack_into('>I',prefix,box,size+len(tlm))
a.output.write_bytes(prefix+tlm+b[first:]);print(f'{len(entries)} parts, {len(tlm)} TLM bytes')
