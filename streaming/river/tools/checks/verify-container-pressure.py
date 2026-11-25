"""Controlled cgroup pressure and recovery, without exceeding the container limit."""
import json,subprocess,time,urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def snapshot():
 with urllib.request.urlopen('http://127.0.0.1:18080/api/diagnostics/resources',timeout=10) as r:return json.load(r)
initial=snapshot();limit=initial['plan']['memory_allowance_bytes'];used=initial['effectivePressureWorkingSetBytes'];allocate=max(0,int(limit*.89)-used-16*1024*1024);assert 0<allocate<limit*.9
rows=[dict(phase='initial',data=initial)];process=subprocess.Popen(['podman','exec','replica-1','python3','-c',f'import time; data=bytearray({allocate}); print("allocated",flush=True); time.sleep(12)'],stdout=subprocess.PIPE,text=True)
try:
 assert process.stdout.readline().strip()=='allocated'
 for _ in range(10):rows.append(dict(phase='pressure',data=snapshot()));time.sleep(1)
 process.wait(timeout=10)
 lowest=min(r['data']['pressure']['scale_per_mille'] for r in rows);assert lowest<initial['pressure']['scale_per_mille'],rows[-1]
 for _ in range(35):
  d=snapshot();rows.append(dict(phase='recovery',data=d))
  if d['pressure']['scale_per_mille']>lowest:break
  time.sleep(1)
 assert rows[-1]['data']['pressure']['scale_per_mille']>lowest
 (ROOT/'target/tests/container-pressure.json').write_text(json.dumps(dict(passed=True,allocatedBytes=allocate,lowestScale=lowest,rows=rows),indent=2)+'\n')
 print(json.dumps(dict(passed=True,lowestScale=lowest,recoveredScale=rows[-1]['data']['pressure']['scale_per_mille'])))
finally:
 if process.poll() is None:process.terminate();process.wait(timeout=15)
