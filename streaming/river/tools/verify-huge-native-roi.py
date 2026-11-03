"""Verify small requests avoid giant native-tile traversal, with exact pixels."""
import hashlib
from concurrent.futures import ThreadPoolExecutor
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from urllib.parse import urlencode
from urllib.request import urlopen
from PIL import Image

root=Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='river-huge-native-') as work:
    work=Path(work)
    source=work/'huge-native.jp2'
    raster=Image.new('RGB',(4096,8192),(37,129,241))
    raster.save(source,format='JPEG2000',tile_size=(4096,4096),irreversible=False,num_resolutions=6)
    raster.close()
    asset=hashlib.sha256(source.read_bytes()).hexdigest()
    catalog=work/'catalog.json'
    catalog.write_text(json.dumps(dict(assets=[dict(assetId=asset,width=4096,height=8192,bitDepth=8,
        source=str(source),nativeTileWidth=4096,nativeTileHeight=4096)])),encoding='utf-8')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
    env=dict(os.environ,RIVER_BIND=f'127.0.0.1:{port}',RIVER_ASSET_CATALOG=str(catalog),
        RIVER_MOSAIC_ROOT=str(work),RIVER_RASTER_CACHE_BYTES='0',RIVER_OTEL_ENABLED='false')
    binary=root/'target/debug'/('river-server.exe' if os.name=='nt' else 'river-server')
    with (work/'server.log').open('w',encoding='utf-8') as log:
        child=subprocess.Popen([str(binary)],cwd=root,env=env,stdout=log,stderr=log,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
        try:
            for _ in range(100):
                try:
                    with urlopen(f'http://127.0.0.1:{port}/health',timeout=1): break
                except OSError:
                    if child.poll() is not None: raise RuntimeError((work/'server.log').read_text(encoding='utf-8'))
                    time.sleep(.05)
            def manifest(_):
                with urlopen(f'http://127.0.0.1:{port}/api/assets/{asset}/manifest') as response:
                    return json.load(response)
            with ThreadPoolExecutor(max_workers=16) as pool:
                manifests=list(pool.map(manifest,range(16)))
            assert all(m['resolutionLevels']==6 and len(m['quality']['cumulativeLayerBytes'])==1 for m in manifests)
            matrix=manifests[0]['resolutionLevels']-1
            overview=urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,
                STYLE='default',FORMAT='image/png',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX='0',
                TILEROW=0,TILECOL=0,DRIVER='openjpeg'))
            with urlopen(f'http://127.0.0.1:{port}/wmts?'+overview,timeout=30) as response:
                image=Image.open(io.BytesIO(response.read())).convert('RGBA')
                assert image.size==(128,256) and image.tobytes()==bytes([37,129,241,255])*(128*256)
            for col,row in [(0,0),(7,0),(0,8)]:
                query=urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,
                    STYLE='default',FORMAT='image/png',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX=str(matrix),
                    TILEROW=row,TILECOL=col,DRIVER='openjpeg'))
                with urlopen(f'http://127.0.0.1:{port}/wmts?'+query,timeout=30) as response:
                    assert response.headers['X-River-Decoder-Route']=='roi'
                    actual=Image.open(io.BytesIO(response.read())).convert('RGBA')
                assert actual.size==(512,512)
                assert actual.tobytes()==bytes([37,129,241,255])*(512*512)
            print('PASS: three 512x512 requests on 4096x4096 native tiles use ROI and match every RGBA pixel')
            print('PASS: concurrent manifests recover six real levels; coarsest preview and quality-layer count are correct')
        finally:
            child.terminate(); child.wait(timeout=10)
