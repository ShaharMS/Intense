"""Verify exact view bytes and bounded pread reuse without requiring a GPU."""
import importlib.util,os,random,tempfile,io
from pathlib import Path
from unittest.mock import patch
from PIL import Image
root=Path(__file__).resolve().parents[1]
def load(path,name):
    spec=importlib.util.spec_from_file_location(name,path);module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
candidate=load(root/'nvjpeg2000-sidecar/resolution_view.py','candidate_view')
native=getattr(os,'pread',None)
def extract(module,path):
    reads=[]
    def pread(fd,count,offset):
        if native is not None:data=native(fd,count,offset)
        else:
            position=os.lseek(fd,0,os.SEEK_CUR)
            try:os.lseek(fd,offset,os.SEEK_SET);data=os.read(fd,count)
            finally:os.lseek(fd,position,os.SEEK_SET)
        reads.append(len(data));return data
    with patch.object(os,'pread',pread,create=True):result=module.extract(path,3)
    assert result is not None,path
    return result,reads
with tempfile.TemporaryDirectory(prefix='river-prefix-view-') as work:
    for edge,tile in [(128,64),(512,128)]:
        path=Path(work)/f'{edge}.jp2'
        pixels=random.Random(42).randbytes(edge*edge*3)
        Image.frombytes('RGB',(edge,edge),pixels).save(path,format='JPEG2000',irreversible=False,
            num_resolutions=4,tile_size=(tile,tile),codeblock_size=(64,64),precinct_size=(128,128),progression='RPCL',mct=0)
        new,new_reads=extract(candidate,path)
        original=Image.open(path);original.reduce=3;original.load()
        reconstructed=Image.open(io.BytesIO(new['data']));reconstructed.load()
        assert reconstructed.size==original.size
        assert reconstructed.convert('RGB').tobytes()==original.convert('RGB').tobytes()
        assert new['readBytes']==sum(new_reads)
        if path.stat().st_size<=65536:assert len(new_reads)==1
        else:assert len(new_reads)>1
        print(f'PASS: {edge}px fixture, {len(new_reads)} pread calls; {sum(new_reads)} bytes; pixels match independent original-source decoding')
    small=Path(work)/'128.jp2';version=small.stat();changed=False
    def changed_pread(fd,count,offset):
        global changed
        if native is not None:data=native(fd,count,offset)
        else:os.lseek(fd,offset,os.SEEK_SET);data=os.read(fd,count)
        if not changed:
            changed=True;os.utime(small,ns=(version.st_atime_ns,version.st_mtime_ns+1000000000))
        return data
    with patch.object(os,'pread',changed_pread,create=True):assert candidate.extract(small,3) is None
    print('PASS: buffered reads still reject source mutation during extraction')
