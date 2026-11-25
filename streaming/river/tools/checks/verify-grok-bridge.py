"""Verify the native Grok ABI against independent Pillow/OpenJPEG samples."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
from PIL import Image

CHECK = r'''
import ctypes as C, json, pathlib
root = pathlib.Path('/tmp/river-grok-bridge-check')
lib = C.CDLL('/usr/local/lib/libriver_grok.so')
class Plane(C.Structure):
    _fields_ = [('data', C.c_void_p)] + [(n, C.c_uint32) for n in ('width','height','stride','x','y')] + [(n,C.c_uint8) for n in ('precision','signed_samples','sample_bytes')]
lib.river_grok_initialize.argtypes = [C.c_uint32]
lib.river_grok_decode.argtypes = [C.c_char_p,C.c_void_p,C.c_size_t] + [C.c_uint32]*4 + [C.c_uint8,C.c_uint16]
lib.river_grok_decode.restype = C.c_void_p
lib.river_grok_plane.argtypes = [C.c_void_p,C.c_uint16,C.POINTER(Plane)]
lib.river_grok_plane.restype = C.c_bool
lib.river_grok_components.argtypes = [C.c_void_p]
lib.river_grok_components.restype = C.c_uint16
lib.river_grok_release.argtypes = [C.c_void_p]
lib.river_grok_initialize(2)
for case in json.loads((root/'cases.json').read_text()):
    body = C.create_string_buffer((root/case['file']).read_bytes())
    for memory in (False,True):
        handle = lib.river_grok_decode(None if memory else str(root/case['file']).encode(), body if memory else None, len(body)-1 if memory else 0, *case['region'],case['reduction'],0)
        assert handle, case
        try:
            assert lib.river_grok_components(handle) == len(case['planes'])
            for channel, expected in enumerate(case['planes']):
                plane=Plane()
                assert lib.river_grok_plane(handle,channel,C.byref(plane))
                assert (plane.width,plane.height)==tuple(case['size']), (case,plane.width,plane.height)
                assert plane.signed_samples==0 and plane.sample_bytes in (2,4)
                sample=C.c_int16 if plane.sample_bytes==2 else C.c_int32
                pixels=C.cast(plane.data,C.POINTER(sample))
                actual=[pixels[y*plane.stride+x] for y in range(plane.height) for x in range(plane.width)]
                assert actual==expected, (case['file'],channel,memory,actual[:8],expected[:8])
        finally:
            lib.river_grok_release(handle)
    print('PASS:',case['file'],case['region'],case['reduction'],'file/memory exact samples')
from concurrent.futures import ThreadPoolExecutor
def concurrent_decode(_):
    body=C.create_string_buffer((root/'rgb.jp2').read_bytes())
    handle=lib.river_grok_decode(None,body,len(body)-1,0,0,128,96,0,0)
    assert handle
    try:
        plane=Plane()
        assert lib.river_grok_plane(handle,0,C.byref(plane))
        sample=C.c_int16 if plane.sample_bytes==2 else C.c_int32
        assert C.cast(plane.data,C.POINTER(sample))[0]==37
    finally: lib.river_grok_release(handle)
with ThreadPoolExecutor(max_workers=16) as pool:
    list(pool.map(concurrent_decode,range(64)))
bad=C.create_string_buffer(b'not a JPEG 2000 stream')
assert not lib.river_grok_decode(None,bad,len(bad)-1,0,0,128,96,0,0)
print('PASS: 64 decodes across 16 callers; independent handles; malformed stream rejected')
'''

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--container',required=True)
    args=parser.parse_args()
    assert args.container.startswith('river-goal-')
    with tempfile.TemporaryDirectory(prefix='river-grok-bridge-') as work:
        root=Path(work)
        cases=[]
        for name, image in [('rgb',Image.new('RGB',(128,96),(37,129,241))),('gray16',Image.new('I;16',(128,96),32768))]:
            image.save(root/(name+'.jp2'),format='JPEG2000',irreversible=False)
            for region,reduction in [([0,0,128,96],0),([0,0,128,96],2),([32,16,96,80],0)]:
                expected=Image.open(root/(name+'.jp2'))
                expected.reduce=reduction
                expected.load()
                if region != [0,0,128,96]: expected=expected.crop(tuple(region))
                values=list(expected.getdata())
                planes=[[v[channel] for v in values] for channel in range(3)] if name=='rgb' else [values]
                cases.append(dict(file=name+'.jp2',region=region,reduction=reduction,size=list(expected.size),planes=planes))
        (root/'cases.json').write_text(json.dumps(cases),encoding='utf-8')
        (root/'check.py').write_text(CHECK,encoding='utf-8')
        subprocess.run(['docker','cp',str(root),args.container+':/tmp/river-grok-bridge-check'],check=True)
        subprocess.run(['docker','exec',args.container,'python3','/tmp/river-grok-bridge-check/check.py'],check=True)

if __name__=='__main__': main()
