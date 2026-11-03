"""Real JPEG 2000/S3 range/pixel regression; no Docker or S3 account required.

Requires Pillow, numpy, and Node 22+ (for its built-in WebSocket client).
Run: cargo build -p river-server && python tools/verify-s3-ranges.py
"""
import argparse
import base64
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.parse import parse_qs, unquote, urlencode, urlparse
from urllib.request import Request, urlopen

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
def source_route(route):
    if route in {'openht-packed-ranges','openht-pipeline-ranges'}:return 's3-packet-ranges'
    return route.removeprefix('openht-fallback-').removeprefix('openjpeg-pcrl-lowest-')

WS = r'''
const config = JSON.parse(process.argv[1]);
const socket = new WebSocket(config.url);
socket.binaryType = 'arraybuffer';
const frames = [];
const started = performance.now();
const timeout = setTimeout(() => { console.error('WebSocket timed out'); process.exit(1); }, 20000);
socket.onopen = () => socket.send(JSON.stringify(config.request));
socket.onmessage = event => {
  if (typeof event.data === 'string') {
    const body = JSON.parse(event.data);
    if (body.type === 'error') { console.error(JSON.stringify(body)); process.exit(1); }
    return;
  }
  const bytes = Buffer.from(event.data);
  const headerLength = bytes.readUInt32LE(4);
  const header = JSON.parse(bytes.subarray(8, 8 + headerLength));
  frames.push({ header, png: bytes.subarray(8 + headerLength).toString('base64'), atMs: performance.now() - started });
  if (header.pass === 'preview' && config.previewOnly) {
    setTimeout(() => { clearTimeout(timeout); console.log(JSON.stringify(frames)); socket.close(); }, 500);
  } else if (header.pass === 'final' && !config.previewOnly && frames.filter(frame => frame.header.pass === 'final').length >= (config.finalCount ?? 1)) {
    clearTimeout(timeout); console.log(JSON.stringify(frames)); socket.close();
  }
};
'''


def fixture(pixels, **options):
    stream = io.BytesIO()
    options.setdefault('num_resolutions', 7)
    Image.fromarray(pixels).save(stream, format='JPEG2000', irreversible=False,
                               **options)
    return stream.getvalue()


def reference(data, reduction, layers=0):
    image = Image.open(io.BytesIO(data))
    image.reduce = reduction
    image.layers = layers
    image.load()
    if image.mode in ('I;16', 'I'):
        pixels = np.asarray(image).astype(np.uint64)
        pixels = ((pixels * 255 + 32767) // 65535).astype(np.uint8)
        return Image.fromarray(pixels).convert('RGBA')
    return image.convert('RGBA')


def verify():
    parser = argparse.ArgumentParser()
    parser.add_argument('--api')
    parser.add_argument('--origin-reference-container', help='Task-owned Grok container for independent shifted-origin fixtures')
    parser.add_argument('--compatibility', action='store_true', help='Force a small selective-read budget and verify disk compatibility pixels')
    parser.add_argument('--compatibility-ranges', action='store_true', help='Force the missing tile-part index request-limit path')
    parser.add_argument('--restart-container', help='Task-owned river-goal-* container for process-cold pixel checks')
    parser.add_argument('--endpoint-host', default='host.docker.internal')
    parser.add_argument('--driver', default='openjpeg')
    parser.add_argument('--range-latency-ms', type=float, default=0)
    parser.add_argument('--slow-payload-ms', type=float, default=0, help='Hold one PCRL payload response to verify rolling range admission')
    parser.add_argument('--max-preview-fraction', type=float, default=.25, help='Transfer gate; use 1 only when measuring an older baseline')
    parser.add_argument('--output')
    args = parser.parse_args()
    if args.origin_reference_container and not args.origin_reference_container.startswith('river-goal-'):
        parser.error('origin oracle must use a task-owned river-goal-* container')
    if args.api and args.slow_payload_ms:
        parser.error('--slow-payload-ms uses the owned local server with a known four-range concurrency limit')
    args.compatibility = args.compatibility or args.compatibility_ranges
    if not 0 <= args.range_latency_ms <= 1000 or not 0 <= args.slow_payload_ms <= 5000 or not 0 < args.max_preview_fraction <= 1:
        parser.error('invalid range latency or preview fraction')
    rng = np.random.default_rng(7421)
    pixels = rng.integers(0, 256, (768, 1024, 3), dtype=np.uint8)
    objects = {
        f'{order}.jp2': fixture(pixels, plt=True, progression=order, quality_layers=[8, 1],
                               tile_size=(512, 512), precinct_size=(64, 64), mct=1)
        for order in ('LRCP', 'RLCP', 'RPCL', 'PCRL', 'CPRL')
    }
    objects['multi-block.jp2'] = fixture(rng.integers(0, 65536, (1536, 2048), dtype=np.uint16),
                                        progression='RPCL', tile_size=(1024, 1024))
    objects['unindexed.jp2'] = fixture(pixels, progression='LRCP', tile_size=(512, 512))
    if args.origin_reference_container:
        for order in ('LRCP','RLCP','RPCL','PCRL','CPRL'):
            objects[f'image-origin-{order}.jp2'] = fixture(pixels, plt=True, progression=order,
                tile_size=(512,512), precinct_size=(64,64), offset=(13,29))
        objects['tile-origin.jp2'] = fixture(pixels, progression='LRCP', tile_size=(512,512), offset=(16,32), tile_offset=(16,32))
    objects['dense-header.jp2'] = fixture(pixels, progression='LRCP', tile_size=(512,512),
                                         num_resolutions=2, codeblock_size=(4,4))
    objects['ll.jp2'] = fixture(rng.integers(0, 65536, (1536, 2048), dtype=np.uint16),
                                progression='RPCL', precinct_size=(64, 64), tile_size=(512, 512))
    objects['large.jp2'] = fixture(rng.integers(0, 65536, (3072, 4096), dtype=np.uint16),
                                   plt=True, progression='RPCL', precinct_size=(64, 64), tile_size=(1024, 1024))
    # Add descriptive COM segments to a new synthetic source; entropy bytes
    # and all coding parameters remain identical to the LRCP pixel oracle.
    original = objects['LRCP.jp2']
    box = 0
    while original[box + 4:box + 8] != b'jp2c':
        box += int.from_bytes(original[box:box + 4], 'big')
    cursor = box + 10
    default_cod = None
    while original[cursor:cursor + 2] != b'\xff\x90':
        if original[cursor:cursor + 2] == b'\xff\x52':
            default_cod=original[cursor+4:cursor+2+int.from_bytes(original[cursor+2:cursor+4],'big')]
        cursor += 2 + int.from_bytes(original[cursor + 2:cursor + 4], 'big')
    assert default_cod is not None
    overrides=b''
    for component in range(3):
        body=bytes([component,default_cod[0]&1])+default_cod[5:]
        overrides+=b'\xff\x53'+(len(body)+2).to_bytes(2,'big')+body
    component_variant=bytearray(original[:cursor]+overrides+original[cursor:])
    component_variant[box:box+4]=(int.from_bytes(original[box:box+4],'big')+len(overrides)).to_bytes(4,'big')
    objects['main-coc.jp2']=bytes(component_variant)
    comments = (b'\xff\x64' + (60004).to_bytes(2, 'big') + b'\x00\x01' + b'c' * 60000) * 12
    modified = bytearray(original[:cursor] + comments + original[cursor:])
    modified[box:box + 4] = (int.from_bytes(original[box:box + 4], 'big') + len(comments)).to_bytes(4, 'big')
    objects['commented.jp2'] = bytes(modified)
    tile_modified = bytearray(original[:cursor + 12] + comments[:60006] + original[cursor + 12:])
    tile_modified[cursor + 6:cursor + 10] = (int.from_bytes(original[cursor + 6:cursor + 10], 'big') + 60006).to_bytes(4, 'big')
    tile_modified[box:box + 4] = (int.from_bytes(original[box:box + 4], 'big') + 60006).to_bytes(4, 'big')
    objects['tile-commented.jp2'] = bytes(tile_modified)
    # Fresh metadata-only variants: packet bytes and their independent pixel
    # oracle remain unchanged, including a TLM which omits tile identifiers.
    tlm_original = fixture(pixels,plt=True,progression='LRCP',tile_size=(128,128),precinct_size=(64,64),quality_layers=[8,1],mct=1)
    tlm_box = 0
    while tlm_original[tlm_box+4:tlm_box+8] != b'jp2c':
        tlm_box += int.from_bytes(tlm_original[tlm_box:tlm_box+4],'big')
    tlm_cursor = tlm_box+10
    while tlm_original[tlm_cursor:tlm_cursor+2] != b'\xff\x90':
        tlm_cursor += 2+int.from_bytes(tlm_original[tlm_cursor+2:tlm_cursor+4],'big')
    tlm_parts = []
    part_cursor = tlm_cursor
    while tlm_original[part_cursor:part_cursor+2] == b'\xff\x90':
        tile = int.from_bytes(tlm_original[part_cursor+4:part_cursor+6],'big')
        length = int.from_bytes(tlm_original[part_cursor+6:part_cursor+10],'big')
        assert length >= 14
        tlm_parts.append((tile,length)); part_cursor += length
    for kind,style in [('tlm-explicit',0x60),('tlm-anonymous',0x40)]:
        entries = b''.join((tile.to_bytes(2,'big') if style==0x60 else b'')+length.to_bytes(4,'big') for tile,length in tlm_parts)
        segment = b'\xff\x55'+(4+len(entries)).to_bytes(2,'big')+bytes([0,style])+entries
        variant = bytearray(tlm_original[:tlm_cursor]+segment+tlm_original[tlm_cursor:])
        variant[tlm_box:tlm_box+4] = (int.from_bytes(tlm_original[tlm_box:tlm_box+4],'big')+len(segment)).to_bytes(4,'big')
        objects[kind+'.jp2'] = bytes(variant)
    # Psot=0 is valid for the final tile part. Bound it to jp2c, not the
    # complete object: unrelated boxes may follow the codestream.
    zero_part = bytearray(original)
    last = cursor
    while zero_part[last:last + 2] != b'\xff\xd9':
        following = last + int.from_bytes(zero_part[last + 6:last + 10], 'big')
        if zero_part[following:following + 2] == b'\xff\xd9':
            zero_part[last + 6:last + 10] = b'\x00' * 4
            break
        last = following
    code_end = box + int.from_bytes(zero_part[box:box + 4], 'big')
    objects['final-unsized.j2k'] = bytes(zero_part[box + 8:code_end])
    objects['final-unsized.jp2'] = bytes(zero_part) + (16).to_bytes(4, 'big') + b'free' + b'padding!'
    reference_data = {key: original for key in ('final-unsized.j2k', 'final-unsized.jp2')}
    reference_data['main-coc.jp2']=objects['LRCP.jp2']
    reference_data.update({key:tlm_original for key in ('tlm-explicit.jp2','tlm-anonymous.jp2')})
    # Delay a real entropy range, not a later tile's PLT header. Header
    # dependencies cannot admit payload replacements until the header arrives.
    payload_spans=[];probe=objects['PCRL.jp2'];box=0
    while True:
        size=int.from_bytes(probe[box:box+4],'big');kind=probe[box+4:box+8];head=8
        if size==1:size=int.from_bytes(probe[box+8:box+16],'big');head=16
        if kind==b'jp2c':cursor=box+head+2;break
        assert size>=head;box+=size
    while probe[cursor:cursor+2]!=b'\xff\x90':cursor+=2+int.from_bytes(probe[cursor+2:cursor+4],'big')
    while probe[cursor:cursor+2]==b'\xff\x90':
        length=int.from_bytes(probe[cursor+6:cursor+10],'big');assert length>=14;start=cursor+12
        while probe[start:start+2]!=b'\xff\x93':start+=2+int.from_bytes(probe[start+2:start+4],'big')
        payload_spans.append((start+2,cursor+length));cursor+=length
    events = []
    lock = threading.Lock()

    class S3(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'
        def handle(self):
            try:
                super().handle()
            except (ConnectionResetError, BrokenPipeError):
                pass
        def setup(self):
            super().setup()
            self.connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        def log_message(self, *_):
            pass

        def do_HEAD(self):
            key = unquote(urlparse(self.path).path).split('/', 2)[-1]
            data = objects[key]
            self.send_response(200)
            self.send_header('Content-Length', str(len(data)))
            self.send_header('ETag', '"' + key + '"')
            self.send_header('Last-Modified', 'Wed, 14 Oct 2026 00:00:00 GMT')
            self.end_headers()

        def do_GET(self):
            if args.range_latency_ms:
                time.sleep(args.range_latency_ms / 1000)
            key = unquote(urlparse(self.path).path).split('/', 2)[-1]
            data = objects[key]
            expected = '"' + key + '"'
            assert self.headers.get('Range'), 'River requested an unbounded S3 download'
            start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
            assert self.headers.get('If-Match') == expected or (self.headers.get('If-Match') is None and start==0 and end==1023),self.headers
            end=min(end,len(data)-1)
            assert end - start + 1 <= 1024 * 1024
            body = data[start:end + 1]
            with lock:
                delayed = bool(args.slow_payload_ms and key == 'PCRL.jp2' and any(lo<=start and end<hi for lo,hi in payload_spans)
                    and b'\xff\x90\x00\x0a' not in body and not any(event.get('delayed') for event in events))
                event = {'key': key, 'start': start, 'end': end, 'bytes': len(body), 'at': time.perf_counter(), 'delayed': delayed}
                events.append(event)
            if delayed:
                time.sleep(args.slow_payload_ms/1000)
            self.send_response(206)
            self.send_header('Content-Range', f'bytes {start}-{end}/{len(data)}')
            self.send_header('Content-Length', str(len(body)))
            self.send_header('ETag', expected)
            self.send_header('Last-Modified', 'Wed, 14 Oct 2026 00:00:00 GMT')
            self.end_headers()
            self.wfile.write(body)
            event['completedAt'] = time.perf_counter()

    class FixtureServer(ThreadingHTTPServer):
        request_queue_size = 128
    s3 = FixtureServer(('127.0.0.1', 0), S3)
    threading.Thread(target=s3.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='river-range-test-') as work:
            origin_oracles = {}
            oracle_remote = '/tmp/river-origin-oracle-' + str(time.time_ns())
            def expected_source(key, reduction, layers=0):
                if not key.startswith('image-origin-') and key != 'tile-origin.jp2':
                    return reference(reference_data.get(key,objects[key]),reduction,layers)
                cache_key = (key,reduction,layers)
                if cache_key not in origin_oracles:
                    container = args.origin_reference_container
                    source_path = Path(work,key); source_path.write_bytes(objects[key])
                    subprocess.run(['docker','exec',container,'mkdir','-p',oracle_remote],check=True,capture_output=True)
                    subprocess.run(['docker','cp',str(source_path),container+':'+oracle_remote+'/'+key],check=True,capture_output=True)
                    output_name = f'{key}-{reduction}-{layers}.png'
                    command = ['docker','exec',container,'grk_decompress','-i',oracle_remote+'/'+key,
                        '-r',str(reduction),'-o',oracle_remote+'/'+output_name]
                    if layers: command += ['-l',str(layers)]
                    subprocess.run(command,check=True,capture_output=True)
                    subprocess.run(['docker','cp',container+':'+oracle_remote+'/'+output_name,str(Path(work,output_name))],check=True,capture_output=True)
                    origin_oracles[cache_key] = Image.open(Path(work,output_name)).convert('RGBA')
                return origin_oracles[cache_key].copy()
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0))
                port = sock.getsockname()[1]
            env = dict(os.environ, RIVER_BIND=f'127.0.0.1:{port}', RIVER_ADMIN_TOKEN='fixture-admin',
                       RIVER_S3_CACHE_ROOT=work, RIVER_MOSAIC_ROOT=work, RIVER_OTEL_ENABLED='false',
                       RIVER_REFINE_IDLE_DELAY_MS='1500', RIVER_PREVIEW_EXTRA_REDUCTIONS='2')
            env.pop('RIVER_ASSET_CATALOG', None)
            if args.slow_payload_ms:
                env['RIVER_S3_RANGE_CONCURRENCY'] = '4'
            if args.compatibility:
                if args.compatibility_ranges:
                    env['RIVER_S3_REQUEST_RANGES'] = '32'
                else:
                    env['RIVER_S3_REQUEST_BYTES'] = '65536'
            binary = ROOT / 'target/debug' / ('river-server.exe' if os.name == 'nt' else 'river-server')
            with open(Path(work) / 'server.log', 'w') as log:
                child = None if args.api else subprocess.Popen([str(binary)], cwd=ROOT, env=env, stdout=log, stderr=log,
                                         creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
                try:
                    base = args.api or f'http://127.0.0.1:{port}'

                    def request(path, body=None, admin=False):
                        headers = {'Content-Type': 'application/json'}
                        if admin:
                            headers['Authorization'] = 'Bearer fixture-admin'
                        try:
                            with urlopen(Request(base + path, data=json.dumps(body).encode() if body is not None else None,
                                                 headers=headers), timeout=40) as response:
                                return response.read(), response.headers
                        except HTTPError as error:
                            raise AssertionError(f'{path}: HTTP {error.code} {error.read().decode()}\n{Path(work, "server.log").read_text()}') from error

                    for _ in range(100):
                        try:
                            request('/health')
                            break
                        except OSError:
                            time.sleep(.05)
                    source_config = {'id': 'fixture', 'name': 'Fixture', 'endpoint': f'http://{args.endpoint_host if args.api else "127.0.0.1"}:{s3.server_port}',
                            'bucket': 'jp2', 'accessKeyId': 'fixture', 'secretAccessKey': 'fixture', 'allowHttp': True}
                    request('/api/s3/sources', source_config, admin=True)

                    def manifest(key):
                        loaded = json.loads(request('/api/s3/sources/fixture/assets', {'key': key})[0])
                        assert loaded['assetId'].startswith('s3v-') and loaded['identityKind'] == 's3-version'
                        assert loaded['bytesFetched'] <= (32768 if key == 'commented.jp2' else 8192), loaded
                        if key == 'commented.jp2':
                            print(json.dumps({'headerBytes': loaded['bytesFetched'], 'commentBytes': len(comments)}), flush=True)
                        return json.loads(request('/api/assets/' + loaded['assetId'] + '/manifest')[0])

                    def wmts(meta, reduction, x=0, y=0, layers=None):
                        query = urlencode(dict(SERVICE='WMTS', REQUEST='GetTile', VERSION='1.0.0',
                            LAYER=meta['assetId'], STYLE='default', FORMAT='image/png',
                            TILEMATRIXSET='RiverPixel-' + meta['assetId'], TILEMATRIX=str(meta['resolutionLevels'] - 1 - reduction),
                            TILEROW=str(y), TILECOL=str(x), DRIVER=args.driver))
                        if layers is not None:
                            query += '&LAYERS=' + str(layers)
                        png, headers = request('/wmts?' + query)
                        return Image.open(io.BytesIO(png)).convert('RGBA'), headers

                    measurements = []
                    for key in objects:
                        print("Checking", key, flush=True)
                        meta = manifest(key)
                        if key.startswith('image-origin-') or key == 'tile-origin.jp2':
                            assert meta['referenceOrigin'] == (dict(x=16,y=32) if key == 'tile-origin.jp2' else dict(x=13,y=29))
                            assert 'nvjpeg2000' not in meta['supportedDecoderDrivers']
                        before = len(events)
                        reduction = min(meta['resolutionLevels'] - 1, 6 if key == 'll.jp2' else 4)
                        preview_started = time.perf_counter()
                        image, headers = wmts(meta, reduction)
                        preview_ms = (time.perf_counter() - preview_started) * 1000
                        expected = expected_source(key,reduction).crop((0, 0, image.width, image.height))
                        actual_array, expected_array = np.asarray(image), np.asarray(expected)
                        if not np.array_equal(actual_array, expected_array):
                            print('Pixel mismatch', key, 'actual', actual_array[0, :8, 0].tolist(), 'expected', expected_array[0, :8, 0].tolist(), 'max diff', int(np.abs(actual_array.astype(int)-expected_array.astype(int)).max()), 'route', headers['X-River-Decoder-Route'], flush=True)
                        assert np.array_equal(actual_array, expected_array), (key, reduction, headers)
                        if key == 'PCRL.jp2' and args.slow_payload_ms:
                            delayed = next(event for event in events if event.get('delayed'))
                            continued = [event for event in events if event['key'] == key and delayed['at'] < event['at'] < delayed['completedAt']]
                            assert len(continued) >= 4, 'one held range prevented admission beyond the initial four-range window'
                        fetched = sum(event['bytes'] for event in events[before:])
                        route = headers['X-River-Decoder-Route']
                        assert source_route(route) in (('s3-packet-ranges', 's3-disk-compatibility') if args.compatibility else ('s3-packet-ranges',)), (key, route)
                        if not args.compatibility:
                            assert fetched < len(objects[key]) * args.max_preview_fraction, (key, fetched, len(objects[key]))
                        count = len(events)
                        image_again, cached = wmts(meta, reduction)
                        assert np.array_equal(np.asarray(image), np.asarray(image_again)) and len(events) == count
                        assert cached['X-River-Source-Read-Bytes'] == '0'
                        if args.driver != 'nvjpeg2000':
                            first_layer, _ = wmts(meta, reduction, layers=1)
                            expected_layer = expected_source(key,reduction,1).crop((0, 0, first_layer.width, first_layer.height))
                            assert np.array_equal(np.asarray(first_layer), np.asarray(expected_layer)), (key, 'WMTS first layer')
                            full_again, _ = wmts(meta, reduction)
                            assert np.array_equal(np.asarray(full_again), actual_array), 'layer-specific cache contaminated full-quality WMTS'
                        measurements.append({'key': key, 'objectBytes': len(objects[key]), 'previewBytes': fetched, 'previewMs': preview_ms,
                                             'ranges': len(events[before:]), 'route': route})
                        if key in ('ll.jp2', 'multi-block.jp2'):
                            middle, middle_headers = wmts(meta, 2)
                            expected_middle = reference(objects[key], 2).crop((0, 0, middle.width, middle.height))
                            assert np.array_equal(np.asarray(middle), np.asarray(expected_middle)), 'unindexed RPCL intermediate pixels'
                            assert source_route(middle_headers['X-River-Decoder-Route']) in ('s3-packet-ranges', 's3-disk-compatibility')
                        # Reuse low-resolution bytes, then fetch only additional detail.
                        detail_before = len(events)
                        image, headers = wmts(meta, 0, 1, 0)
                        if args.compatibility and source_route(route) == 's3-disk-compatibility':
                            assert len(events) == detail_before, 'compatibility source downloaded again for another tile'
                        expected = expected_source(key,0).crop((512, 0, 512 + image.width, image.height))
                        assert np.array_equal(np.asarray(image), np.asarray(expected)), (key, 'detail')
                        if key.startswith('final-unsized'):
                            bottom, _ = wmts(meta, 0, 1, 1)
                            expected_bottom = reference(reference_data.get(key, objects[key]), 0).crop((512, 512, 512 + bottom.width, 512 + bottom.height))
                            assert np.array_equal(np.asarray(bottom), np.asarray(expected_bottom)), (key, 'final tile part')

                    objects['tlm-anonymous-partial.jp2'] = objects['tlm-anonymous.jp2']
                    reference_data['tlm-anonymous-partial.jp2'] = tlm_original
                    partial_meta = manifest('tlm-anonymous-partial.jp2')
                    partial_before = len(events)
                    partial, partial_headers = wmts(partial_meta,0)
                    expected = reference(tlm_original,0).crop((0,0,partial.width,partial.height))
                    assert np.array_equal(np.asarray(partial),np.asarray(expected)), 'cold anonymous TLM ROI pixels'
                    partial_bytes = sum(event['bytes'] for event in events[partial_before:])
                    if source_route(partial_headers['X-River-Decoder-Route'])=='s3-disk-compatibility':
                        assert args.compatibility, 'ordinary anonymous TLM ROI unexpectedly downloaded the complete source'
                        assert partial_bytes >= len(tlm_original), 'compatibility route omitted original source bytes'
                    else:
                        assert partial_bytes < len(tlm_original)*.6, 'anonymous TLM discovery pulled unrelated tile bodies'
                    meta = manifest('RPCL.jp2')
                    ws_request = {'type': 'viewport', 'viewport': {'assetId': meta['assetId'], 'sequence': 1,
                        'x': 0, 'y': 0, 'width': 512, 'height': 512, 'zoom': 0}, 'format': 'png', 'driver': args.driver}
                    frames = json.loads(subprocess.check_output(['node', '-e', WS, json.dumps({
                        'url': base.replace('http', 'ws', 1) + '/ws', 'request': ws_request, 'previewOnly': True})], text=True, timeout=25))
                    assert len(frames) == 1 and frames[0]['header']['pass'] == 'preview'
                    image = Image.open(io.BytesIO(base64.b64decode(frames[0]['png']))).convert('RGBA')
                    expected = reference(objects['RPCL.jp2'], 2, 1).crop((0, 0, image.width, image.height))
                    assert np.array_equal(np.asarray(image), np.asarray(expected)), 'WebSocket preview pixels'
                    before = len(events)
                    time.sleep(1.8)
                    assert len(events) == before, 'closing preview socket continued reading source data'
                    frames = json.loads(subprocess.check_output(['node', '-e', WS, json.dumps({
                        'url': base.replace('http', 'ws', 1) + '/ws', 'request': ws_request, 'previewOnly': False})], text=True, timeout=25))
                    assert [frame['header']['pass'] for frame in frames] == ['preview', 'final']
                    assert frames[1]['atMs'] - frames[0]['atMs'] >= 1400
                    for frame in frames:
                        reduction = 2 if frame['header']['pass'] == 'preview' else 0
                        layers = frame['header']['sourceLayers']
                        image = Image.open(io.BytesIO(base64.b64decode(frame['png']))).convert('RGBA')
                        expected = reference(objects['RPCL.jp2'], reduction, layers).crop((0, 0, image.width, image.height))
                        assert np.array_equal(np.asarray(image), np.asarray(expected)), frame['header']
                        assert 'sourceReadBytes' in frame['header']['serverTiming']
                    before = len(events)
                    wmts_final, wmts_headers = wmts(meta, 0)
                    assert np.array_equal(np.asarray(wmts_final), np.asarray(image)), 'cross-protocol final pixels differ'
                    assert wmts_headers['X-River-Decoder-Route'] == 'encoded-raster-cache', 'WMTS redecoded WebSocket full-quality pixels'
                    assert float(wmts_headers['X-River-Decode-Ms']) == 0 and float(wmts_headers['X-River-Encode-Ms']) == 0
                    assert len(events) == before and wmts_headers['X-River-Source-Read-Bytes'] == '0'
                    wmts_count, count_headers = wmts(meta,0,layers=frames[-1]['header']['sourceLayers'])
                    assert np.array_equal(np.asarray(wmts_count),np.asarray(wmts_final)) and count_headers['X-River-Decoder-Route'] == 'encoded-raster-cache'
                    wmts(meta, 0, 1, 0)  # Refresh this tile after the larger fixture sweep may have evicted it.
                    batch = {'type': 'tiles', 'batch': {'assetId': meta['assetId'], 'sequence': 2,
                        'tiles': [{'x': 1, 'y': 0, 'z': 0}, {'x': 1, 'y': 1, 'z': 0}]}, 'format': 'png', 'driver': args.driver}
                    mixed = json.loads(subprocess.check_output(['node', '-e', WS, json.dumps({
                        'url': base.replace('http', 'ws', 1) + '/ws', 'request': batch, 'previewOnly': False, 'finalCount': 2})], text=True, timeout=25))
                    finals = [frame for frame in mixed if frame['header']['pass'] == 'final']
                    assert len(finals) == 2
                    assert {(frame['header']['tileX'],frame['header']['tileY']) for frame in finals} == {(1,0),(1,1)}
                    for frame in finals:
                        header = frame['header']
                        x, y = header['tileX'] * 512, header['tileY'] * 512
                        decoded = Image.open(io.BytesIO(base64.b64decode(frame['png']))).convert('RGBA')
                        expected = reference(objects['RPCL.jp2'], 0).crop((x,y,x+decoded.width,y+decoded.height))
                        assert np.array_equal(np.asarray(decoded),np.asarray(expected)), 'mixed cached/missing batch pixels differ'
                        route = header['serverTiming']['decoderRoute']
                        assert (route == 'encoded-raster-cache') == (header['tileY'] == 0), ('batch redecoded a cached tile or reused missing pixels', header)
                    assert not list(Path(work).rglob('river-range-*')), 'temporary compressed subsets leaked'
                    assert sum(path.stat().st_size for path in Path(work).rglob('river-compatible-*')) <= 2 * 1024**3, 'compatibility disk cache exceeded retention budget'
                    if args.restart_container:
                        assert args.api and args.restart_container.startswith('river-goal-'), 'restart only task-owned test containers'
                        subprocess.run(['docker', 'restart', args.restart_container], check=True, capture_output=True, timeout=45)
                        deadline = time.monotonic() + 30
                        while True:
                            try:
                                request('/health')
                                break
                            except OSError:
                                if time.monotonic() >= deadline:
                                    raise
                                time.sleep(.1)
                        request('/api/s3/sources', source_config, admin=True)
                        for key in objects:
                            meta = manifest(key)
                            for reduction, x in ((min(meta['resolutionLevels']-1,6 if key == 'll.jp2' else 4), 0), (0, 1)):
                                image, headers = wmts(meta, reduction, x)
                                left = x * 512
                                expected = expected_source(key,reduction).crop((left, 0, left + image.width, image.height))
                                assert np.array_equal(np.asarray(image), np.asarray(expected)), (key, 'restart', reduction)
                        print('Restarted-process pixels verified for every fixture', flush=True)
                    report = {'passed': True, 'measurements': measurements,
                              'rangeLatencyMs': args.range_latency_ms,
                        'rollingWindow': {'heldResponseMs': args.slow_payload_ms,
                            'replacementReadsWhileHeld': len(continued) if args.slow_payload_ms else None,
                            'concurrency': 4 if args.slow_payload_ms else None},
                        'websocket': 'preview pause, cancellation, cross-protocol reuse, and mixed cached/missing batch pixels verified',
                        'crossProtocolReuse': {'route': wmts_headers['X-River-Decoder-Route'], 'decodeMs': float(wmts_headers['X-River-Decode-Ms']),
                            'encodeMs': float(wmts_headers['X-River-Encode-Ms']), 'sourceBytes': int(wmts_headers['X-River-Source-Read-Bytes'])},
                        'coldAnonymousTlmRoi': {'sourceBytes':len(tlm_original),'readBytes':partial_bytes,'pixels':'exact'},
                        'mixedBatch': [{'x': frame['header']['tileX'], 'y': frame['header']['tileY'],
                            'timing': frame['header']['serverTiming']} for frame in finals]}
                    if args.output:
                        Path(args.output).write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
                    print(json.dumps(report, indent=2))
                finally:
                    if child is not None:
                        child.terminate()
                        child.wait(timeout=10)
                    else:
                        try:
                            urlopen(Request(base + '/api/s3/sources/fixture', method='DELETE', headers={'Authorization':'Bearer fixture-admin'}), timeout=5).close()
                        except OSError:
                            pass
    finally:
        s3.shutdown()


if __name__ == '__main__':
    verify()
