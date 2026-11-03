"""Discovery/version identities, lazy metadata and exact rendering without registration."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET
from urllib.parse import urlencode
from urllib.request import urlopen
from PIL import Image

root=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser()
parser.add_argument('--shifted-origin',action='store_true')
args=parser.parse_args()
with tempfile.TemporaryDirectory(prefix='river-local-discovery-') as work:
    work=Path(work)
    source=work/'discovered.jp2'
    options=dict(offset=(13,29),tile_size=(512,512)) if args.shifted_origin else {}
    Image.new('RGB',(512,384),(37,129,241)).save(source,format='JPEG2000',irreversible=False,num_resolutions=4,**options)
    # A valid trailing free box makes full-file hashing avoidable work.
    padding=32*1024*1024
    with source.open('ab') as file:
        file.write(struct.pack('>I4s',padding,b'free'))
        file.truncate(file.tell()+padding-8)
    Path(str(source)+'.info.json').write_text(json.dumps(dict(size=[512,384])),encoding='utf-8')
    previous=None
    for mode in ('version','version','sha256'):
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
        env=dict(os.environ,RIVER_BIND=f'127.0.0.1:{port}',RIVER_MOSAIC_ROOT=str(work),
            RIVER_LOCAL_IDENTITY=mode,RIVER_OTEL_ENABLED='false')
        env.pop('RIVER_ASSET_CATALOG',None)
        binary=root/'target/debug'/('river-server.exe' if os.name=='nt' else 'river-server')
        with (work/'server.log').open('w',encoding='utf-8') as log:
            child=subprocess.Popen([str(binary)],cwd=root,env=env,stdout=log,stderr=log,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
            try:
                base=f'http://127.0.0.1:{port}'
                selected=None
                for _ in range(200):
                    try:
                        with urlopen(base+'/api/assets?limit=50',timeout=1) as response:
                            selected=next((asset for asset in json.load(response) if asset['name']=='discovered.jp2'),None)
                        if selected: break
                    except OSError: pass
                    if child.poll() is not None: raise RuntimeError((work/'server.log').read_text(encoding='utf-8'))
                    time.sleep(.05)
                assert selected,'image was not discovered'
                asset=selected['assetId']
                if mode=='version':
                    assert asset.startswith('filev-')
                    if previous: assert asset==previous,'identity changed across unchanged-file restart'
                    previous=asset
                else:
                    digest=hashlib.sha256()
                    with source.open('rb') as file:
                        for block in iter(lambda:file.read(1024*1024),b''): digest.update(block)
                    assert asset==digest.hexdigest(),'compatibility identity is not the content digest'
                ns=dict(w='http://www.opengis.net/wmts/1.0',ows='http://www.opengis.net/ows/1.1')
                with urlopen(base+'/wmts?'+urlencode(dict(SERVICE='WMTS',REQUEST='GetCapabilities',LAYER=asset))) as response:
                    capabilities=ET.fromstring(response.read())
                assert len(capabilities.findall('.//w:Layer',ns))==1
                assert len(capabilities.findall('.//w:TileMatrix',ns))==4,'capabilities advertised a nonexistent resolution'
                seen=[]; cursor=None
                for _ in range(10):
                    params=dict(SERVICE='WMTS',REQUEST='GetCapabilities',LIMIT=1)
                    if cursor: params['CURSOR']=cursor
                    with urlopen(base+'/wmts?'+urlencode(params)) as response:
                        page=ET.fromstring(response.read()); cursor=response.headers.get('X-River-Next-Cursor')
                    ids=[node.text for node in page.findall('.//w:Layer/ows:Identifier',ns)]
                    assert len(ids)<=1
                    seen.extend(ids)
                    if not cursor: break
                assert asset in seen and len(seen)==len(set(seen))
                with urlopen(base+'/api/assets/'+asset+'/manifest') as response: manifest=json.load(response)
                assert (manifest['width'],manifest['height'],manifest['bitDepth'],manifest['resolutionLevels'])==(512,384,8,4)
                assert manifest['referenceOrigin']==(dict(x=13,y=29) if args.shifted_origin else dict(x=0,y=0))
                if args.shifted_origin: assert 'nvjpeg2000' not in manifest['supportedDecoderDrivers']
                if mode=='version': assert manifest['identityKind']=='file-version'
                query=urlencode(dict(SERVICE='WMTS',REQUEST='GetTile',VERSION='1.0.0',LAYER=asset,
                    STYLE='default',FORMAT='image/png',TILEMATRIXSET='RiverPixel-'+asset,TILEMATRIX='3',
                    TILEROW=0,TILECOL=0,DRIVER='openjpeg'))
                with urlopen(base+'/wmts?'+query,timeout=10) as response:
                    image=Image.open(io.BytesIO(response.read())).convert('RGBA')
                assert image.size==(512,384) and image.tobytes()==bytes([37,129,241,255])*(512*384)
                reduced_query=query.replace('TILEMATRIX=3','TILEMATRIX=1')
                with urlopen(base+'/wmts?'+reduced_query,timeout=10) as response:
                    reduced=Image.open(io.BytesIO(response.read())).convert('RGBA')
                assert reduced.size==(128,96) and reduced.tobytes()==bytes([37,129,241,255])*(128*96)
                print('PASS:',mode,'discovery, identity, actual metadata and exact WMTS pixels')
            finally:
                child.terminate(); child.wait(timeout=10)
