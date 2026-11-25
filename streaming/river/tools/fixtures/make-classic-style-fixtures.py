"""Synthetic, immutable Part1 coding-style corpus. Uses the task-owned encoder lab."""
import argparse,hashlib,json,subprocess,uuid
from pathlib import Path
import numpy as np
p=argparse.ArgumentParser();p.add_argument('--output',type=Path,required=True);p.add_argument('--container',default='river-goal-openht-runtime');a=p.parse_args()
a.output.mkdir(parents=True,exist_ok=False);remote='/tmp/river-styles-'+uuid.uuid4().hex
def run(*cmd):subprocess.run(cmd,check=True,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
run('docker','exec',a.container,'mkdir',remote)
rng=np.random.default_rng(20261026);rgb=rng.integers(0,256,(517,769,3),dtype=np.uint8)
ppm=a.output/'source.ppm';ppm.write_bytes(b'P6\n769 517\n255\n'+rgb.tobytes());run('docker','cp',str(ppm),a.container+':'+remote+'/source.ppm')
fixtures=[]
for style in range(64):
 name=f'style-{style:02d}';out=a.output/(name+'.jp2')
 cmd=['opj_compress','-i',remote+'/source.ppm','-o',remote+'/'+name+'.jp2','-t','256,256','-n','5','-p','RPCL','-r','64,32,16,8,4,3,2,1.5,1.2,1','-TP','R','-TLM','-M',str(style),'-mct','1','-threads','4']
 run('docker','exec',a.container,*cmd);run('docker','cp',a.container+':'+remote+'/'+name+'.jp2',str(out))
 fixtures.append(dict(name=name,file=out.name,bytes=out.stat().st_size,sha256=hashlib.sha256(out.read_bytes()).hexdigest(),style=style,width=769,height=517,layers=10,resolutions=5))
(a.output/'manifest.json').write_text(json.dumps(dict(diagnosticOnly=True,fixtures=fixtures),indent=2)+'\n',encoding='utf-8')
print(json.dumps(dict(fixtures=len(fixtures),output=str(a.output))))
