"""Diagnostic corpus matching organizational multipart/RPCL/layer structure.

Creates synthetic files only, using the existing Grok lab container. Does not
change deployment settings or claim equivalence to the organizational pixels.
"""
import argparse, hashlib, json, subprocess, uuid
from pathlib import Path
import numpy as np

p=argparse.ArgumentParser()
p.add_argument('--output',type=Path,required=True)
p.add_argument('--container',default='river-goal-openht-runtime')
a=p.parse_args()
a.output.mkdir(parents=True,exist_ok=False)
rng=np.random.default_rng(20261025)
pixels=rng.integers(0,256,(2048,2048,3),dtype=np.uint8)
ppm=a.output/'synthetic.ppm'
ppm.write_bytes(b'P6\n2048 2048\n255\n'+pixels.tobytes())
remote='/tmp/river-org-multipart-'+uuid.uuid4().hex
def run(*cmd):return subprocess.run(cmd,check=True,capture_output=True,text=True)
run('docker','exec',a.container,'mkdir',remote)
run('docker','cp',str(ppm),a.container+':'+remote+'/input.ppm')
rows=[]
for plt in [False,True]:
 name='rpcl-ten-layers-resolution-parts'+('-plt' if plt else '-inline')
 target=a.output/(name+'.jp2')
 cmd=['grk_compress','-i',remote+'/input.ppm','-o',remote+'/'+name+'.jp2',
      '-t','128,128','-n','7','-p','RPCL','-r','64,32,16,8,4,3,2,1.5,1.2,1',
      '-u','R','-X','-H','4','-Y','1']
 if plt:cmd+=['-L']
 run('docker','exec',a.container,*cmd)
 run('docker','cp',a.container+':'+remote+'/'+name+'.jp2',str(target))
 rows.append(dict(name=name,file=target.name,bytes=target.stat().st_size,
                  sha256=hashlib.sha256(target.read_bytes()).hexdigest(),command=cmd))
name='openjpeg-plt';target=a.output/(name+'.jp2')
cmd=['opj_compress','-i',remote+'/input.ppm','-o',remote+'/'+name+'.jp2',
     '-t','128,128','-n','7','-p','RPCL','-r','64,32,16,8,4,3,2,1.5,1.2,1',
     '-TP','R','-TLM','-PLT','-mct','1','-threads','4']
run('docker','exec',a.container,*cmd)
run('docker','cp',a.container+':'+remote+'/'+name+'.jp2',str(target))
rows.append(dict(name=name,file=target.name,bytes=target.stat().st_size,
                 sha256=hashlib.sha256(target.read_bytes()).hexdigest(),command=cmd))
name='openjpeg-precinct-inline';target=a.output/(name+'.jp2')
cmd=['opj_compress','-i',remote+'/input.ppm','-o',remote+'/'+name+'.jp2',
     '-t','128,128','-n','7','-p','RPCL','-r','64,32,16,8,4,3,2,1.5,1.2,1',
     '-TP','R','-TLM','-c','[64,64],[32,32],[16,16],[8,8],[4,4],[2,2],[1,1]',
     '-mct','1','-threads','4']
run('docker','exec',a.container,*cmd)
run('docker','cp',a.container+':'+remote+'/'+name+'.jp2',str(target))
rows.append(dict(name=name,file=target.name,bytes=target.stat().st_size,
                 sha256=hashlib.sha256(target.read_bytes()).hexdigest(),command=cmd))
(a.output/'manifest.json').write_text(json.dumps(dict(diagnosticOnly=True,
 source='deterministic synthetic RGB8; not organizational source bytes',fixtures=rows),indent=2)+'\n',encoding='utf-8')
print(json.dumps(rows,indent=2))
