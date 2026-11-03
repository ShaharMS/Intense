"""Exact lowest-resolution compressed views for River's lossless RPCL corpus.

No pixels are decoded or re-encoded. The original LL packet bytes are retained;
SIZ/COD/QCD and tile-part lengths describe their native reduced grid. Other
codestream layouts return None and continue through the ordinary decoder.
"""
import math
import os
import struct
import threading
from collections import OrderedDict

_cache = OrderedDict()
_lock = threading.Lock()
_condition = threading.Condition(_lock)
_pending = set()
_budget = 16 * 1024 * 1024


class Unsupported(Exception):
    pass


class Bits:
    def __init__(self, data, offset):
        self.data, self.offset, self.remaining, self.byte, self.previous = data, offset, 0, 0, 0

    def bit(self):
        if not self.remaining:
            if self.offset >= len(self.data):
                raise Unsupported('packet prefix exceeds probe')
            self.byte = self.data[self.offset]
            self.offset += 1
            self.remaining = 7 if self.previous == 255 else 8
            self.previous = self.byte
        self.remaining -= 1
        return (self.byte >> self.remaining) & 1

    def bits(self, count):
        value = 0
        for _ in range(count):
            value = (value << 1) | self.bit()
        return value

    def align(self):
        self.remaining = 0
        if self.previous == 255:
            self.offset += 1
        self.previous = 0


def packet_end(data, offset):
    bits = Bits(data, offset)
    length = 0
    if bits.bit() and bits.bit():  # present packet, single included codeblock
        zeros = 0
        while not bits.bit():
            zeros += 1
            if zeros > 31:
                raise Unsupported('invalid zero-bitplane tag tree')
        if not bits.bit():
            passes = 1
        elif not bits.bit():
            passes = 2
        else:
            value = bits.bits(2)
            if value < 3:
                passes = 3 + value
            else:
                value = bits.bits(5)
                passes = 6 + value if value < 31 else 37 + bits.bits(7)
        lblock = 3
        while bits.bit():
            lblock += 1
            if lblock > 31:
                raise Unsupported('invalid codeblock length')
        length = bits.bits(lblock + passes.bit_length() - 1)
    bits.align()
    end = bits.offset + length
    if end > len(data):
        raise Unsupported('packet prefix exceeds probe')
    return end


def marker(code, payload):
    return struct.pack('>HH', code, len(payload) + 2) + payload


def extract(path, reduction):
    try:
        return _extract(path, reduction)
    except (Unsupported, struct.error, IndexError):
        return None


def _extract(path, reduction):
    with open(path, 'rb') as source:
        fd = source.fileno()
        stat = os.fstat(fd)
        first = os.pread(fd, min(65536, stat.st_size), 0)
        read_bytes = len(first)
        def read_at(offset, count):
            nonlocal read_bytes
            end = min(offset + count, stat.st_size)
            if end <= len(first):
                return first[offset:end]
            data = os.pread(fd, count, offset)
            read_bytes += len(data)
            return data
        if first[:2] == b'\xffO':
            start = 0
        else:
            box = 0
            while box + 8 <= len(first):
                size, kind = struct.unpack_from('>I4s', first, box)
                if size < 8:
                    raise Unsupported('extended JP2 box')
                if kind == b'jp2c':
                    start = box + 8
                    break
                box += size
            else:
                raise Unsupported('large JP2 prefix')
        cursor = start + 2
        headers = []
        siz = cod = qcd = None
        while cursor + 4 <= len(first):
            code, length = struct.unpack_from('>HH', first, cursor)
            if code == 0xff90:
                break
            if length < 2: raise Unsupported('invalid marker length')
            payload = first[cursor + 4:cursor + 2 + length]
            if len(payload) != length - 2:
                raise Unsupported('large codestream header')
            if code == 0xff51: siz = payload
            elif code == 0xff52: cod = payload
            elif code == 0xff5c: qcd = payload
            elif code != 0xff64: raise Unsupported('unsupported main-header marker')
            headers.append((code, payload))
            cursor += 2 + length
        if siz is None or cod is None or qcd is None:
            raise Unsupported('missing headers')
        if cod[0] != 1 or cod[1] != 2 or struct.unpack_from('>H', cod, 2)[0] != 1 or cod[4] != 0:
            raise Unsupported('requires precinct RPCL, one layer, no MCT')
        if cod[5] != reduction or reduction < 1 or cod[8] != 0 or cod[9] != 1 or qcd[0] & 31:
            raise Unsupported('requires lowest reversible resolution, ordinary codeblocks')
        scale = 1 << reduction
        rsiz, width, height, ox, oy, tw, th, tox, toy, components = struct.unpack_from('>H8IH', siz)
        if rsiz not in (0,1,2) or ox or oy or tox or toy or tw % scale or th % scale or not 1 <= components <= 4:
            raise Unsupported('unsupported reference grid')
        if len(siz) != 36 + components * 3 or any(siz[37 + 3*i:39 + 3*i] != b'\x01\x01' for i in range(components)):
            raise Unsupported('subsampled components')
        pw, ph = 1 << (cod[10] & 15), 1 << (cod[10] >> 4)
        cbw, cbh = 1 << (cod[6] + 2), 1 << (cod[7] + 2)
        low_width, low_height = (width + scale - 1) // scale, (height + scale - 1) // scale
        if low_width * low_height > 1024 * 1024:
            raise Unsupported('view output too large')
        low_siz = struct.pack('>H8IH', rsiz, low_width, low_height, 0, 0, tw//scale, th//scale, 0, 0, components) + siz[36:]
        low_cod = bytearray(cod[:10]);low_cod[5] = 0;low_cod.append(cod[10])
        # At zero DWT levels, independent LL codeblocks can become precincts
        # in one tile without changing their entropy bytes or sample values.
        # Restrict this to one block/packet and power-of-two aligned tiles.
        lw, lh = tw//scale, th//scale
        packed_w,packed_h=min(lw,pw,cbw),min(lh,ph,cbh)
        packed = (os.environ.get('RIVER_NVJPEG2000_PACKED_VIEWS','true') == 'true'
                  and 4 <= packed_w and 4 <= packed_h and min(pw,lw)<=cbw and min(ph,lh)<=cbh
                  and lw & (lw-1) == 0 and lh & (lh-1) == 0)
        if packed:
            low_siz = struct.pack('>H8IH',0,low_width,low_height,0,0,low_width,low_height,0,0,components)+siz[36:]
            low_cod[6],low_cod[7] = packed_w.bit_length()-3,packed_h.bit_length()-3
            low_cod[10] = (packed_h.bit_length()-1)*16+packed_w.bit_length()-1
        result = bytearray(b'\xffO')
        for code, payload in headers:
            result += marker(code, low_siz if code == 0xff51 else bytes(low_cod) if code == 0xff52 else qcd[:2] if code == 0xff5c else payload)
        columns = (width + tw - 1) // tw
        rows = (height + th - 1) // th
        if columns * rows > 4096: raise Unsupported('too many tile parts for a compact view')
        seen = set();packed_packets={}
        for _ in range(columns * rows):
            prefix = read_at(cursor, 8192)
            if prefix[:4] != b'\xff\x90\x00\x0a' or prefix[12:14] != b'\xff\x93':
                raise Unsupported('tile headers or packet markers')
            tid, size = struct.unpack_from('>HI', prefix, 4)
            if tid in seen or tid >= columns * rows or prefix[10] != 0 or prefix[11] not in (0, 1) or size < 14:
                raise Unsupported('multiple tile parts')
            seen.add(tid)
            x, y = tid % columns * tw, tid // columns * th
            w, h = (min(x+tw,width)+scale-1)//scale-x//scale, (min(y+th,height)+scale-1)//scale-y//scale
            if min(pw,w) > cbw or min(ph,h) > cbh:
                raise Unsupported('multiple codeblocks per precinct')
            packets = ((w+pw-1)//pw) * ((h+ph-1)//ph) * components
            end = 14
            for py in range((h+ph-1)//ph):
                for px in range((w+pw-1)//pw):
                    for component in range(components):
                        packet_start=end;end=packet_end(prefix,end)
                        if packed:
                            key=(y//scale+py*ph,x//scale+px*pw,component)
                            packed_packets[key]=prefix[packet_start:end]
            if end > size: raise Unsupported('packet prefix crosses tile part')
            sot = bytearray(prefix[:12]);struct.pack_into('>I', sot, 6, end)
            if packed:
                pass
            else:
                result += sot + prefix[12:end]
            cursor += size
        if read_at(cursor, 2) != b'\xff\xd9':
            raise Unsupported('unexpected codestream tail')
        after = os.fstat(fd)
        if after.st_size != stat.st_size or after.st_mtime_ns != stat.st_mtime_ns:
            raise Unsupported('source changed during extraction')
        if packed:
            payload=b''.join(packed_packets[key] for key in sorted(packed_packets))
            result += struct.pack('>HHHIBB',0xff90,10,0,14+len(payload),0,1)+b'\xff\x93'+payload
        result += b'\xff\xd9'
        return dict(data=bytes(result),width=low_width,height=low_height,scale=scale,readBytes=read_bytes,sourceBytes=stat.st_size,packed=packed)


def get(path, reduction):
    stat = path.stat();key = (str(path),stat.st_size,stat.st_mtime_ns,reduction)
    # Coalesce only the same source. Cached views and other sources never wait
    # behind another file's packet extraction while the IO is in progress.
    with _condition:
        while key in _pending and key not in _cache: _condition.wait()
        if key in _cache:
            value = _cache.pop(key);_cache[key] = value;return {**value, 'cacheHit': True} if value else None
        _pending.add(key)
    try:
        value = extract(path, reduction)
    except BaseException:
        with _condition:
            _pending.remove(key);_condition.notify_all()
        raise
    with _condition:
        _cache[key] = value
        while sum(len(v['data']) for v in _cache.values() if v is not None) > _budget or len(_cache) > 64:
            _cache.popitem(last=False)
        _pending.remove(key);_condition.notify_all()
        return {**value, 'cacheHit': False} if value else None
