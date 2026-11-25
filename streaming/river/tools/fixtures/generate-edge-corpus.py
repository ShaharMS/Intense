"""Small deterministic grayscale, alpha, multispectral and HT correctness fixtures.
Uses the existing River container's encoder; never creates another container.
"""
import argparse,json,subprocess,array,sys
from pathlib import Path
from PIL import Image
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--engine',default='podman');p.add_argument('--container',default='river-stable');a=p.parse_args()
work=ROOT/'target/fixtures/edge-inputs';work.mkdir(parents=True,exist_ok=True)
rows=[];n=512
for kind,components,bits in [('gray',1,16),('alpha',4,8),('six-band',6,16),('eight-band',8,16),('ht-rgb',3,8)]:
 planes=[[((x*257+y*131+c*97)%65536 if bits==16 else (x*3+y*7+c*53)%256) for y in range(n) for x in range(n)] for c in range(components)]
 if bits==16:
  pixels=array.array('H',[v for plane in planes for v in plane]);raw=pixels.tobytes();suffix='rawl' if sys.byteorder=='little' else 'raw'
 else:raw=bytes(v for plane in planes for v in plane);suffix='raw'
 src=work/(kind+'.'+suffix);src.write_bytes(raw)
 subprocess.run([a.engine,'cp',str(src),a.container+':/tmp/river-fixture.'+suffix],check=True)
 name='synthetic-edge-'+kind+('.jph' if kind=='ht-rgb' else '.jp2')
 args=[a.engine,'exec',a.container,'grk_compress','-i','/tmp/river-fixture.'+suffix,'-F',f'{n},{n},{components},{bits},u','-o','/tmp/'+name,'-n','6','-t','256,256','-p','RPCL','-X','-L','-Y','0','-H','2']
 if kind=='ht-rgb':args+=['-M','64']
 if kind=='alpha':
  im=Image.frombytes('RGBA',(n,n),bytes(v for i in range(n*n) for v in [planes[c][i] for c in range(4)]));src=work/'alpha.png';im.save(src)
  subprocess.run([a.engine,'cp',str(src),a.container+':/tmp/river-alpha.png'],check=True)
  args=[a.engine,'exec',a.container,'grk_compress','-i','/tmp/river-alpha.png','-o','/tmp/'+name,'-n','6','-t','256,256','-X','-L','-H','2']
 subprocess.run(args,check=True,capture_output=True)
 subprocess.run([a.engine,'cp',a.container+':/tmp/'+name,str(ROOT/'corpus'/name)],check=True)
 if components>=3 and kind!='alpha':
  # Raw encoder inputs are tagged sYCC by Grok. These authored planes are RGB;
  # set the enumerated JP2 colour specification accordingly, preserving lengths.
  dst=ROOT/'corpus'/name;data=bytearray(dst.read_bytes());at=data.index(b'colr')
  assert data[at+4]==1
  data[at+7:at+11]=(16).to_bytes(4,'big');dst.write_bytes(data)
 # Reference samples are authored independently of the JPEG 2000 decoder.
 mode='RGBA' if kind=='alpha' else 'RGB'
 rgb=bytes(v for i in range(n*n) for v in ([planes[c][i] for c in range(4)] if kind=='alpha' else [((planes[c if components>1 else 0][i]*255+((1<<bits)-1)//2)//((1<<bits)-1)) for c in range(3)]))
 ref='synthetic-edge-'+kind+'-reference.png';Image.frombytes(mode,(n,n),rgb).save(ROOT/'corpus'/ref)
 rows.append(dict(file=name,source=ref,components=components,bits=bits,kind=kind,encoder='Grok 20.4.15 from original m1227 image'))
(ROOT/'target/fixtures/edge-corpus.json').write_text(json.dumps({'fixtures':rows},indent=2)+'\n')
print(json.dumps(rows,indent=2))
