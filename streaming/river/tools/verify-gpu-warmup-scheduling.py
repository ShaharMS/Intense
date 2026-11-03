"""Exercise actual scheduling/bounds functions without requiring a GPU."""
import ast
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import tempfile
import threading
import time

source = Path(__file__).resolve().parents[1]/'nvjpeg2000-sidecar/server.py'
tree = ast.parse(source.read_text(encoding='utf-8'))
functions = [node for node in tree.body if isinstance(node,ast.FunctionDef)
    and node.name in ('packaged_warm_sources','schedule_packaged_warmup')]
release = threading.Event()
entered = threading.Event()
calls = []
local = threading.local()
class Decoder:
    def decode(self,*args):
        entered.set()
        assert release.wait(3)
        calls.append(args)
def initialize(): local.decoder = Decoder()
pool = ThreadPoolExecutor(max_workers=2,initializer=initialize)
namespace = dict(threading=threading,time=time,json=json,pool=pool,local=local,WORKERS=2,
    warmup=dict(status='disabled',elapsedMs=0),warmup_lock=threading.Lock())
exec(compile(ast.Module(body=functions,type_ignores=[]),str(source),'exec'),namespace)
try:
    start = time.monotonic()
    namespace['schedule_packaged_warmup']([Path('tiny-legacy.j2k'),Path('tiny-ht.jhc')])
    assert time.monotonic()-start < .2, 'scheduling blocked on decoder completion'
    assert entered.wait(1) and namespace['warmup']['status']=='running'
    serving = pool.submit(lambda:'serving')
    assert not serving.done(), 'persistent serving loop bypassed queued warmup'
    release.set()
    assert serving.result(timeout=3)=='serving'
    deadline = time.monotonic()+3
    while namespace['warmup']['status']=='running' and time.monotonic()<deadline: time.sleep(.01)
    assert namespace['warmup']['status']=='ready' and namespace['warmup']['workersReady']==2
    assert len(calls)==4 and all(args[1:]==(64,64,[(0,0,64,64)],2,4096) for args in calls)
    with tempfile.TemporaryDirectory(prefix='river-warmup-bounds-') as work:
        root = Path(work); directory=root/'river-warmup';directory.mkdir()
        image=directory/'tiny.j2k';image.write_bytes(b'probe')
        manifest=directory/'sources.json'
        manifest.write_text(json.dumps([dict(source=image.name,width=64,height=64,reduction=2)]))
        namespace.update(ALLOWED_ROOTS=[root],allowed_source=lambda path:Path(path).resolve(strict=True))
        assert namespace['packaged_warm_sources']()==[image]
        image.write_bytes(b'x'*65537)
        try: namespace['packaged_warm_sources']()
        except ValueError: pass
        else: raise AssertionError('oversized warm source admitted')
    print('PASS: non-blocking warmup scheduling, all-worker priming, fixed decode geometry and file bounds')
finally:
    release.set();pool.shutdown(wait=True)
