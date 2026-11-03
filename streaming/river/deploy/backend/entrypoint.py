#!/usr/bin/env python3
"""Run the API and its private GPU helper as one signal-aware container."""
import ctypes, os, signal, subprocess, sys, time, urllib.request
from river_profiles import apply_profile

try:
    print('River sizing profile: ' + apply_profile(os.environ), flush=True)
except ValueError as error:
    sys.exit(str(error))

children = []
stopping = False
def stop(signum, _frame):
    global stopping
    stopping = True
    for child in children:
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGTERM)

signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
mode = os.environ.get('RIVER_ENABLE_NVIDIA', 'auto').lower()
if mode not in ('auto', 'true', 'false'):
    sys.exit('RIVER_ENABLE_NVIDIA must be auto, true or false')
try:
    cuda = ctypes.CDLL('/usr/local/lib/libcudart.so.13')
    count = ctypes.c_int()
    gpu = cuda.cudaGetDeviceCount(ctypes.byref(count)) == 0 and count.value > 0
except OSError:
    gpu = False
if mode == 'true' and not gpu:
    sys.exit('NVIDIA requested but unavailable. Enable Docker GPU access and install a compatible NVIDIA driver.')
try:
    if gpu and mode != 'false':
        env = dict(os.environ, PORT='8081', RIVER_NVJPEG2000_BIND='127.0.0.1')
        child = subprocess.Popen([sys.executable, '/opt/river/nvidia/server.py'], env=env, start_new_session=True)
        children.append(child)
        deadline = time.monotonic() + 180
        while not stopping:
            if child.poll() is not None:
                raise RuntimeError('NVIDIA helper exited during startup')
            try:
                with urllib.request.urlopen('http://127.0.0.1:8081/health', timeout=1): break
            except OSError:
                if time.monotonic() >= deadline: raise RuntimeError('NVIDIA helper startup timed out')
                time.sleep(.2)
        os.environ['RIVER_NVJPEG2000_URL'] = 'http://127.0.0.1:8081'
    else:
        os.environ.pop('RIVER_NVJPEG2000_URL', None)
        os.environ['RIVER_NVJPEG2000_NATIVE_FINE'] = 'false'
        print('CPU mode: OpenJPEG and JP2Grok available; NVIDIA driver requires GPU access.', flush=True)
    if not stopping:
        children.append(subprocess.Popen(['/usr/local/bin/river-server'], start_new_session=True))
    while not stopping:
        ended = next((child for child in children if child.poll() is not None), None)
        if ended is not None:
            raise RuntimeError(f'backend component exited with status {ended.returncode}')
        time.sleep(.2)
except Exception as error:
    print(str(error), file=sys.stderr)
    sys.exit_code = 1
finally:
    stop(signal.SIGTERM, None)
    deadline = time.monotonic() + 10
    for child in children:
        try: child.wait(timeout=max(.1, deadline-time.monotonic()))
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
sys.exit(getattr(sys, 'exit_code', 0))
