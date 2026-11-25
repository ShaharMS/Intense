"""Check encoded markers independently of the generator's requested options."""
import json,struct
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
manifest=json.loads((ROOT/'target/fixtures/corpus.json').read_text())
rows=[]
for fixture in manifest['fixtures']:
 data=(ROOT/'corpus'/fixture['file']).read_bytes();box=0
 while data[box+4:box+8]!=b'jp2c':
  size=struct.unpack_from('>I',data,box)[0];assert size>=8;box+=size
 at=box+10;tlm=False;cod=None
 while data[at:at+2]!=b'\xff\x90':
  marker,length=struct.unpack_from('>HH',data,at)
  if marker==0xff55:tlm=True
  if marker==0xff52:cod=data[at+4:at+2+length]
  at+=length+2
 assert cod is not None
 assert cod[1]=={'LRCP':0,'RLCP':1,'RPCL':2}[fixture['progression']]
 assert struct.unpack_from('>H',cod,2)[0]==len(fixture['qualityLayers'])
 assert cod[5]==fixture['waveletLevels'] and cod[9]==1
 parts=0;plt=False
 while data[at:at+2]==b'\xff\x90':
  size=struct.unpack_from('>I',data,at+6)[0];assert size>=14
  h=at+12
  while data[h:h+2]!=b'\xff\x93':
   marker,length=struct.unpack_from('>HH',data,h);plt|=marker==0xff58;h+=length+2;assert h<at+size
  at+=size;parts+=1
 assert data[at:]==b'\xff\xd9'
 assert tlm==fixture['tlm'] and plt==fixture['plt'],fixture['file']
 rows.append(dict(file=fixture['file'],tlm=tlm,plt=plt,tileParts=parts,waveletLevels=cod[5],reversible=True))
(ROOT/'target/fixtures/structure-verification.json').write_text(json.dumps(dict(passed=True,rows=rows),indent=2)+'\n')
print('Verified TLM, PLT, progression, layer count, wavelet levels, reversibility and tile-part bounds for',len(rows),'images')
