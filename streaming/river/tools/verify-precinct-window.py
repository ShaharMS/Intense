"""Pixel qualification for translated precinct windows, not a performance suite."""
import argparse,hashlib,importlib.util,io,json,threading,uuid
from pathlib import Path
from http.server import ThreadingHTTPServer
from urllib.parse import urlencode
import numpy as np
from PIL import Image,ImageFile
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
spec=importlib.util.spec_from_file_location('h',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
out=a.output.parent/'window-fixtures-v2';out.mkdir(exist_ok=True);rng=np.random.default_rng(264);rgb=rng.integers(0,256,(517,769,3),dtype=np.uint8);gray=rng.integers(0,65536,(517,769),dtype=np.uint16);files={};gold={};meta=[]
for name,order,array in [(o.lower(),o,rgb) for o in ['LRCP','RLCP','RPCL','PCRL','CPRL']]+[('gray16','RPCL',gray)]:
 file=out/(name+'.jp2');buffer=io.BytesIO();Image.fromarray(array).save(buffer,format='JPEG2000',irreversible=False,num_resolutions=5,tile_size=(769,517),precinct_size=(64,64),codeblock_size=(16,16),progression=order,quality_layers=[1],plt=True,mct=int(array.ndim==3))
 if file.exists():assert file.read_bytes()==buffer.getvalue(), 'Preserve existing qualification fixture'
 else:file.write_bytes(buffer.getvalue())
 for reduction in [0,1,2]:
  im=Image.open(file);scale=1<<reduction
  if reduction:
   im._size=tuple((n+scale-1)//scale for n in im.size);t=im.tile[0];args=list(t.args);args[1]=reduction;im.tile=[ImageFile._Tile(t.codec_name,(0,0,*im.size),t.offset,tuple(args))];ImageFile.ImageFile.load(im)
  else:im.load()
  original=np.asarray(im)
  if original.ndim==2:original=np.repeat(((original.astype(np.uint32)*255+32767)//65535).astype(np.uint8)[:,:,None],3,axis=2)
  gold[name,reduction]=original
 files[name]=file;meta.append(dict(name=name,sha256=hashlib.sha256(file.read_bytes()).hexdigest()))
counters={};lock=threading.Lock();origin=h.make_origin(files,meta,dict(cases=meta),counters,lock);server=ThreadingHTTPServer(('0.0.0.0',18993),origin);threading.Thread(target=server.serve_forever,daemon=True).start();sid='window-'+uuid.uuid4().hex;endpoint=f'http://host.docker.internal:18993/{sid}';rows=[];report=dict(passed=False,scope='Forced window eligibility on small odd-sized fixtures, all five progression orders and grayscale16; independent complete-source pixels, repeats and multiple windows per source identity.',fixtures=meta,rows=rows)
try:
 h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
 for driver in ['openjpeg','jp2grok','openhtj2k']:
  for name in files:
   for reduction,positions in [(0,[(0,0),(1,1),(6,4),(11,7),(12,8)]),(1,[(0,0),(1,1),(2,1),(5,3),(6,4)]),(2,[(0,0),(1,0),(1,1),(2,1),(3,2)])]:
    for x,y in positions:
     url=a.api+f'/streamer/v1/{4-reduction}/{x}/{y}/tile.png?'+urlencode(dict(image=endpoint+'/fixture/'+name+'.jp2',driver=driver,edge='crop',tileSize=64,maxZoom=4,minZoom=0,tilePixelRatio=1))
     body,headers=h.http(url,include_headers=True);actual=np.asarray(Image.open(io.BytesIO(body)).convert('RGB'));expected=gold[name,reduction][y*64:min((y+1)*64,gold[name,reduction].shape[0]),x*64:min((x+1)*64,gold[name,reduction].shape[1])];assert actual.shape==expected.shape
     error=int(np.abs(actual.astype(np.int16)-expected.astype(np.int16)).max());assert error==0,(driver,name,x,y,error)
     repeat=h.http(url);assert repeat==body
     rows.append(dict(driver=driver,source=name,reduction=reduction,x=x,y=y,pixelError=error,route=headers.get('x-river-decoder-route')))
 assert all('precinct-window' in str(row['route']) for row in rows), 'Qualification must exercise translated windows, not fallback'
 report['passed']=True;print(json.dumps(dict(passed=True,cases=len(rows),routes=sorted({r['route'] for r in rows}))))
finally:
 h.http(a.api+'/api/s3/sources/'+sid,method='DELETE');server.shutdown();a.output.write_text(json.dumps(report,indent=2)+'\n')
