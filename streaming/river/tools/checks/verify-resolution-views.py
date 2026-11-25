import importlib.util,json,time,sys
from pathlib import Path
import numpy as np
from PIL import Image
sys.path.insert(0,'/tmp')
import resolution_view
spec=importlib.util.spec_from_file_location('native_experiment','/tmp/native-experiment.py');native=importlib.util.module_from_spec(spec);spec.loader.exec_module(native)
def reference(path):
 data=path.read_bytes();tokens=[];pos=0
 while len(tokens)<4:
  while data[pos] in b' \t\r\n':pos+=1
  if data[pos]==35:pos=data.index(b'\n',pos)+1;continue
  end=pos
  while data[end] not in b' \t\r\n':end+=1
  tokens.append(data[pos:end]);pos=end
 if tokens[0]!=b'P6':raise ValueError('expected RGB P6 reference')
 w,h,maximum=map(int,tokens[1:]);pos+=1
 raw=np.frombuffer(data[pos:],dtype='>u2' if maximum>255 else np.uint8).reshape(h,w,3).astype(np.uint32)
 rgb=((raw*255+maximum//2)//maximum).astype(np.uint8)
 return np.concatenate([rgb,np.full((h,w,1),255,dtype=np.uint8)],axis=-1)
samples=[];decoder=native.Decoder()
catalog=json.loads(Path('/app/sentinel-date-mosaics/river-assets.json').read_text())
for entry in catalog['assets']:
 source=Path('/app/sentinel-date-mosaics')/entry['source']
 ref=reference(Path('/var/lib/river/s3-cache/local-sources/cold-study-refs/'+source.stem+'.pam'))
 started=time.perf_counter();images,timing=decoder.decode(source,40980,10980,[(0,0,40980,10980)],6,1000000);elapsed=(time.perf_counter()-started)*1000
 delta=np.abs(images[0].astype(np.int16)-ref.astype(np.int16))
 samples.append(dict(source=source.name,e2eMs=elapsed,**timing,equalPixels=bool(np.array_equal(images[0],ref)),maxError=int(delta.max())))
decoder.close()
print(json.dumps(dict(samples=samples),indent=2))
if not all(s['equalPixels'] and s.get('resolutionView') for s in samples):raise SystemExit(1)
