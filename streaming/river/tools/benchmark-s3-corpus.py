"""Cold/repeated protocol loads against real, read-only corpus objects.

Run against a milestone image (default http://127.0.0.1:18380). This fixture
serves local files as version-pinned S3 ranges. It never copies or hashes full
objects. 'Cold' means a new River source identity/range cache, not an OS-cache
flush. Measurements stop at complete preview coverage and close the socket.
"""
import argparse
import datetime
import hashlib
import json
from pathlib import Path
import subprocess
import socket
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote, urlencode, urlparse
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
WS = r'''
const cfg = JSON.parse(process.argv[1]);
function one(index) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(cfg.url); ws.binaryType = 'arraybuffer';
    let requestAt; let previewMs = null; const tiles = new Set(); const previewTiles = new Set(); const frames = [];
    const timer = setTimeout(() => { ws.close(); reject(new Error('preview timed out')); }, 90000);
    ws.onopen = () => { requestAt = performance.now(); ws.send(JSON.stringify(cfg.requests?.[index] || cfg.request)); };
    ws.onmessage = event => {
      if (typeof event.data === 'string') {
        const message = JSON.parse(event.data);
        if (message.type === 'error') { clearTimeout(timer); ws.close(); reject(new Error(JSON.stringify(message))); }
        return;
      }
      const bytes = Buffer.from(event.data); const size = bytes.readUInt32LE(4);
      const frame = JSON.parse(bytes.subarray(8, 8 + size));
      if (frame.pass === 'preview') {
        previewTiles.add(`${frame.tileX}:${frame.tileY}`);
        if (previewTiles.size === (frame.previewPatchCount || cfg.tiles)) previewMs = performance.now() - requestAt;
      }
      if (frame.pass === (cfg.complete ? 'final' : 'preview')) tiles.add(`${frame.tileX}:${frame.tileY}`);
      frames.push({ x: frame.tileX, y: frame.tileY, sourceTileEdge:frame.sourceTileEdge, bytes: bytes.length,
        atMs: performance.now() - requestAt, timing: frame.serverTiming,
        ...(cfg.verifyPixels ? {pass:frame.pass, layers:frame.sourceLayers, png:bytes.subarray(8 + size).toString('base64')} : {}) });
      if (cfg.cancelAfterFirst || tiles.size === ((!cfg.complete && frame.previewPatchCount) || cfg.tiles)) {
        clearTimeout(timer); resolve({ firstMs: frames[0].atMs, previewMs, finalMs: cfg.complete ? performance.now() - requestAt : null, frames }); ws.close();
      }
    };
    ws.onerror = event => { clearTimeout(timer); reject(new Error('WebSocket failed')); };
  });
}
try {
  const samples = await Promise.all(Array.from({length:cfg.viewers || 1}, (_,index) => one(index)));
  if (samples.length === 1) console.log(JSON.stringify(samples[0]));
  else {
    const stat = key => {
      const sorted = samples.map(sample => sample[key]).sort((a,b) => a-b);
      return { p50:sorted[Math.floor((sorted.length-1)*.5)], p95:sorted[Math.ceil((sorted.length-1)*.95)], max:sorted.at(-1) };
    };
    console.log(JSON.stringify({ viewers:samples.length, firstMs:stat('firstMs'), previewMs:stat('previewMs'), finalMs:cfg.complete ? stat('finalMs') : null, samples }));
  }
} catch(error) { console.error(error.message); process.exit(1); }
'''


def load_preview_reference(path, reduction, layers, ppm=None):
    """Independent full-source oracle, with explicit 16-bit normalization."""
    import numpy as np
    from PIL import Image
    if not ppm:
        image = Image.open(path); image.reduce = reduction; image.layers = layers; image.load()
        return image.convert('RGBA')
    with Path(ppm).open('rb') as stream:
        assert stream.readline().strip() == b'P6'
        tokens = []
        while len(tokens) < 3:
            line = stream.readline(); assert line, 'truncated PPM header'
            if not line.startswith(b'#'): tokens.extend(line.split())
        width,height,maximum = map(int,tokens)
        assert 0 < width*height <= 16_000_000 and maximum in (255,65535)
        raw = stream.read(width*height*3*(2 if maximum>255 else 1)+1)
        rgb = np.frombuffer(raw,dtype='>u2' if maximum>255 else np.uint8).reshape(height,width,3).astype(np.uint64)
        rgb = ((rgb*255+maximum//2)//maximum).astype(np.uint8)
        return Image.fromarray(rgb).convert('RGBA')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--api', default='http://127.0.0.1:18380')
    parser.add_argument('--endpoint-host', default='host.docker.internal')
    parser.add_argument('--driver', default='openjpeg')
    parser.add_argument('--tiny-source', type=Path, help='Read-only override for an additional small JPEG 2000 fixture')
    parser.add_argument('--objects', nargs='+', default=['tiny', 'small', 'large'])
    parser.add_argument('--output')
    parser.add_argument('--origin-http11', action='store_true', help='Use persistent HTTP/1.1 fixture connections to model real S3 connection reuse')
    parser.add_argument('--record-range-events', action='store_true', help='Include bounded fixture range timing diagnostics in the saved report')
    parser.add_argument('--viewers', type=int, default=1)
    parser.add_argument('--distinct', action='store_true')
    parser.add_argument('--complete', action='store_true')
    parser.add_argument('--range-latency-ms', type=float, default=0,
                        help='Simulated per-range service latency; OS caches remain unchanged')
    parser.add_argument('--head-latency-ms', type=float, default=0)
    parser.add_argument('--progressive-patches', action='store_true')
    parser.add_argument('--cancel-after-first', action='store_true')
    parser.add_argument('--parallel-open', action='store_true', help='Open distinct source manifests concurrently')
    parser.add_argument('--verify-preview-pixels', action='store_true', help='Use PNG and independently verify small-corpus preview pixels')
    parser.add_argument('--preview-reference', help='Independent full-source P6 PPM preview for one corpus object')
    parser.add_argument('--reference-reduction', type=int, default=6)
    parser.add_argument('--wmts-preview', action='store_true', help='Verify one first-layer PNG overview tile on the small corpus')
    parser.add_argument('--repeat-runs', type=int, default=1,
                        help='Number of warm protocol requests after each fresh source load')
    parser.add_argument('--restart-container',
                        help='Restart a river-goal-* test container to measure process-cold technical-index reuse')
    args = parser.parse_args()
    if not 0 <= args.range_latency_ms <= 1000:
        parser.error('--range-latency-ms must be between 0 and 1000')
    if not 0 <= args.head_latency_ms <= 1000:
        parser.error('--head-latency-ms must be between 0 and 1000')
    if not 1 <= args.repeat_runs <= 100:
        parser.error('--repeat-runs must be between 1 and 100')
    if args.restart_container and not args.restart_container.startswith('river-goal-'):
        parser.error('--restart-container accepts only task-owned river-goal-* test containers')
    paths = {
        'tiny': args.tiny_source or ROOT / 'corpus/cache/small_world_non_consecutive_tilepart_tlm.jp2',
        'small': ROOT / 'corpus/cache/Bretagne2.j2k',
        'large': ROOT / 'corpus/cache/sentinel-date-mosaics/2016-09-12.jp2',
    }
    cases = {key + paths[key].suffix: paths[key] for key in args.objects}
    if args.preview_reference and (not args.verify_preview_pixels or len(cases) != 1):
        parser.error('--preview-reference requires one object and --verify-preview-pixels')
    if (args.verify_preview_pixels or args.wmts_preview) and (args.complete or (not args.preview_reference and any(path.stat().st_size > 8 * 1024 * 1024 for path in cases.values()))):
        parser.error('preview pixel verification accepts preview-only corpus objects up to 8 MiB')
    if args.wmts_preview and args.viewers != 1:
        parser.error('WMTS pixel verification uses one viewer')
    objects = dict(cases)
    if args.distinct:
        for key, path in cases.items():
            for i in range(args.viewers):
                objects[f'{Path(key).stem}-{i}{Path(key).suffix}'] = path
    metadata = {key: path.stat() for key, path in objects.items()}
    etags = {key: '"' + hashlib.sha256(f'{path}:{metadata[key].st_size}:{metadata[key].st_mtime_ns}'.encode()).hexdigest() + '"'
             for key, path in objects.items()}
    events = []
    heads = []
    event_lock = threading.Lock()
    origin_load = {'active': 0, 'peak': 0}

    class S3(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        protocol_version = "HTTP/1.1" if args.origin_http11 else "HTTP/1.0"

        def setup(self):
            super().setup()
            self.connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)

        def log_message(self, *_):
            pass

        def object(self):
            key = unquote(urlparse(self.path).path).split('/', 2)[-1]
            return key, objects[key], metadata[key]

        def do_HEAD(self):
            head_started=time.perf_counter()
            if args.head_latency_ms:
                time.sleep(args.head_latency_ms / 1000)
            key, _, meta = self.object()
            self.send_response(200)
            self.send_header('Content-Length', str(meta.st_size))
            self.send_header('ETag', etags[key])
            self.send_header('Last-Modified', 'Wed, 14 Oct 2026 00:00:00 GMT')
            self.end_headers()
            with event_lock: heads.append({'key':key,'startedAt':head_started,'readyAt':time.perf_counter()})

        def do_GET(self):
            with event_lock:
                origin_load['active'] += 1
                origin_load['peak'] = max(origin_load['peak'], origin_load['active'])
            try:
                self.serve_range()
            finally:
                with event_lock:
                    origin_load['active'] -= 1

        def serve_range(self):
            range_started = time.perf_counter()
            if args.range_latency_ms:
                time.sleep(args.range_latency_ms / 1000)
            key, path, meta = self.object()
            assert self.headers.get('Range'), 'unbounded object GET'
            start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
            initial=self.headers.get('If-Match') is None and start==0 and end==1023
            if (not initial and self.headers.get('If-Match') != etags[key]) or path.stat().st_mtime_ns != meta.st_mtime_ns:
                self.send_response(412)
                self.send_header('Content-Length', '0')
                self.end_headers()
                return
            end=min(end,meta.st_size-1)
            assert end - start + 1 <= 1024 * 1024
            with path.open('rb') as stream:
                stream.seek(start)
                body = stream.read(end - start + 1)
            with event_lock:
                events.append({'key': key, 'bytes': len(body), 'start': start, 'end': end, 'startedAt': range_started, 'readyAt': time.perf_counter(), 'clientPort': self.client_address[1]})
            self.send_response(206)
            self.send_header('Content-Length', str(len(body)))
            self.send_header('Content-Range', f'bytes {start}-{end}/{meta.st_size}')
            self.send_header('ETag', etags[key])
            self.send_header('Last-Modified', 'Wed, 14 Oct 2026 00:00:00 GMT')
            self.end_headers()
            try:
                self.wfile.write(body)
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass

    class Origin(ThreadingHTTPServer):
        request_queue_size = 256
    server = Origin(('127.0.0.1', 0), S3)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    measurements = []
    try:
        def request(path, body=None, method=None):
            headers = {'Content-Type': 'application/json', 'Authorization': 'Bearer fixture-admin'}
            with urlopen(Request(args.api + path, data=json.dumps(body).encode() if body is not None else None,
                                 headers=headers, method=method), timeout=45) as response:
                return json.loads(response.read()) if response.status != 204 else None

        for key, path in cases.items():
            source = 'bench-' + str(time.time_ns())
            source_config = {'id': source, 'name': source, 'endpoint': f'http://{args.endpoint_host}:{server.server_port}',
                'bucket': 'jp2', 'accessKeyId': 'fixture', 'secretAccessKey': 'fixture', 'allowHttp': True}
            request('/api/s3/sources', source_config)
            started = time.perf_counter()
            before = len(events)
            before_heads = len(heads)
            try:
                keys = [f'{Path(key).stem}-{i}{Path(key).suffix}' for i in range(args.viewers)] if args.distinct else [key]
                def open_one(object_key):
                    opened = request(f'/api/s3/sources/{source}/assets', {'key': object_key})
                    return request('/api/assets/' + opened['assetId'] + '/manifest')
                if args.parallel_open:
                    from concurrent.futures import ThreadPoolExecutor
                    with ThreadPoolExecutor(max_workers=min(len(keys), 16)) as pool:
                        metas = list(pool.map(open_one, keys))
                else:
                    metas = [open_one(object_key) for object_key in keys]
                meta = metas[0]
                open_ms = (time.perf_counter() - started) * 1000
                import math
                zoom = min(meta['resolutionLevels'] - 1, max(0, math.ceil(math.log2(max(meta['width'] / 1920, meta['height'] / 1080)))))
                width, height = min(meta['width'], 1920 * 2 ** zoom), min(meta['height'], 1080 * 2 ** zoom)
                count = math.ceil(width / (512 * 2 ** zoom)) * math.ceil(height / (512 * 2 ** zoom))
                cfg = {'url': args.api.replace('http', 'ws', 1) + '/ws', 'tiles': count, 'viewers': args.viewers, 'complete': args.complete,
                       'verifyPixels': args.verify_preview_pixels, 'cancelAfterFirst':args.cancel_after_first,
                       'request': {'type': 'viewport', 'progressivePatches':args.progressive_patches, 'viewport': {'assetId': meta['assetId'], 'sequence': 1,
                            'x': 0, 'y': 0, 'width': width, 'height': height, 'zoom': zoom},
                            'format': 'png' if args.verify_preview_pixels else 'webp', 'driver': args.driver}}
                if args.distinct:
                    cfg['requests'] = [dict(cfg['request'], viewport=dict(cfg['request']['viewport'], assetId=value['assetId'])) for value in metas]
                states = ['cold'] + (['restart'] if args.restart_container else []) + ['repeat'] * args.repeat_runs
                for run, state in enumerate(states):
                    begin = time.perf_counter()
                    marker = before if state == 'cold' else len(events)
                    head_marker = before_heads if state == 'cold' else len(heads)
                    state_open_ms = open_ms if state == 'cold' else 0
                    if state == 'restart':
                        subprocess.run(['docker', 'restart', args.restart_container], check=True, capture_output=True, timeout=45)
                        deadline = time.monotonic() + 30
                        while True:
                            try:
                                with urlopen(args.api + '/health', timeout=2) as response:
                                    assert response.status == 200
                                break
                            except Exception:
                                if time.monotonic() >= deadline:
                                    raise
                                time.sleep(.2)
                        reopen_started = time.perf_counter()
                        request('/api/s3/sources', source_config)
                        for object_key, previous in zip(keys, metas):
                            opened = request(f'/api/s3/sources/{source}/assets', {'key': object_key})
                            current = request('/api/assets/' + opened['assetId'] + '/manifest')
                            assert current['assetId'] == previous['assetId'], 'restart changed object snapshot identity'
                        state_open_ms = (time.perf_counter() - reopen_started) * 1000
                        begin = time.perf_counter()
                    if args.wmts_preview:
                        import io
                        import numpy as np
                        from PIL import Image
                        reduction = min(zoom + 2, meta['resolutionLevels'] - 1)
                        query = urlencode(dict(SERVICE='WMTS', REQUEST='GetTile', VERSION='1.0.0', LAYER=meta['assetId'],
                            STYLE='default', FORMAT='image/png', DRIVER=args.driver, LAYERS='1',
                            TILEMATRIXSET='RiverPixel-' + meta['assetId'], TILEMATRIX=str(meta['resolutionLevels'] - 1 - reduction),
                            TILEROW='0', TILECOL='0'))
                        with urlopen(args.api + '/wmts?' + query, timeout=30) as response:
                            png = response.read()
                        elapsed = (time.perf_counter() - begin) * 1000
                        actual = Image.open(io.BytesIO(png)).convert('RGBA')
                        if args.preview_reference: assert reduction == args.reference_reduction
                        reference = load_preview_reference(path,reduction,1,args.preview_reference)
                        expected = reference.crop((0, 0, actual.width, actual.height))
                        assert np.array_equal(np.asarray(actual), np.asarray(expected)), 'WMTS first-layer pixels'
                        sample = {'firstMs': elapsed, 'firstNonBlackMs': elapsed if np.any(np.asarray(actual)[:,:,:3]) else None, 'previewMs': elapsed, 'previewPixels': 'exact', 'requestMethod': 'wmts'}
                        child = subprocess.CompletedProcess([], 0, json.dumps(sample), '')
                    else:
                        child = subprocess.run(['node', '-e', WS, json.dumps(cfg)], text=True, capture_output=True, timeout=100)
                    if child.returncode:
                        measurements.append({'object': key, 'state': state, 'driver': args.driver, 'error': child.stderr.strip()})
                        print(json.dumps(measurements[-1]), flush=True)
                        break
                    sample = json.loads(child.stdout)
                    if args.verify_preview_pixels and not args.wmts_preview:
                        import base64, io
                        import numpy as np
                        from PIL import Image
                        reduction = min(zoom + 2, meta['resolutionLevels'] - 1)
                        for viewer in sample.get('samples', [sample]):
                            for frame in viewer['frames']:
                                actual = Image.open(io.BytesIO(base64.b64decode(frame['png']))).convert('RGBA')
                                if args.preview_reference:
                                    assert reduction == args.reference_reduction and frame['layers'] == 1
                                reference = load_preview_reference(path,reduction,frame['layers'],args.preview_reference)
                                left = frame['x'] * frame.get('sourceTileEdge',512 << zoom) // (1 << reduction)
                                top = frame['y'] * frame.get('sourceTileEdge',512 << zoom) // (1 << reduction)
                                if np.any(np.asarray(actual)[:,:,:3]):
                                    viewer.setdefault('firstNonBlackMs', frame['atMs'])
                                expected = reference.convert('RGBA').crop((left, top, left + actual.width, top + actual.height))
                                if not np.array_equal(np.asarray(actual), np.asarray(expected)) and args.output:
                                    diagnostic=Path(args.output).with_suffix('')
                                    actual.save(str(diagnostic)+'-'+key+'-actual.png')
                                    expected.save(str(diagnostic)+'-'+key+'-expected.png')
                                assert np.array_equal(np.asarray(actual), np.asarray(expected)), (key, 'preview pixel mismatch', frame['x'], frame['y'])
                                del frame['png']
                        sample['previewPixels'] = 'exact'
                    if args.cancel_after_first:
                        time.sleep(.15); settled=len(events); time.sleep(.15)
                        assert len(events)==settled, 'source reads continued after preview cancellation'
                        sample['cancellation']='source reads stopped after first patch'
                    sample.update({'object': key, 'state': state, 'run': run, 'driver': args.driver, 'objectBytes': path.stat().st_size,
                                   'openMs': state_open_ms,
                                   'wallMs': (time.perf_counter() - begin) * 1000,
                                   'sourceReadBytes': sum(event['bytes'] for event in events[marker:]),
                                   'rangeRequests': len(events[marker:]), 'sourceHeadRequests': len(heads)-head_marker, 'distinctSources': args.distinct,
                                   'rangeLatencyMs': args.range_latency_ms})
                    sample['headLatencyMs'] = args.head_latency_ms
                    sample['parallelOpen'] = args.parallel_open
                    measurements.append(sample)
                    print(json.dumps({k: v for k, v in sample.items() if k not in ('frames', 'samples')}), flush=True)
            finally:
                request('/api/s3/sources/' + source, method='DELETE')
    finally:
        server.shutdown()
    result = {'scope': 'fresh S3 identity / compressed cache; OS cache not flushed; protocol excludes browser decode/paint',
              'originPeakRequests': origin_load['peak'], 'originHttp11': args.origin_http11,
              'api': args.api, 'driver': args.driver, 'rangeLatencyMs': args.range_latency_ms,
              'measurements': measurements}
    if args.record_range_events:
        result['rangeEvents'] = events
        result['headEvents'] = heads
    if args.output:
        Path(args.output).write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
    # A saved report is not a successful verification if a viewer failed.
    return 1 if any('error' in sample for sample in measurements) else 0


if __name__ == '__main__':
    raise SystemExit(main())
