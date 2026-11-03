#!/usr/bin/env python3
"""Small private nvImageCodec endpoint used by River's selectable CUDA driver."""

import atexit
from concurrent.futures import ThreadPoolExecutor
import json
import os
import socket
import struct
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from functools import lru_cache

from native import Decoder
from rpc_codec import encode as encode_raster

ALLOWED_ROOTS = tuple(Path(p).resolve() for p in os.environ.get(
    "RIVER_NVJPEG2000_ALLOWED_ROOTS", "/app/corpus:/app/sentinel-date-mosaics"
).split(":"))
MAX_ROI_PIXELS = int(os.environ.get("RIVER_NVJPEG2000_MAX_ROI_PIXELS", "100000000"))
WORKERS = max(1, min(32, int(os.environ.get("RIVER_NVJPEG2000_WORKERS", "8"))))
local = threading.local()
contexts = []
warmup = {"status": "disabled", "elapsedMs": 0.0}
warmup_lock = threading.Lock()
pending=[]
collecting=set()
admission=threading.Condition()
stopping=False
closed=False
BATCH_REQUESTS=max(1,min(16,int(os.environ.get('RIVER_NVJPEG2000_BATCH_REQUESTS','8'))))
BATCH_WAIT=max(0,min(0.005,float(os.environ.get('RIVER_NVJPEG2000_BATCH_WAIT_MS','1'))/1000))

@lru_cache(maxsize=64)
def native_grid(path,size,mtime,width,height):
    # Read only SIZ; unsupported/ambiguous headers simply disable grouping.
    with path.open('rb') as source:data=source.read(65536)
    start=data.find(b'\xff\x51')
    if start<0 or start+40>len(data):return None
    _,w,h,ox,oy,tw,th,tox,toy,_=struct.unpack_from('>H8IH',data,start+4)
    if (w,h)!=(width,height) or ox or oy or tox or toy or not tw or not th:return None
    return tw,th

def batch_worker():
    while True:
        with admission:
            while True:
                if stopping:return
                first=next((j for j in pending if j['key'] not in collecting),None)
                if first is not None:break
                admission.wait()
            pending.remove(first);batch=[first]
            # Bounded microbatch delay; oldest request always wins admission.
            # Fine ROIs group only when they touch identical native tiles.
            # Arbitrary scattered ROI batching regressed throughput.
            limit=BATCH_REQUESTS if first['key'][3]>=3 or first['key'][4] is not None else 1
            collecting.add(first['key'])
            deadline=first['queued']+(BATCH_WAIT if limit>1 else 0)
            while len(batch)<limit:
                remaining=deadline-time.perf_counter()
                candidates=[j for j in pending if j['key']==first['key']]
                for job in candidates:
                    if len(batch)>=limit:break
                    if sum(j['pixels'] for j in batch)+job['pixels']>min(MAX_ROI_PIXELS,8388608):continue
                    pending.remove(job);batch.append(job)
                if len(batch)>=limit or remaining<=0:break
                admission.wait(remaining)
            collecting.remove(first['key']);admission.notify_all()
        started=time.perf_counter()
        try:
            bounds=[b for j in batch for b in j['bounds']]
            source,width,height,reduction=first['key'][:4]
            decoded,timing=local.decoder.decode(source,width,height,bounds,reduction,MAX_ROI_PIXELS)
            elapsed=(time.perf_counter()-started)*1000
            timing={**timing,'batchRequests':len(batch),'batchOutputs':len(bounds)}
            offset=0
            for job in batch:
                count=len(job['bounds'])
                job['result']=(decoded[offset:offset+count],timing,(started-job['queued'])*1000,elapsed)
                offset+=count
        except Exception as error:
            for job in batch:job['error']=error
        finally:
            for job in batch:
                job['completed']=time.perf_counter();job['event'].set()
def initialize_worker():
    local.decoder = Decoder()
    contexts.append(local.decoder)

pool = ThreadPoolExecutor(max_workers=WORKERS, initializer=initialize_worker)
def shutdown():
    global stopping,closed
    with admission:
        if closed:return
        closed=True
        stopping=True;admission.notify_all()
        for job in pending:
            job['error']=RuntimeError('sidecar is shutting down');job['event'].set()
        pending.clear()
    pool.shutdown(wait=True)
    for context in contexts:
        context.close()
atexit.register(shutdown)
# Health is only available once every persistent worker has a CUDA context.
ready = threading.Barrier(WORKERS)
list(pool.map(lambda _: ready.wait(timeout=60), range(WORKERS)))


def allowed_source(path_text):
    path = Path(path_text).resolve(strict=True)
    if not path.is_file() or not any(path.is_relative_to(root) for root in ALLOWED_ROOTS):
        raise ValueError("source path is outside configured River asset roots")
    return path


def tile_bounds(tile_x, tile_y, edge, width, height):
    if tile_x < 0 or tile_y < 0:
        raise ValueError("negative tile coordinate")
    x0 = min(tile_x * edge, max(width - 1, 0))
    y0 = min(tile_y * edge, max(height - 1, 0))
    x1 = min((tile_x + 1) * edge, width)
    y1 = min((tile_y + 1) * edge, height)
    if x0 >= x1 or y0 >= y1:
        raise ValueError("tile lies outside the JPEG 2000 source")
    return x0, y0, x1, y1


def decode_tiles(request):
    prepared_started=time.perf_counter()
    source = allowed_source(request["sourcePath"])
    image_width = int(request["sourceWidth"])
    image_height = int(request["sourceHeight"])
    tile_edge = int(request["sourceTile"])
    reduction = int(request["reduction"])
    if not 0 <= reduction <= 15 or tile_edge <= 0 or image_width <= 0 or image_height <= 0:
        raise ValueError("invalid decode dimensions or reduction")
    scale = 1 << reduction
    positions = request["tiles"]
    if not positions or len(positions) > 16:
        raise ValueError("nvJPEG2000 request must contain 1 to 16 tiles")
    bounds = [tile_bounds(int(x), int(y), tile_edge, image_width, image_height)
              for x, y in positions]
    # Validate the sum of reduced outputs before entering the worker queue.
    if sum(((x1 + scale - 1) // scale - (x0 + scale - 1) // scale)
           * ((y1 + scale - 1) // scale - (y0 + scale - 1) // scale)
           for x0, y0, x1, y1 in bounds) > MAX_ROI_PIXELS:
        raise ValueError("decoded output exceeds its configured pixel limit")
    footprint=None
    if reduction<3:
        stat=source.stat();grid=native_grid(source,stat.st_size,stat.st_mtime_ns,image_width,image_height)
        if grid:
            tw,th=grid
            footprint=tuple(sorted({(x,y) for a,b,c,d in bounds for y in range(b//th,(d-1)//th+1) for x in range(a//tw,(c-1)//tw+1)}))
        if os.environ.get('RIVER_NVJPEG2000_SCATTER_BATCH','false')=='true':footprint='scatter'
    job=dict(key=(source,image_width,image_height,reduction,footprint),bounds=bounds,queued=time.perf_counter(),
             pixels=sum(((c+scale-1)//scale-(a+scale-1)//scale)*((d+scale-1)//scale-(b+scale-1)//scale) for a,b,c,d in bounds),event=threading.Event())
    with admission:
        if len(pending)>=128:raise ValueError('sidecar admission queue is full')
        pending.append(job);admission.notify_all()
    if not job['event'].wait(110):raise TimeoutError('sidecar batch deadline exceeded')
    if 'error' in job:raise job['error']
    decoded,timing,queue_ms,decode_ms=job['result']
    prepare_ms=(job['queued']-prepared_started)*1000
    dispatch_ms=(time.perf_counter()-job['completed'])*1000
    serialization_started=time.perf_counter()
    codec=request.get('rpcCodec','raw')
    output = []
    payloads = []
    for (tile_x, tile_y), rgba in zip(positions, decoded):
        payload = encode_raster(rgba,codec)
        payloads.append(payload)
        output.append({
            "tileX": int(tile_x),
            "tileY": int(tile_y),
            "width": rgba.shape[1],
            "height": rgba.shape[0],
            "byteLength": len(payload),
            "pixelEncoding": codec,
        })
    metadata = {"decodeMs": decode_ms, "queueMs": queue_ms, "timing": {**timing,'prepareMs':prepare_ms,'dispatchMs':dispatch_ms,'serializeMs':(time.perf_counter()-serialization_started)*1000}, "tiles": output}
    header = json.dumps(metadata, separators=(",", ":")).encode("utf-8")
    return [struct.pack('<I', len(header))+header,*payloads]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def reply(self, status, value):
        body = json.dumps(value, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            with warmup_lock: warmup_state = dict(warmup)
            self.reply(200, {"status": "ok", "driver": "nvjpeg2000-cuda-native", "device": int(os.environ.get('RIVER_NVJPEG2000_DEVICE', '0')), "workers": WORKERS, "warmup": warmup_state})
        else:
            self.reply(404, {"error": "not found"})

    def do_POST(self):
        if self.path != "/decode":
            self.reply(404, {"error": "not found"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length <= 0 or length > 1024 * 1024:
                raise ValueError("invalid decode request size")
            request = json.loads(self.rfile.read(length))
            body = decode_tiles(request)
            self.send_response(200)
            self.send_header("Content-Type", "application/x-river-rgba")
            self.send_header("Content-Length", str(sum(len(part) for part in body)))
            self.end_headers()
            for part in body:self.wfile.write(part)
        except ValueError as error:
            self.reply(422, {"error": str(error)})
        except Exception as error:
            self.reply(500, {"error": f"nvJPEG2000 decode failed: {error}"})

    def log_message(self, _format, *_args):
        return


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 128
    # Bound connections as well as CUDA work, including persistent idle clients.
    def __init__(self, *args):
        super().__init__(*args)
        self.connections = threading.BoundedSemaphore(64)

    def process_request(self, request, address):
        request.settimeout(120)
        request.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.connections.acquire()
        try:
            super().process_request(request, address)
        except Exception:
            self.connections.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.connections.release()


def packaged_warm_sources():
    for root in ALLOWED_ROOTS:
        catalog = root/'river-warmup'/'sources.json'
        if not catalog.is_file(): continue
        if catalog.stat().st_size > 4096: raise ValueError('warmup catalog exceeds bound')
        entries = json.loads(catalog.read_text())
        if not 1 <= len(entries) <= 3: raise ValueError('warmup source count exceeds bound')
        result = []
        for entry in entries:
            source = allowed_source(str(catalog.parent/entry['source']))
            if source.stat().st_size > 65536 or (entry['width'],entry['height'],entry['reduction']) != (64,64,2):
                raise ValueError('warmup source exceeds fixed decode bounds')
            result.append(source)
        return result
    return []

def schedule_packaged_warmup(sources):
    started = time.perf_counter()
    barrier = threading.Barrier(WORKERS)
    with warmup_lock: warmup.update(status='running',sources=[source.name for source in sources],workersReady=0)
    def prime():
        try:
            for source in sources:
                local.decoder.decode(source,64,64,[(0,0,64,64)],2,4096)
            with warmup_lock: warmup['workersReady'] += 1
        finally: barrier.wait(timeout=30)
    # Enqueue before persistent batch loops, but do not wait before listening.
    futures = [pool.submit(prime) for _ in range(WORKERS)]
    def watch():
        try:
            for future in futures: future.result(timeout=35)
            with warmup_lock: warmup['status'] = 'ready'
        except Exception as error:
            with warmup_lock: warmup.update(status='failed',error=str(error))
        finally:
            with warmup_lock: warmup['elapsedMs'] = (time.perf_counter()-started)*1000
    threading.Thread(target=watch,name='river-gpu-warmup',daemon=True).start()

if __name__ == "__main__":
    packaged = []
    if os.environ.get('RIVER_NVJPEG2000_WARMUP', 'true') == 'true':
        try: packaged = packaged_warm_sources()
        except Exception as error: warmup.update(status='failed',error=str(error))
    if packaged:
        schedule_packaged_warmup(packaged)
    elif os.environ.get('RIVER_NVJPEG2000_WARMUP', 'true') == 'true':
        started = time.perf_counter()
        try:
            candidate = None
            for root in ALLOWED_ROOTS:
                catalog = root / 'river-assets.json'
                if catalog.is_file():
                    for entry in json.loads(catalog.read_text())['assets']:
                        source = allowed_source(str(root / entry['source']))
                        from resolution_view import get
                        if get(source, 6) is not None:
                            candidate = (source, int(entry['width']), int(entry['height']));break
                if candidate:break
            if candidate:
                source,w,h = candidate
                barrier = threading.Barrier(WORKERS)
                def prime(_):
                    try:
                        # Prime every in-flight state with small LL tiles;
                        # no complete source file is copied or raster cached.
                        local.decoder.decode(source,w,h,[(0,0,min(w,8192),min(h,1024))],6,MAX_ROI_PIXELS)
                    finally:
                        barrier.wait(timeout=60)
                list(pool.map(prime,range(WORKERS)))
                warmup.update(status='ready',source=source.name)
            else:warmup['status']='skipped'
        except Exception as error:
            warmup.update(status='failed',error=str(error))
        warmup['elapsedMs']=(time.perf_counter()-started)*1000
    for _ in range(WORKERS):pool.submit(batch_worker)
    try:
        Server((os.environ.get('RIVER_NVJPEG2000_BIND', '0.0.0.0'), int(os.environ.get("PORT", "8081"))), Handler).serve_forever()
    finally:
        shutdown()
