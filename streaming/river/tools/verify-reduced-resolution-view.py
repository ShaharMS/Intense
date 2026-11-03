"""Correctness coverage for reduced-resolution technical codestreams; no timing claims."""
import argparse,hashlib,importlib.util,io,json,socket,threading,uuid
from pathlib import Path
from http.server import ThreadingHTTPServer
from urllib.parse import urlencode
import numpy as np
from PIL import Image,ImageFile
p=argparse.ArgumentParser();p.add_argument('--api',required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();assert not a.output.exists()
spec=importlib.util.spec_from_file_location('h',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
folder=a.output.parent/'reduced-view-fixtures';folder.mkdir(exist_ok=True);rng=np.random.default_rng(268);rgb=rng.integers(0,256,(517,769,3),dtype=np.uint8);gray=rng.integers(0,65536,(517,769),dtype=np.uint16);large_rgb=rng.integers(0,256,(2049,4097,3),dtype=np.uint8);large_gray=rng.integers(0,65536,(2049,4097),dtype=np.uint16);files={};meta=[];gold={}
for order,array,label in [(o,rgb,o.lower()) for o in ['LRCP','RLCP','RPCL','PCRL','CPRL']]+[('RPCL',gray,'gray16'),('RPCL',large_rgb,'parallel-rgb8'),('RPCL',large_gray,'parallel-gray16')]:
 for plt in [False,True]:
  name=label+('-plt' if plt else '-inline');file=folder/(name+'.jp2');buffer=io.BytesIO();Image.fromarray(array).save(buffer,format='JPEG2000',irreversible=False,num_resolutions=5,tile_size=(256,256),precinct_size=(64,64),codeblock_size=(16,16),progression=order,quality_layers=[1],plt=plt,mct=int(array.ndim==3))
  if file.exists():assert file.read_bytes()==buffer.getvalue(),'Preserve immutable qualification inputs'
  else:file.write_bytes(buffer.getvalue())
  files[name]=file;meta.append(dict(name=name,sha256=hashlib.sha256(file.read_bytes()).hexdigest(),expectProjected=order in ['LRCP','RLCP','RPCL']))
  for reduction in [1,2,3]:
   im=Image.open(file);scale=1<<reduction;im._size=tuple((n+scale-1)//scale for n in im.size);t=im.tile[0];args=list(t.args);args[1]=reduction;im.tile=[ImageFile._Tile(t.codec_name,(0,0,*im.size),t.offset,tuple(args))];ImageFile.ImageFile.load(im);original=np.asarray(im)
   if original.ndim==2:original=np.repeat(((original.astype(np.uint32)*255+32767)//65535).astype(np.uint8)[:,:,None],3,axis=2)
   gold[name,reduction]=original
counters={};lock=threading.Lock();base=h.make_origin(files,meta,dict(cases=meta),counters,lock)
class Handler(base):
 def setup(self):super().setup();self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
class Origin(ThreadingHTTPServer):request_queue_size=256
server=Origin(('0.0.0.0',18997),Handler);threading.Thread(target=server.serve_forever,daemon=True).start();sid='reduced-'+uuid.uuid4().hex;endpoint='http://host.docker.internal:18997/'+sid;report={'passed':False,'fixtures':meta,'rows':[]}
try:
 h.http(a.api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST')
 for driver in ['openjpeg','jp2grok','openhtj2k']:
  for fixture in meta:
   name=fixture['name']
   for reduction in [1,2,3]:
    url=a.api+f'/streamer/v1/{6-reduction}/0/0/tile.png?'+urlencode(dict(image=endpoint+'/fixture/'+name+'.jp2',driver=driver,edge='crop',tileSize=4096,maxZoom=6,minZoom=0,tilePixelRatio=1))
    body,headers=h.http(url,include_headers=True);actual=np.asarray(Image.open(io.BytesIO(body)).convert('RGB'));reference=gold[name,reduction]
    assert actual.shape==reference.shape and actual.tobytes()==reference.tobytes(),(driver,name,reduction,actual.shape,reference.shape)
    route=headers.get('x-river-decoder-route');assert ('reduced-resolution-view' in str(route))==fixture['expectProjected'],(driver,name,reduction,route)
    if driver=='openjpeg' and name.startswith('parallel-'):assert 'openjpeg-parallel-reduced' in str(route),(driver,name,reduction,route)
    before=dict(counters[sid]);assert h.http(url)==body and counters[sid]==before
    report['rows'].append(dict(driver=driver,source=name,reduction=reduction,route=route,pixelsExact=True,repeatIoZero=True))
 report['passed']=True;print(json.dumps(dict(passed=True,cases=len(report['rows']))))
except Exception as error:report['failure']=str(error);raise
finally:
 h.http(a.api+'/api/s3/sources/'+sid,method='DELETE');server.shutdown();a.output.write_text(json.dumps(report,indent=2)+'\n')
