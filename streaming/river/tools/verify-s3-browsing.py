"""Exercise lazy S3 loading and bounded provider paging against a local S3 fixture.
Run after cargo build -p river-server. Requires Pillow with JPEG 2000 support.
"""
import io
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import xml.etree.ElementTree as ET
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, urlparse
from urllib.request import Request, urlopen
from urllib.error import HTTPError
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]


def verify():
    raster = Image.new('RGB', (1024, 768), (180, 100, 40))
    buffer = io.BytesIO()
    raster.save(buffer, format='JPEG2000', irreversible=False)
    data = buffer.getvalue()
    high = io.BytesIO()
    Image.new("I;16", (1024, 768), 32768).save(high, format="JPEG2000", irreversible=False)
    high_data = high.getvalue()
    calls = []
    slow_entered, slow_release = threading.Event(), threading.Event()
    admission_full, admission_release = threading.Event(), threading.Event()
    admission_lock = threading.Lock()
    admission = {'active': 0, 'peak': 0}

    class S3(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_HEAD(self):
            calls.append((urlparse(self.path).path, {'method': ['HEAD']}))
            if '/admission-' in self.path:
                with admission_lock:
                    admission['active'] += 1
                    admission['peak'] = max(admission['peak'], admission['active'])
                    if admission['active'] >= 4:
                        admission_full.set()
                try:
                    assert admission_release.wait(10), 'header admission fixture was not released'
                finally:
                    with admission_lock:
                        admission['active'] -= 1
            if self.path.endswith('/slow.jp2'):
                slow_entered.set()
                assert slow_release.wait(10), 'slow HEAD fixture was not released'
            body = high_data if self.path.endswith('high.jp2') else data
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.send_header('ETag', '"fixture"')
            self.send_header('Last-Modified', 'Wed, 14 Oct 2026 00:00:00 GMT')
            self.end_headers()

        def do_GET(self):
            query = parse_qs(urlparse(self.path).query)
            calls.append((urlparse(self.path).path, query))
            if 'list-type' in query:
                assert query['max-keys'][0] in ('50', '100')
                if query.get('continuation-token') == ['next-page']:
                    objects = '<Contents><Key>folder/second.jp2</Key><LastModified>2026-10-14T00:00:00Z</LastModified><ETag>"fixture"</ETag><Size>%d</Size></Contents>' % len(data)
                    continuation = '<IsTruncated>false</IsTruncated>'
                else:
                    objects = '<Contents><Key>folder/image.jp2</Key><LastModified>2026-10-14T00:00:00Z</LastModified><ETag>"fixture"</ETag><Size>%d</Size></Contents>' % len(data)
                    continuation = '<IsTruncated>true</IsTruncated><NextContinuationToken>next-page</NextContinuationToken>'
                body = ('<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">' + objects + continuation + '</ListBucketResult>').encode()
                content_type = 'application/xml'
            else:
                body, content_type = high_data if self.path.endswith('high.jp2') else data, 'application/octet-stream'
            status = 200
            total = len(body)
            if 'list-type' not in query and self.headers.get('Range'):
                start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
                body = body[start:end + 1]
                status = 206
            self.send_response(status)
            if status == 206:
                self.send_header('Content-Range', f'bytes {start}-{end}/{total}')
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(body)))
            self.send_header('ETag', '"fixture"')
            self.send_header('Last-Modified', 'Wed, 14 Oct 2026 00:00:00 GMT')
            self.end_headers()
            self.wfile.write(body)

    s3 = ThreadingHTTPServer(('127.0.0.1', 0), S3)
    threading.Thread(target=s3.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='river-browse-test-') as work:
        # Find an unused loopback port before launching the fixture process.
        import socket
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        env = dict(os.environ, RIVER_BIND=f'127.0.0.1:{port}', RIVER_ADMIN_TOKEN='fixture-admin',
                   RIVER_S3_CACHE_ROOT=work, RIVER_MOSAIC_ROOT=work, RIVER_OTEL_ENABLED='false')
        local_assets=[]
        for name, body, depth in [('local-eight.jp2',data,8),('local-sixteen.jp2',high_data,16)]:
            path=Path(work)/name
            path.write_bytes(body)
            local_assets.append(dict(assetId=hashlib.sha256(body).hexdigest(),width=1024,height=768,
                bitDepth=depth,source=str(path)))
        catalog=Path(work)/'catalog.json'
        catalog.write_text(json.dumps(dict(assets=local_assets)),encoding='utf-8')
        env['RIVER_ASSET_CATALOG']=str(catalog)
        binary = ROOT / 'target/debug' / ('river-server.exe' if os.name == 'nt' else 'river-server')
        with open(Path(work) / 'server.log', 'w') as log:
            child = subprocess.Popen([str(binary)], cwd=ROOT, env=env, stdout=log, stderr=log,
                                     creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            try:
                base = f'http://127.0.0.1:{port}'

                def request(path, body=None, admin=False):
                    headers = {'Content-Type': 'application/json'}
                    if admin:
                        headers['Authorization'] = 'Bearer fixture-admin'
                    with urlopen(Request(base + path, data=json.dumps(body).encode() if body is not None else None,
                                         headers=headers), timeout=20) as response:
                        return response.read(), response.headers

                for _ in range(100):
                    try:
                        request('/health')
                        break
                    except OSError:
                        if child.poll() is not None:
                            raise RuntimeError(Path(work, 'server.log').read_text())
                        time.sleep(.05)
                request('/api/s3/sources', {'id': 'fixture', 'name': 'Fixture', 'endpoint': f'http://127.0.0.1:{s3.server_port}',
                        'bucket': 'jp2', 'prefix': 'folder', 'accessKeyId': 'fixture', 'secretAccessKey': 'fixture',
                        'allowHttp': True}, admin=True)
                assert json.loads(request('/api/s3/sources')[0])[0]['id'] == 'fixture'
                page = json.loads(request('/api/assets?source=fixture&limit=9999')[0])
                assert len(calls) == 1 and calls[0][1]['max-keys'] == ['100']
                assert calls[0][1]['prefix'] == ['folder/']
                assert page['nextCursor'] == 'next-page'
                second = json.loads(request('/api/s3/sources/fixture/objects?cursor=next-page')[0])
                assert len(calls) == 2 and second['objects'][0]['key'] == 'folder/second.jp2'
                assert not second['truncated']
                asset_id = page['objects'][0]['assetId']
                from concurrent.futures import ThreadPoolExecutor
                with ThreadPoolExecutor(max_workers=3) as pool:
                    manifests = list(pool.map(lambda _: request(f'/api/assets/{asset_id}/manifest'), range(3)))
                manifest = json.loads(manifests[0][0])
                assert (manifest['width'], manifest['height'], manifest['bitDepth']) == (1024, 768, 8)
                assert len(calls) == 4, calls
                from concurrent.futures import ThreadPoolExecutor
                with ThreadPoolExecutor(max_workers=3) as pool:
                    list(pool.map(lambda _: request(f'/api/assets/{asset_id}/manifest'), range(3)))
                request('/api/s3/sources/fixture/assets', {'key': 'folder/image.jp2'})
                request('/api/s3/sources/fixture/assets', {'key': 'folder/image.jp2', 'width': 1024, 'height': 768, 'bitDepth': 8})
                for hint, wrong in [('width', 1025), ('height', 769), ('bitDepth', 16)]:
                    try:
                        request('/api/s3/sources/fixture/assets', {'key': 'folder/image.jp2', hint: wrong})
                        raise AssertionError(f'cached load accepted wrong {hint}')
                    except HTTPError as error:
                        assert error.code == 422 and json.loads(error.read())['code'] == 'metadata_mismatch'
                assert len(calls) == 4, 'repeat metadata validation refetched headers'
                ns=dict(w='http://www.opengis.net/wmts/1.0',ows='http://www.opengis.net/ows/1.1')
                caps=second['objects'][0]['assetId']
                xml, _=request('/wmts?'+urlencode(dict(SERVICE='WMTS',REQUEST='GetCapabilities',LAYER=caps)))
                parsed=ET.fromstring(xml)
                assert len(parsed.findall('.//w:Layer',ns))==1
                assert len(parsed.findall('.//w:TileMatrix',ns))==manifest['resolutionLevels']
                assert parsed.find('.//w:Layer/ows:Identifier',ns).text.startswith('s3v-')
                assert sum('list-type' in query for _,query in calls)==2,'capabilities scanned the provider catalog'
                local_page, headers = request('/api/assets?limit=1')
                local_all = json.loads(request('/api/assets?limit=100')[0])
                assert all(not asset['assetId'].startswith(('s3-', 's3v-')) for asset in local_all), 'remote assets leaked into local catalog'
                assert len(json.loads(local_page)) == 1 and headers.get('X-River-Next-Cursor')
                seen=[json.loads(local_page)[0]['assetId']]
                cursor=headers['X-River-Next-Cursor']
                for _ in range(len(local_all)-1):
                    following, page_headers=request('/api/assets?'+urlencode(dict(limit=1,cursor=cursor)))
                    following=json.loads(following)
                    assert len(following)==1
                    seen.append(following[0]['assetId'])
                    cursor=page_headers.get('X-River-Next-Cursor')
                assert cursor is None and len(set(seen))==len(seen)
                assert seen==sorted(asset['assetId'] for asset in local_all)
                for high_cursor in ['s3-',manifest['assetId'],'z','☃']:
                    empty, page_headers=request('/api/assets?'+urlencode(dict(limit=1,cursor=high_cursor)))
                    assert json.loads(empty)==[] and not page_headers.get('X-River-Next-Cursor')
                tile_query = urlencode(dict(SERVICE='WMTS', REQUEST='GetTile', VERSION='1.0.0',
                    LAYER=manifest['assetId'], STYLE='default', FORMAT='image/png',
                    TILEMATRIXSET='RiverPixel-' + manifest['assetId'], TILEMATRIX=str(manifest['resolutionLevels'] - 1), TILEROW='0', TILECOL='0'))
                png, _ = request('/wmts?' + tile_query)
                image = Image.open(io.BytesIO(png)).convert('RGB')
                assert image.getpixel((100, 100)) == (180, 100, 40), image.getpixel((100, 100))
                high_manifest = json.loads(request('/api/s3/sources/fixture/assets', {'key': 'folder/high.jp2'})[0])
                high_manifest = json.loads(request('/api/assets/' + high_manifest['assetId'] + '/manifest')[0])
                assert high_manifest['bitDepth'] == 16
                high_query = urlencode(dict(SERVICE='WMTS', REQUEST='GetTile', VERSION='1.0.0',
                    LAYER=high_manifest['assetId'], STYLE='default', FORMAT='image/png',
                    TILEMATRIXSET='RiverPixel-' + high_manifest['assetId'], TILEMATRIX=str(high_manifest['resolutionLevels'] - 1), TILEROW='0', TILECOL='0'))
                high_png, _ = request('/wmts?' + high_query)
                high_image = Image.open(io.BytesIO(high_png)).convert('RGB')
                assert high_image.getpixel((100, 100)) == (128, 128, 128), high_image.getpixel((100, 100))
                with ThreadPoolExecutor(max_workers=3) as pool:
                    slow = pool.submit(request, '/api/s3/sources/fixture/assets', {'key': 'folder/slow.jp2'})
                    try:
                        assert slow_entered.wait(3), 'slow object did not enter HEAD'
                        warm = pool.submit(request, f'/api/assets/{asset_id}/manifest')
                        independent = pool.submit(request, '/api/s3/sources/fixture/assets', {'key': 'folder/independent.jp2'})
                        assert json.loads(warm.result(timeout=1)[0])['tilesAvailable'] is True
                        assert json.loads(independent.result(timeout=1)[0])['width'] == 1024
                        assert not slow.done(), 'fixture failed to hold the slow open'
                    finally:
                        slow_release.set()
                    assert json.loads(slow.result(timeout=5)[0])['width'] == 1024
                print('PASS: bounded S3 paging, lazy metadata, repeat-load reuse, local cursor and non-black 8/16-bit decoded PNGs')
                print('PASS: a blocked cold HEAD does not block cached or independent object opens')
                with ThreadPoolExecutor(max_workers=8) as pool:
                    opens = [pool.submit(request, '/api/s3/sources/fixture/assets', {'key': f'folder/admission-{i}.jp2'}) for i in range(8)]
                    try:
                        assert admission_full.wait(3), 'independent opens did not overlap'
                        time.sleep(.1)
                        assert admission['peak'] == 4, admission
                    finally:
                        admission_release.set()
                    assert all(json.loads(opened.result(timeout=5)[0])['width'] == 1024 for opened in opens)
                assert admission['peak'] == 4, 'header opens exceeded shared IO admission'
                print('PASS: eight distinct opens overlap within the four-request IO budget')
                before = len(calls)
                search = json.loads(request('/api/assets?' + urlencode(dict(source='fixture', keyPrefix='im')))[0])
                assert len(calls) == before + 1 and calls[-1][1]['prefix'] == ['folder/im']
                assert search['objects'][0]['key'] == 'folder/image.jp2'
                before = len(calls)
                search = json.loads(request('/api/assets?' + urlencode(dict(source='fixture', q='SECOND')))[0])
                assert search['objects'] == [] and search['nextCursor'] == 'next-page'
                assert len(calls) == before + 1, 'search silently scanned extra provider pages'
                search = json.loads(request('/api/assets?' + urlencode(dict(source='fixture', q='SECOND', cursor=search['nextCursor'])))[0])
                assert search['objects'][0]['key'] == 'folder/second.jp2' and not search['nextCursor']
                assert len(calls) == before + 2 and all('list-type' in query for _, query in calls[before:])
                before = len(calls)
                html, _ = request('/')
                script, _ = request('/viewer.js')
                assert b'catalog-search-mode' in html and b'keyPrefix' in script
                assert len(calls) == before, 'serving viewer queried S3'
                print('PASS: server key-prefix and paged substring search, empty-page continuation, and bundled viewer')
            finally:
                child.terminate()
                child.wait(timeout=10)
    s3.shutdown()


if __name__ == '__main__':
    verify()
