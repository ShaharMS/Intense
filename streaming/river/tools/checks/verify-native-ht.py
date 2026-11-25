"""Verify existing HT-coded input delivery using new synthetic fixtures only.

Uses a task-owned River image's Grok encoder for matched legacy/HT codestreams.
The independent Pillow/OpenJPEG legacy decode is the reduced/full pixel oracle.
No served corpus images are read, modified or transcoded by this tool.
"""
import argparse
import io
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, urlparse
from urllib.error import HTTPError
from urllib.request import Request, urlopen

import numpy as np
from PIL import Image

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--api', required=True)
    parser.add_argument('--container', required=True)
    parser.add_argument('--driver', default='jp2grok')
    parser.add_argument('--verify-capabilities', action='store_true')
    parser.add_argument('--unindexed', action='store_true', help='Encode fixtures without PLT/TLM and verify inline HT packet prefixes')
    parser.add_argument('--progression', choices=['LRCP','RLCP','RPCL'], default='RPCL')
    parser.add_argument('--allow-whole-preview', action='store_true', help='Measure an older fallback baseline without the preview transfer gate')
    parser.add_argument('--output')
    parser.add_argument('--range-latency-ms',type=float,default=0)
    parser.add_argument('--hold-seconds', type=int, default=0, help='Keep synthetic source alive briefly for viewer checks')
    args = parser.parse_args()
    if not args.container.startswith('river-goal-'):
        parser.error('use a task-owned river-goal-* fixture container')
    if not 0 <= args.hold_seconds <= 180:
        parser.error('--hold-seconds must be between 0 and 180')
    if not 0 <= args.range_latency_ms <= 1000: parser.error('invalid range latency')
    objects, legacy, events, measurements = {}, {}, [], []
    rng = np.random.default_rng(8054)
    with tempfile.TemporaryDirectory(prefix='river-ht-fixtures-') as work:
        remote = '/tmp/river-ht-fixtures'
        subprocess.run(['docker', 'exec', args.container, 'mkdir', '-p', remote], check=True)
        for name, pixels in [('rgb8', rng.integers(0, 256, (768, 1024, 3), dtype=np.uint8)),
                             ('gray16', rng.integers(0, 65536, (768, 1024), dtype=np.uint16))]:
            image = Path(work) / (name + '.png')
            Image.fromarray(pixels).save(image)
            subprocess.run(['docker', 'cp', str(image), args.container + ':' + remote + '/' + image.name], check=True)
            for mode, flag in [('legacy', '0'), ('ht', '64')]:
                out = name + '-' + mode + ('.jph' if mode == 'ht' else '.jp2')
                command = ['docker', 'exec', args.container, 'grk_compress', '-i', remote + '/' + image.name,
                           '-o', remote + '/' + out, '-n', '7', '-t', '256,256', '-Y', '0', '-M', flag,
                           '-H', '1', '-p', args.progression]
                if not args.unindexed: command += ['-L','-X']
                if mode == 'ht':
                    command += ['-Z', '16384']
                result = subprocess.run(command, capture_output=True)
                if result.returncode:
                    raise RuntimeError((result.stdout + result.stderr).decode(errors='replace'))
                path = Path(work) / out
                subprocess.run(['docker', 'cp', args.container + ':' + remote + '/' + out, str(path)], check=True)
                (objects if mode == 'ht' else legacy)[name + '.jph'] = path.read_bytes()
        for key, data in list(objects.items()):
            offset = 0
            while offset < len(data):
                length = int.from_bytes(data[offset:offset + 4], 'big')
                header_size = 8
                if length == 1:
                    length = int.from_bytes(data[offset + 8:offset + 16], 'big')
                    header_size = 16
                if length == 0:
                    length = len(data) - offset
                assert length >= header_size and offset + length <= len(data)
                if data[offset + 4:offset + 8] == b'jp2c':
                    raw_key = key.removesuffix('.jph') + '.jhc'
                    objects[raw_key] = data[offset + header_size:offset + length]
                    legacy[raw_key] = legacy[key]
                    break
                offset += length
            else:
                raise AssertionError('HT fixture lacks codestream box')
        class S3(BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def setup(self):
                super().setup()
                self.connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            def log_message(self, *_): pass
            def key(self): return urlparse(self.path).path.rsplit('/', 1)[-1]
            def do_HEAD(self):
                self.send_response(200)
                self.send_header('Content-Length', str(len(objects[self.key()])))
                self.send_header('ETag', '"ht-fixture-v1"')
                self.send_header('Last-Modified','Wed, 14 Oct 2026 00:00:00 GMT')
                self.end_headers()
            def do_GET(self):
                query = parse_qs(urlparse(self.path).query)
                if query.get('list-type') == ['2']:
                    limit = int(query['max-keys'][0])
                    assert 1 <= limit <= 100, 'HT catalog requested an oversized page'
                    first = int(query.get('continuation-token', ['0'])[0])
                    keys = sorted(objects)[first:first + limit]
                    next_index = first + len(keys)
                    truncated = next_index < len(objects)
                    entries = ''.join(f'<Contents><Key>{key}</Key><LastModified>2026-10-14T00:00:00Z</LastModified>'
                        f'<ETag>"ht-fixture-v1"</ETag><Size>{len(objects[key])}</Size></Contents>' for key in keys)
                    token = f'<NextContinuationToken>{next_index}</NextContinuationToken>' if truncated else ''
                    body = (f'<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
                            f'<Name>jp2</Name><IsTruncated>{str(truncated).lower()}</IsTruncated>{entries}{token}</ListBucketResult>').encode()
                    self.send_response(200)
                    self.send_header('Content-Length', str(len(body)))
                    self.send_header('Content-Type', 'application/xml')
                    self.end_headers()
                    self.wfile.write(body)
                    return
                data = objects[self.key()]
                if args.range_latency_ms: time.sleep(args.range_latency_ms/1000)
                start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
                # The bounded opening GET discovers the actual version itself.
                if self.headers.get('If-Match') is None:
                    assert start==0 and end==min(1023,len(data)-1),'unbounded initial version probe'
                else:assert self.headers['If-Match']=='"ht-fixture-v1"'
                assert 0 <= start <= end < len(data) and end - start + 1 <= 1024 * 1024
                body = data[start:end + 1]
                events.append(len(body))
                self.send_response(206)
                self.send_header('Content-Length', str(len(body)))
                self.send_header('Content-Range', f'bytes {start}-{end}/{len(data)}')
                self.send_header('ETag', '"ht-fixture-v1"')
                self.send_header('Last-Modified','Wed, 14 Oct 2026 00:00:00 GMT')
                self.end_headers()
                self.wfile.write(body)
        server = ThreadingHTTPServer(('127.0.0.1', 0), S3)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        source = 'ht-' + str(server.server_port)
        def request(path, body=None, method=None):
            with urlopen(Request(args.api + path, data=json.dumps(body).encode() if body is not None else None,
                headers={'Content-Type': 'application/json', 'Authorization': 'Bearer fixture-admin'}, method=method), timeout=30) as response:
                return response.read()
        try:
            request('/api/s3/sources', {'id': source, 'name': source, 'endpoint': f'http://host.docker.internal:{server.server_port}',
                'bucket': 'jp2', 'accessKeyId': 'fixture', 'secretAccessKey': 'fixture', 'allowHttp': True})
            available = {}
            cursor = None
            while True:
                params = {'source': source, 'limit': 2}
                if cursor:
                    params['cursor'] = cursor
                page = json.loads(request('/api/assets?' + urlencode(params)))
                assert len(page['objects']) <= 2
                available.update({entry['key']: entry['assetId'] for entry in page['objects']})
                cursor = page.get('nextCursor')
                if not cursor:
                    break
            assert set(available) == set(objects), 'HT source contents were not discoverable through paging'
            for key in objects:
                meta = json.loads(request('/api/assets/' + available[key] + '/manifest'))
                if args.verify_capabilities:
                    assert meta['tilesAvailable'] is True
                    assert meta['codestreamCoding'] == 'htj2k'
                    expected=['jp2grok','nvjpeg2000']
                    if any(d['id']=='openhtj2k' for d in json.loads(request('/api/drivers'))):expected.insert(1,'openhtj2k')
                    assert meta['supportedDecoderDrivers']==expected,(meta['supportedDecoderDrivers'],expected)
                    capabilities = request('/wmts?' + urlencode(dict(SERVICE='WMTS',REQUEST='GetCapabilities',LAYER=available[key])))
                    assert b'DRIVER=openjpeg' not in capabilities, 'HT capabilities advertised an incompatible decoder'
                    assert b'DRIVER=jp2grok' in capabilities
                    assert capabilities.count(b'<TileMatrix>') == meta['resolutionLevels']
                for reduction, x in [(4, 0), (2, 0), (0, 1)]:
                    marker = len(events)
                    query = urlencode(dict(SERVICE='WMTS', REQUEST='GetTile', VERSION='1.0.0', LAYER=meta['assetId'],
                        STYLE='default', FORMAT='image/png', TILEMATRIXSET='RiverPixel-' + meta['assetId'],
                        TILEMATRIX=str(meta['resolutionLevels'] - 1 - reduction), TILEROW='0', TILECOL=str(x), DRIVER=args.driver))
                    started = time.perf_counter()
                    actual = Image.open(io.BytesIO(request('/wmts?' + query))).convert('RGBA')
                    elapsed_ms = (time.perf_counter()-started)*1000
                    expected = Image.open(io.BytesIO(legacy[key]))
                    expected.reduce = reduction
                    expected.load()
                    if expected.mode in ('I;16', 'I'):
                        values = ((np.asarray(expected).astype(np.uint64) * 255 + 32767) // 65535).astype(np.uint8)
                        expected = Image.fromarray(values)
                    expected = expected.convert('RGBA').crop((512 * x, 0, 512 * x + actual.width, actual.height))
                    assert np.array_equal(np.asarray(actual), np.asarray(expected)), (key, reduction, 'native HT pixels')
                    fetched = sum(events[marker:])
                    if reduction == 4 and not args.allow_whole_preview:
                        assert fetched < len(objects[key]) // 4, 'HT preview fetched excess detail'
                    measurement = {'key': key, 'driver': args.driver, 'reduction': reduction, 'sourceBytes': len(objects[key]),
                        'fetchedBytes': fetched, 'ranges': len(events)-marker, 'elapsedMs': elapsed_ms, 'pixels': 'exact'}
                    measurements.append(measurement)
                    print(json.dumps(measurement), flush=True)
                if args.verify_capabilities:
                    marker = len(events)
                    try:
                        request('/wmts?' + query.replace('DRIVER=' + args.driver, 'DRIVER=openjpeg'))
                    except HTTPError as error:
                        assert error.code >= 400 and b'HTJ2K input needs' in error.read()
                    else:
                        raise AssertionError('OpenJPEG was advertised as usable for native HT input')
                    assert len(events) == marker, 'unsupported HT decoder fetched source payload'
            if args.hold_seconds:
                print(json.dumps({'viewerSource': source, 'holdSeconds': args.hold_seconds}), flush=True)
                time.sleep(args.hold_seconds)
            if args.output:
                    Path(args.output).write_text(json.dumps({'passed':True,'unindexed':args.unindexed,'rangeLatencyMs':args.range_latency_ms,
                    'progression':args.progression,'measurements':measurements},indent=2)+'\n',encoding='utf-8')
        finally:
            request('/api/s3/sources/' + source, method='DELETE')
            server.shutdown()

if __name__ == '__main__':
    main()
