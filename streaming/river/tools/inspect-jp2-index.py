import json,mmap,struct,sys
def inspect(path):
 with open(path,'rb') as f,mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as b:
  start=0
  if b[:2]!=b'\xffO':
   while start<len(b):
    size,kind=struct.unpack_from('>I4s',b,start);header=8
    if size==1:size=struct.unpack_from('>Q',b,start+8)[0];header=16
    if kind==b'jp2c':start+=header;break
    start+=size or len(b)-start
  offset=start+2;main={};tile_markers={};parts=0;tiles=set();progression=None
  while offset+2<len(b):
   marker=struct.unpack_from('>H',b,offset)[0]
   if marker==0xffd9:break
   if marker==0xff90:
    parts+=1;tile=struct.unpack_from('>H',b,offset+4)[0];tiles.add(tile);length=struct.unpack_from('>I',b,offset+6)[0];cursor=offset+12
    while cursor+2<len(b) and struct.unpack_from('>H',b,cursor)[0]!=0xff93:
     m=struct.unpack_from('>H',b,cursor)[0];tile_markers[hex(m)]=tile_markers.get(hex(m),0)+1
     cursor+=2+struct.unpack_from('>H',b,cursor+2)[0]
    if not length:break
    offset+=length;continue
   main[hex(marker)]=main.get(hex(marker),0)+1
   if marker==0xff52:progression=['LRCP','RLCP','RPCL','PCRL','CPRL'][b[offset+5]]
   offset+=2+struct.unpack_from('>H',b,offset+2)[0]
  return dict(path=path,bytes=len(b),progression=progression,mainMarkers=main,tileMarkers=tile_markers,tileParts=parts,tiles=len(tiles),hasTLM='0xff55' in main,hasPLT='0xff58' in tile_markers)
print(json.dumps([inspect(p) for p in sys.argv[1:]],indent=2))
