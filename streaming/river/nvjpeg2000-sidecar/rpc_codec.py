"""Lossless low-latency raster transport; output encoders remain in Rust."""
import ctypes as C
import ctypes.util
import numpy as np

lz4=C.CDLL(ctypes.util.find_library('lz4') or 'liblz4.so.1')
lz4.LZ4_compressBound.argtypes=[C.c_int];lz4.LZ4_compressBound.restype=C.c_int
lz4.LZ4_compress_default.argtypes=[C.c_void_p,C.c_void_p,C.c_int,C.c_int];lz4.LZ4_compress_default.restype=C.c_int
lz4.LZ4_decompress_safe.argtypes=[C.c_void_p,C.c_void_p,C.c_int,C.c_int];lz4.LZ4_decompress_safe.restype=C.c_int

def encode(rgba,codec):
    if codec=='raw':return memoryview(rgba).cast('B')
    if codec not in ('lz4','delta-lz4'):raise ValueError('unsupported RPC raster codec')
    source=rgba
    if codec=='delta-lz4':
        source=np.empty_like(rgba)
        source[:,0,:]=rgba[:,0,:]
        np.subtract(rgba[:,1:,:],rgba[:,:-1,:],out=source[:,1:,:])
    count=source.nbytes;capacity=lz4.LZ4_compressBound(count);output=C.create_string_buffer(capacity)
    size=lz4.LZ4_compress_default(source.ctypes.data,output,count,capacity)
    if size<=0:raise RuntimeError('LZ4 raster compression failed')
    return output.raw[:size]
