"""Concurrent cold WMTS decode count and exact encoded-output reuse.

Run on a fresh task-owned image instance; do not prewarm the selected tile.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import time
from urllib.parse import urlencode
from urllib.request import urlopen

parser = argparse.ArgumentParser()
parser.add_argument('--api', required=True)
parser.add_argument('--expected-cold-decodes', type=int, default=1)
args = parser.parse_args()
asset = '94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290'
query = urlencode(dict(SERVICE='WMTS', REQUEST='GetTile', VERSION='1.0.0', LAYER=asset,
    STYLE='default', FORMAT='image/png', TILEMATRIXSET='RiverPixel-' + asset, TILEMATRIX='5',
    TILEROW='0', TILECOL='0', DRIVER='openjpeg'))
def one(_):
    started = time.perf_counter()
    with urlopen(args.api + '/wmts?' + query, timeout=20) as response:
        body = response.read()
        assert response.status == 200 and body.startswith(b'\x89PNG')
        return {'hash': hashlib.sha256(body).hexdigest(), 'route': response.headers['X-River-Decoder-Route'],
                'elapsedMs': (time.perf_counter() - started) * 1000}
results = []
for state in ('cold', 'repeat'):
    with ThreadPoolExecutor(max_workers=16) as pool:
        samples = list(pool.map(one, range(16)))
    decodes = sum(sample['route'] != 'encoded-raster-cache' for sample in samples)
    result = {'state': state, 'clients': 16, 'decodeResponses': decodes,
              'p95Ms': sorted(sample['elapsedMs'] for sample in samples)[-1]}
    results.extend(samples)
    print(json.dumps(result), flush=True)
    assert decodes == (args.expected_cold_decodes if state == 'cold' else 0), result
assert len({sample['hash'] for sample in results}) == 1, 'coalesced/repeated tile bytes differ'
