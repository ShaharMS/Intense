"""Run alternating, matched progressive measurements for both release images."""
import os,subprocess,time,urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];env=dict(os.environ,PYTHONPATH=str(ROOT/'target/tools-venv/lib64/python3.15/site-packages'))
for overview in [False,True]:
 for i,suffix in enumerate(['','-r2','-r3']):
  for version,port in ([('m1227',18081),('current',18080)] if i%2==0 else [('current',18080),('m1227',18081)]):
   subprocess.run(['podman','restart','river-stable' if version=='m1227' else 'replica-1'],check=True,stdout=subprocess.DEVNULL)
   for _ in range(100):
    try:
     urllib.request.urlopen(f'http://127.0.0.1:{port}/health',timeout=2).close();break
    except OSError:time.sleep(.1)
   label=version+suffix;extra=['--driver','auto','--overview'] if overview else []
   with (ROOT/'target/benchmarks'/('progressive-'+label+('-auto-overview' if overview else '')+'.log')).open('w') as log:
    subprocess.run(['python3',str(ROOT/'tools/benchmarks/check-progressive.py'),'--port',str(port),'--label',label,*extra],env=env,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
