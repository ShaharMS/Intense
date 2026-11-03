"""Offline independent-SOT hypothesis screen; never used by serving code."""
import argparse,hashlib,io,json,pathlib,struct
from PIL import Image
p=argparse.ArgumentParser();p.add_argument('--source',type=pathlib.Path,required=True);p.add_argument('--parts',type=pathlib.Path,required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();assert not a.output.exists()
parts=json.loads(a.parts.read_bytes());known={0x51,0x52,0x53,0x55,0x57,0x58,0x5c,0x5d,0x5e,0x5f,0x60,0x61,0x63,0x64,0x90,0x91};tile={0x52,0x53,0x58,0x5c,0x5d,0x5e,0x5f,0x61,0x64,0x90,0x91};modes={'allLengthMarkers':set(range(0x40,0xff))-{0x4f,0x92,0x93,0xd9},'knownMarkers':known,'basicTileMarkers':tile}
def blockers(data,target,allowed):
 found=[];at=data.find(b'\xff')
 while at>=0 and at<target:
  if at+4<=len(data) and data[at+1] in allowed:
   length=int.from_bytes(data[at+2:at+4],'big')
   if length>=2 and at+2+length>target:found.append(dict(relativeStart=at-target,marker=hex(0xff00|data[at+1]),length=length))
  at=data.find(b'\xff',at+1)
 return found
stats={m:dict(accepted=[],blocked=0,examples=[]) for m in modes}
with a.source.open('rb') as f:
 digest=hashlib.file_digest(f,'sha256').hexdigest();assert digest=='f1c4ca5c01705779ca596fbc9c44528c0264ee5cff790ce78d5d9267dc3ba8fc'
 for part in parts:
  offset=part['offset'];lo=max(parts[0]['offset'],offset-65537);f.seek(lo);data=f.read(offset-lo+12);assert data[offset-lo:offset-lo+4]==b'\xff\x90\x00\x0a'
  for name,allowed in modes.items():
   suspects=blockers(data,offset-lo,allowed);row=stats[name]
   if not suspects:row['accepted'].append(part['tile'])
   else:
    row['blocked']+=1
    if len(row['examples'])<3:row['examples'].append(dict(tile=part['tile'],suspects=suspects[:3]))
# Make a conforming two-tile image and put a fake tile1 inside tile0's COM.
original=Image.new('RGB',(128,64),(17,40,90));original.paste((90,20,10),(64,0,128,64));stream=io.BytesIO();original.save(stream,format='JPEG2000',tile_size=(64,64),num_resolutions=4,irreversible=False);raw=stream.getvalue();box=0
while raw[box+4:box+8]!=b'jp2c':box+=int.from_bytes(raw[box:box+4],'big')
cs=box+8;at=cs+2
while raw[at:at+2]!=b'\xff\x90':at+=2+int.from_bytes(raw[at+2:at+4],'big')
first=at;length=int.from_bytes(raw[first+6:first+10],'big');fake=b'\xff\x90\x00\x0a'+struct.pack('>HI',1,26)+b'\x00\x01\xff\x93'+bytes(12);comment=b'\xff\x64'+struct.pack('>HH',4+len(fake),0)+fake;insert=first+12;modified=bytearray(raw[:insert]+comment+raw[insert:]);struct.pack_into('>I',modified,box,int.from_bytes(raw[box:box+4],'big')+len(comment));struct.pack_into('>I',modified,first+6,length+len(comment))
decoded=Image.open(io.BytesIO(modified)).convert('RGB');assert decoded.size==original.size and decoded.tobytes()==original.tobytes();fake_at=insert+6
adversarial={name:bool(blockers(modified,max(0,fake_at),allowed)) for name,allowed in modes.items()};assert all(adversarial.values())
# Demonstrate that syntactically valid fake empty packets can decode to wrong pixels.
main=bytearray(raw[cs:first]);struct.pack_into('>I',main,8,64);rebased=bytearray(fake);rebased[4:6]=b'\0\0';view=Image.open(io.BytesIO(main+rebased+b'\xff\xd9')).convert('RGB');assert view.size==(64,64) and view.tobytes()!=original.crop((64,0,128,64)).tobytes()
a.output.parent.mkdir(parents=True,exist_ok=True);fixture=a.output.with_suffix('.embedded-com.jp2');fixture.write_bytes(modified)
report=dict(status='diagnostic-only; no serving changes',sourceSha256=digest,tileParts=len(parts),guardLookbehindBytes=65537,modes=stats,adversarial=dict(fixture=str(fixture),validOriginalPixelsPreserved=True,fakeTile=1,fakeSotInsideComment=True,unanchoredFakeViewDecodes=True,fakeViewMatchesOriginalTile=False,guardsRejectFake=adversarial),scope='Known offsets are an oracle for evaluating guard acceptance only, not available to a cold reader. No latency or GET-count speedup claim. Known/basic-marker variants do not establish safety for unknown extensions. OpenJPEG MQ/RAW code motivates marker-emulation investigation; this is not a proof across all JPEG2000 coding modes.',primarySources=['https://github.com/uclouvain/openjpeg/blob/master/src/lib/openjp2/mqc_inl.h','https://github.com/uclouvain/openjpeg/blob/master/src/lib/openjp2/j2k.c'])
a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8');print(json.dumps({name:dict(accepted=len(r['accepted']),blocked=r['blocked']) for name,r in stats.items()}));print(json.dumps(report['adversarial']))
