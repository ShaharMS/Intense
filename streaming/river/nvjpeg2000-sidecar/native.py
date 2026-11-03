"""Bounded nvJPEG2000 contexts with native wavelet reduction (no image resize).

ABI declarations follow the nvjpeg2k.h installed by the pinned NVIDIA wheel.
Each leased context owns its decode state, parsed stream, and CUDA stream:
those states may not be shared by concurrent requests (NVIDIA thread safety).
"""
import ctypes as C
import glob
import mmap
import os
import time
from functools import lru_cache
from collections import OrderedDict

import numpy as np
import resolution_view

@lru_cache(maxsize=16)
def sample_lut(precision):
    maximum=(1 << precision)-1
    values=((np.arange(maximum+1,dtype=np.uint32)*255+maximum//2)//maximum).astype(np.uint8)
    values.flags.writeable=False
    return values


class ImageInfo(C.Structure):
    _fields_ = [(name, C.c_uint32) for name in (
        'width', 'height', 'tile_width', 'tile_height', 'columns', 'rows', 'components')]


class ComponentInfo(C.Structure):
    _fields_ = [('width', C.c_uint32), ('height', C.c_uint32),
                ('precision', C.c_uint8), ('signed', C.c_uint8)]


class Image(C.Structure):
    _fields_ = [('pixels', C.POINTER(C.c_void_p)), ('pitches', C.POINTER(C.c_size_t)),
                ('pixel_type', C.c_int), ('components', C.c_uint32)]


def library(pattern):
    paths = glob.glob(pattern)
    if not paths:
        raise RuntimeError('NVIDIA library not found: ' + pattern)
    return C.CDLL(paths[0])


def bind(lib, name, args):
    fn = getattr(lib, name)
    fn.argtypes, fn.restype = args, C.c_int
    return fn


def check(status, operation):
    if status:
        raise RuntimeError(f'{operation} returned status {status}')


P, U, Z = C.c_void_p, C.c_uint32, C.c_size_t
MallocV2=C.CFUNCTYPE(C.c_int,P,C.POINTER(P),Z,P)
FreeV2=C.CFUNCTYPE(C.c_int,P,P,Z,P)
class AllocatorV2(C.Structure):
    _fields_=[('allocate',MallocV2),('free',FreeV2),('context',P)]
nv = library(os.environ.get('RIVER_NVJPEG2000_LIBRARY', '/usr/local/lib/python*/dist-packages/nvidia/nvjpeg2k/lib/libnvjpeg2k.so*'))
cu = library(os.environ.get('RIVER_CUDART_LIBRARY', '/usr/local/cuda*/targets/x86_64-linux/lib/libcudart.so.*'))
for name, args in {
    'nvjpeg2kCreateSimple': [C.POINTER(P)], 'nvjpeg2kDestroy': [P],
    'nvjpeg2kCreateV2':[C.c_int,C.POINTER(AllocatorV2),C.POINTER(AllocatorV2),C.POINTER(P)],
    'nvjpeg2kDecodeStateCreate': [P, C.POINTER(P)], 'nvjpeg2kDecodeStateDestroy': [P],
    'nvjpeg2kStreamCreate': [C.POINTER(P)], 'nvjpeg2kStreamDestroy': [P],
    'nvjpeg2kStreamParse': [P, P, Z, C.c_int, C.c_int, P],
    'nvjpeg2kStreamGetImageInfo': [P, C.POINTER(ImageInfo)],
    'nvjpeg2kStreamGetImageComponentInfo': [P, C.POINTER(ComponentInfo), U],
    'nvjpeg2kStreamGetResolutionsInTile': [P, U, C.POINTER(U)],
    'nvjpeg2kDecodeParamsCreate': [C.POINTER(P)], 'nvjpeg2kDecodeParamsDestroy': [P],
    'nvjpeg2kDecodeParamsSetRGBOutput': [P, C.c_int],
    'nvjpeg2kDecodeParamsSetDecodeArea': [P, U, U, U, U],
    'nvjpeg2kDecodeTile': [P, P, P, P, U, U, C.POINTER(Image), P],
    'nvjpeg2kDecodeImage': [P, P, P, P, C.POINTER(Image), P],
}.items():
    bind(nv, name, args)
for name, args in {
    'cudaSetDevice': [C.c_int], 'cudaStreamCreateWithFlags': [C.POINTER(P), U],
    'cudaSetDeviceFlags':[U],
    'cudaStreamSynchronize': [P], 'cudaStreamDestroy': [P],
    'cudaMalloc': [C.POINTER(P), Z], 'cudaFree': [P],
    'cudaHostAlloc': [C.POINTER(P), Z, U], 'cudaFreeHost': [P],
    'cudaMemcpyAsync': [P, P, Z, C.c_int, P],
    'cudaMallocAsync':[C.POINTER(P),Z,P], 'cudaFreeAsync':[P,P],
    'cudaDeviceGetDefaultMemPool':[C.POINTER(P),C.c_int],
    'cudaMemPoolSetAttribute':[P,C.c_int,P],
}.items():
    bind(cu, name, args)

if os.environ.get('RIVER_NVJPEG2000_SYNC_MODE','auto')=='blocking':
    check(cu.cudaSetDeviceFlags(4),'blocking CUDA synchronization')


class Decoder:
    def __init__(self):
        self.device = int(os.environ.get('RIVER_NVJPEG2000_DEVICE', '0'))
        check(cu.cudaSetDevice(self.device), 'cudaSetDevice')
        self.handle, self.state, self.stream, self.params, self.cuda = (P() for _ in range(5))
        self.mapping = self.buffer = None
        self.identity = None
        self.sources=OrderedDict()
        self.gpu = P()
        self.host = P()
        self.capacity = 0
        self.allocator_mode=os.environ.get('RIVER_NVJPEG2000_ASYNC_ALLOCATOR','false')=='true'
        if self.allocator_mode:
            pool=P();check(cu.cudaDeviceGetDefaultMemPool(C.byref(pool),self.device),'device memory pool')
            threshold=C.c_uint64(536870912)
            check(cu.cudaMemPoolSetAttribute(pool,4,C.byref(threshold)),'pool release threshold')
            self.callbacks=(MallocV2(lambda ctx,ptr,size,stream:cu.cudaMallocAsync(ptr,size,stream)),
                            FreeV2(lambda ctx,ptr,size,stream:cu.cudaFreeAsync(ptr,stream)),
                            MallocV2(lambda ctx,ptr,size,stream:cu.cudaHostAlloc(ptr,size,0)),
                            FreeV2(lambda ctx,ptr,size,stream:cu.cudaFreeHost(ptr)))
            self.device_allocator=AllocatorV2(self.callbacks[0],self.callbacks[1],None)
            self.pinned_allocator=AllocatorV2(self.callbacks[2],self.callbacks[3],None)
            check(nv.nvjpeg2kCreateV2(0,C.byref(self.device_allocator),C.byref(self.pinned_allocator),C.byref(self.handle)),'create async allocator')
        else:
            check(nv.nvjpeg2kCreateSimple(C.byref(self.handle)), 'create')
        check(nv.nvjpeg2kDecodeStateCreate(self.handle, C.byref(self.state)), 'state create')
        self.states = [self.state]
        for _ in range(7):
            state = P()
            check(nv.nvjpeg2kDecodeStateCreate(self.handle, C.byref(state)), 'batch state create')
            self.states.append(state)
        check(nv.nvjpeg2kStreamCreate(C.byref(self.stream)), 'stream create')
        check(nv.nvjpeg2kDecodeParamsCreate(C.byref(self.params)), 'params create')
        check(nv.nvjpeg2kDecodeParamsSetRGBOutput(self.params, 1), 'RGB output')
        self.tile_params = [self.params]
        for _ in range(7):
            params = P()
            check(nv.nvjpeg2kDecodeParamsCreate(C.byref(params)), 'tile params create')
            check(nv.nvjpeg2kDecodeParamsSetRGBOutput(params, 1), 'tile RGB output')
            self.tile_params.append(params)
        check(cu.cudaStreamCreateWithFlags(C.byref(self.cuda), 1), 'CUDA stream create')
        stream_count = max(1, min(8, int(os.environ.get('RIVER_NVJPEG2000_TILE_STREAMS', '4'))))
        self.tile_cudas = [self.cuda]
        for _ in range(stream_count - 1):
            stream = P()
            check(cu.cudaStreamCreateWithFlags(C.byref(stream), 1), 'tile CUDA stream create')
            self.tile_cudas.append(stream)
        reserve=max(0,min(134217728,int(os.environ.get('RIVER_NVJPEG2000_RESERVE_BYTES','8388608'))))
        if reserve:
            check(cu.cudaMalloc(C.byref(self.gpu),reserve),'reserve output')
            check(cu.cudaHostAlloc(C.byref(self.host),reserve,0),'reserve pinned output')
            self.capacity=reserve

    def switch_source(self, identity):
        if self.identity==identity:return True
        if self.identity is not None:
            self.sources[self.identity]=(self.stream,self.buffer,self.mapping,self.info,self.components)
            self.stream,self.buffer,self.mapping=P(),None,None
        self.identity=None
        if identity in self.sources:
            self.stream,self.buffer,self.mapping,self.info,self.components=self.sources.pop(identity)
            self.identity=identity
            return True
        while len(self.sources)>1:
            _,(stream,buffer,mapping,_,_)=self.sources.popitem(last=False)
            check(nv.nvjpeg2kStreamDestroy(stream),'evict parsed stream')
            del buffer
            if mapping is not None:mapping.close()
        if self.stream.value:
            check(nv.nvjpeg2kStreamDestroy(self.stream),'stream destroy')
            self.buffer=None
            if self.mapping is not None:self.mapping.close()
        self.mapping=None
        check(nv.nvjpeg2kStreamCreate(C.byref(self.stream)),'stream create')
        return False

    def source(self, path):
        stat = path.stat()
        identity = (str(path), stat.st_size, stat.st_mtime_ns)
        if self.switch_source(identity):
            return 0.0
        start = time.perf_counter()
        # One parsed source per worker; mmap retains bytes without reading/copying
        # the complete mosaic on every ROI request. Invalidate on file changes.
        with path.open('rb') as source:
            self.mapping = mmap.mmap(source.fileno(), 0, access=mmap.ACCESS_COPY)
        self.buffer = (C.c_ubyte * len(self.mapping)).from_buffer(self.mapping)
        check(nv.nvjpeg2kStreamParse(self.handle, self.buffer, len(self.mapping), 0, 0, self.stream), 'parse')
        self.info = ImageInfo()
        check(nv.nvjpeg2kStreamGetImageInfo(self.stream, C.byref(self.info)), 'image info')
        if not 1 <= self.info.components <= 4:
            raise ValueError('unsupported JPEG2000 component count')
        self.components = []
        for i in range(self.info.components):
            comp = ComponentInfo()
            check(nv.nvjpeg2kStreamGetImageComponentInfo(self.stream, C.byref(comp), i), 'component info')
            if comp.width != self.info.width or comp.height != self.info.height or comp.signed or not 1 <= comp.precision <= 16:
                raise ValueError('native CUDA driver requires unsigned equal-size 1–16 bit component planes')
            self.components.append(comp)
        self.identity = identity
        return (time.perf_counter() - start) * 1000

    def tiles(self, ids, reduction, bounds=None):
        """Submit all native tiles before one pinned download/synchronization.

        Repeated per-tile CPU/GPU synchronization dominates WSL overview
        latency. Keep every output plane alive until the shared stream finishes.
        """
        info, scale = self.info, 1 << reduction
        dtype = np.uint16 if max(c.precision for c in self.components) > 8 else np.uint8
        itemsize = np.dtype(dtype).itemsize
        plans, total = [], 0
        roi_enabled = os.environ.get('RIVER_NVJPEG2000_PRECISE_ROI', 'true') == 'true'
        for tid in ids:
            x0, y0 = tid % info.columns * info.tile_width, tid // info.columns * info.tile_height
            x1, y1 = min(x0 + info.tile_width, info.width), min(y0 + info.tile_height, info.height)
            if roi_enabled and bounds:
                intersections = [(max(x0,a), max(y0,b), min(x1,c), min(y1,d))
                                 for a,b,c,d in bounds if a < x1 and c > x0 and b < y1 and d > y0]
                x0, y0 = min(r[0] for r in intersections), min(r[1] for r in intersections)
                x1, y1 = max(r[2] for r in intersections), max(r[3] for r in intersections)
            rx, ry = (x0 + scale - 1) // scale, (y0 + scale - 1) // scale
            w, h = (x1 + scale - 1) // scale - rx, (y1 + scale - 1) // scale - ry
            nres = U()
            check(nv.nvjpeg2kStreamGetResolutionsInTile(self.stream, tid, C.byref(nres)), 'resolutions')
            if reduction >= nres.value:
                raise ValueError('reduction exceeds codestream resolutions')
            # The native decode-area API uses the requested resolution grid.
            # Each in-flight state owns its parameters until GPU completion.
            plans.append((tid, rx, ry, w, h, total, nres.value - reduction, (rx,rx+w,ry,ry+h)))
            total += w * h * itemsize * info.components
        if total > int(os.environ.get('RIVER_NVJPEG2000_MAX_NATIVE_BYTES', '134217728')):
            raise ValueError('native decode workspace exceeds configured byte limit')
        if total > self.capacity:
            if self.gpu.value:
                check(cu.cudaFree(self.gpu), 'free output')
            if self.host.value:
                check(cu.cudaFreeHost(self.host), 'free pinned output')
            self.gpu, self.host, self.capacity = P(), P(), 0
            check(cu.cudaMalloc(C.byref(self.gpu), total), 'allocate output')
            check(cu.cudaHostAlloc(C.byref(self.host), total, 0), 'allocate pinned output')
            self.capacity = total
        start = time.perf_counter()
        refs = []
        used_streams = set()
        api_ms=0.0;wait_ms=0.0
        for index, (tid, rx, ry, w, h, offset, levels, area) in enumerate(plans):
            state_index = index % len(self.states)
            cuda = self.tile_cudas[state_index % len(self.tile_cudas)]
            used_streams.add(state_index % len(self.tile_cudas))
            if len(self.tile_cudas) == 1 and index and state_index == 0:
                check(cu.cudaStreamSynchronize(self.cuda), 'synchronize before state reuse')
            elif len(self.tile_cudas) > 1 and index >= len(self.states):
                check(cu.cudaStreamSynchronize(cuda), 'synchronize before state reuse')
            plane = w * h * itemsize
            pointers = (P * info.components)(*[self.gpu.value + offset + plane * i for i in range(info.components)])
            pitches = (Z * info.components)(*[w * itemsize] * info.components)
            output = Image(pointers, pitches, 1 if dtype == np.uint16 else 0, info.components)
            refs.append((pointers, pitches, output))
            params = self.tile_params[state_index]
            check(nv.nvjpeg2kDecodeParamsSetDecodeArea(params, *(area if roi_enabled else (0,0,0,0))), 'decode area')
            api_started=time.perf_counter()
            check(nv.nvjpeg2kDecodeTile(self.handle, self.states[state_index], self.stream, params, tid,
                                      levels, C.byref(output), cuda), f'decode tile {tid} ({w}x{h})')
            api_ms+=(time.perf_counter()-api_started)*1000
        wait_started=time.perf_counter()
        if len(self.tile_cudas) > 1:
            for index in used_streams:
                if index == 0: continue  # primary download is already stream-ordered
                check(cu.cudaStreamSynchronize(self.tile_cudas[index]), 'join native tile stream')
        check(cu.cudaMemcpyAsync(self.host, self.gpu, total, 2, self.cuda), 'download')
        check(cu.cudaStreamSynchronize(self.cuda), 'synchronize')
        decode_ms = (time.perf_counter() - start) * 1000
        wait_ms=(time.perf_counter()-wait_started)*1000
        self.last_tile_timing={'nativeSubmitMs':api_ms,'nativeWaitMs':wait_ms}
        result = {}
        for tid, rx, ry, w, h, offset, _, _ in plans:
            raw = np.frombuffer((C.c_ubyte * (w * h * itemsize * info.components)).from_address(self.host.value + offset), dtype=dtype).reshape(info.components, h, w)
            planes = [p if c.precision == 8 else sample_lut(c.precision)[p]
                      for p,c in zip(raw,self.components)]
            rgba=np.empty((h,w,4),dtype=np.uint8)
            rgba[:,:,0]=planes[0]
            rgba[:,:,1]=planes[0] if len(planes)<3 else planes[1]
            rgba[:,:,2]=planes[0] if len(planes)<3 else planes[2]
            rgba[:,:,3]=planes[1] if len(planes)==2 else planes[3] if len(planes)==4 else 255
            result[tid] = (rx, ry, rgba)
        return result, decode_ms

    def decode(self, path, width, height, bounds, reduction, max_pixels):
        check(cu.cudaSetDevice(self.device), 'cudaSetDevice')
        view_started = time.perf_counter()
        view = resolution_view.get(path, reduction) if reduction >= 3 and os.environ.get('RIVER_NVJPEG2000_RESOLUTION_VIEWS', 'true') == 'true' else None
        view_ms = (time.perf_counter() - view_started) * 1000
        if view is not None:
            return self.decode_view(path, width, height, bounds, reduction, max_pixels, view, view_ms)
        start = time.perf_counter()
        parse_ms = self.source(path)
        if (width, height) != (self.info.width, self.info.height):
            raise ValueError('source dimensions disagree with codestream')
        scale = 1 << reduction
        outputs, decode_ms, tile_count = [], 0.0, 0
        # Cache only within this request, to share intersecting native tiles in
        # a batch. No decoded raster cache disguises subsequent benchmarks.
        ids = set()
        for x0, y0, x1, y1 in bounds:
            for ty in range(y0 // self.info.tile_height, (y1 - 1) // self.info.tile_height + 1):
                for tx in range(x0 // self.info.tile_width, (x1 - 1) // self.info.tile_width + 1):
                    ids.add(ty * self.info.columns + tx)
        native_tiles, decode_ms = self.tiles(sorted(ids), reduction, bounds)
        tile_count = len(ids)
        for x0, y0, x1, y1 in bounds:
            rx0, ry0 = (x0 + scale - 1) // scale, (y0 + scale - 1) // scale
            rx1, ry1 = (x1 + scale - 1) // scale, (y1 + scale - 1) // scale
            if (rx1 - rx0) * (ry1 - ry0) > max_pixels:
                raise ValueError('decoded output exceeds its configured pixel limit')
            rgba = np.empty((ry1 - ry0, rx1 - rx0, 4), dtype=np.uint8)
            for ty in range(y0 // self.info.tile_height, (y1 - 1) // self.info.tile_height + 1):
                for tx in range(x0 // self.info.tile_width, (x1 - 1) // self.info.tile_width + 1):
                    tid = ty * self.info.columns + tx
                    nx, ny, pixels = native_tiles[tid]
                    left, top = max(nx, rx0), max(ny, ry0)
                    right, bottom = min(nx + pixels.shape[1], rx1), min(ny + pixels.shape[0], ry1)
                    rgba[top - ry0:bottom - ry0, left - rx0:right - rx0] = pixels[top - ny:bottom - ny, left - nx:right - nx]
            outputs.append(rgba)
        return outputs, {'parseMs': parse_ms, 'nativeDecodeMs': decode_ms,
                         **self.last_tile_timing,
                         'rasterMs': (time.perf_counter() - start) * 1000 - parse_ms - decode_ms,
                         'nativeTileCount': tile_count}

    def decode_view(self, path, width, height, bounds, reduction, max_pixels, view, view_ms):
        """One GPU image decode of the exact original compressed LL packets."""
        start = time.perf_counter();scale = 1 << reduction
        w,h = view['width'],view['height']
        if (w,h) != ((width+scale-1)//scale,(height+scale-1)//scale):
            raise ValueError('compressed resolution view dimensions disagree')
        stat = path.stat();identity = (str(path),stat.st_size,stat.st_mtime_ns,'lowest')
        parse_ms = 0.0
        if not self.switch_source(identity):
            parsed_at = time.perf_counter()
            self.buffer = (C.c_ubyte*len(view['data'])).from_buffer_copy(view['data'])
            check(nv.nvjpeg2kStreamParse(self.handle,self.buffer,len(view['data']),0,0,self.stream),'parse resolution view')
            self.info = ImageInfo();check(nv.nvjpeg2kStreamGetImageInfo(self.stream,C.byref(self.info)),'image info')
            self.components=[]
            for i in range(self.info.components):
                comp=ComponentInfo();check(nv.nvjpeg2kStreamGetImageComponentInfo(self.stream,C.byref(comp),i),'component info')
                if comp.signed or comp.width!=w or comp.height!=h or not 1<=comp.precision<=16:
                    raise ValueError('unsupported compressed view planes')
                self.components.append(comp)
            self.identity=identity;parse_ms=(time.perf_counter()-parsed_at)*1000
        low_bounds=[((x0+scale-1)//scale,(y0+scale-1)//scale,(x1+scale-1)//scale,(y1+scale-1)//scale) for x0,y0,x1,y1 in bounds]
        ids=set()
        for left,top,right,bottom in low_bounds:
            for ty in range(top//self.info.tile_height,(bottom-1)//self.info.tile_height+1):
                for tx in range(left//self.info.tile_width,(right-1)//self.info.tile_width+1):
                    ids.add(ty*self.info.columns+tx)
        tiles,decode_ms=self.tiles(sorted(ids),0,low_bounds)
        outputs=[]
        for left,top,right,bottom in low_bounds:
            if (right-left)*(bottom-top)>max_pixels:raise ValueError('view output exceeds configured pixel limit')
            rgba=np.empty((bottom-top,right-left,4),dtype=np.uint8)
            for ty in range(top//self.info.tile_height,(bottom-1)//self.info.tile_height+1):
                for tx in range(left//self.info.tile_width,(right-1)//self.info.tile_width+1):
                    nx,ny,pixels=tiles[ty*self.info.columns+tx]
                    a,b=max(nx,left),max(ny,top);c,d=min(nx+pixels.shape[1],right),min(ny+pixels.shape[0],bottom)
                    rgba[b-top:d-top,a-left:c-left]=pixels[b-ny:d-ny,a-nx:c-nx]
            outputs.append(rgba)
        return outputs,{'parseMs':parse_ms,'nativeDecodeMs':decode_ms,
                        **self.last_tile_timing,
                        'rasterMs':(time.perf_counter()-start)*1000-parse_ms-decode_ms,
                        'nativeTileCount':len(ids),'resolutionViewMs':view_ms,
                        'viewBytes':len(view['data']),'readBytes':0 if view['cacheHit'] else view['readBytes'],
                        'sourceBytes':view['sourceBytes'],'resolutionView':True}

    def close(self):
        cu.cudaSetDevice(self.device)
        if self.gpu.value:
            cu.cudaFree(self.gpu)
        if self.host.value:
            cu.cudaFreeHost(self.host)
        nv.nvjpeg2kStreamDestroy(self.stream)
        self.buffer = None
        if self.mapping is not None:
            self.mapping.close()
        for stream,buffer,mapping,_,_ in self.sources.values():
            nv.nvjpeg2kStreamDestroy(stream)
            del buffer
            # Tuple ownership retains buffer until sources is cleared below.
        cached_mappings=[s[2] for s in self.sources.values() if s[2] is not None]
        self.sources.clear()
        for mapping in cached_mappings:mapping.close()
        for params in self.tile_params:
            nv.nvjpeg2kDecodeParamsDestroy(params)
        for state in self.states:
            nv.nvjpeg2kDecodeStateDestroy(state)
        nv.nvjpeg2kDestroy(self.handle)
        for stream in self.tile_cudas:
            cu.cudaStreamDestroy(stream)
