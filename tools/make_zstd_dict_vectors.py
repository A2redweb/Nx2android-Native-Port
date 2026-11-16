#!/usr/bin/env python3
"""Regenerates crates/nx-zstd/tests/dict/: a dictionary trained by libzstd (through ctypes) on generated records, frames that name it, and the plain data of each.
Needs libzstd.so.1; run it only to change the vectors."""
import ctypes, os
z = ctypes.CDLL('libzstd.so.1')
M = (1 << 64) - 1
x = 7


def rnd():
    global x
    x = (x * 6364136223846793005 + 1442695040888963407) & M
    return x >> 33


NAMES = [b'albedo', b'normal', b'roughness', b'emission', b'shadow', b'vertex', b'fragment', b'mask', b'lut', b'bloom']


def record():
    return b'{"shader":"%s_%d","stage":%d,"keys":[%d,%d,%d],"flags":"%s"}\n' % (NAMES[rnd() % 10], rnd() % 500, rnd() % 6, rnd() % 9000, rnd() % 9000, rnd() % 9000, NAMES[rnd() % 10])


samples = [b''.join(record() for _ in range(3 + rnd() % 4)) for _ in range(3000)]
buf = b''.join(samples)
sizes = (ctypes.c_size_t * len(samples))(*[len(s) for s in samples])
dbuf = ctypes.create_string_buffer(16384)
z.ZDICT_trainFromBuffer.restype = ctypes.c_size_t
n = z.ZDICT_trainFromBuffer(dbuf, ctypes.c_size_t(16384), buf, sizes, ctypes.c_uint(len(samples)))
assert not z.ZDICT_isError(ctypes.c_size_t(n)), 'train failed'
dic = dbuf.raw[:n]
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), '../crates/nx-zstd/tests/dict')
open(os.path.join(out, 'dict.bin'), 'wb').write(dic)
z.ZSTD_createCCtx.restype = ctypes.c_void_p
z.ZSTD_compress_usingDict.restype = ctypes.c_size_t
cctx = ctypes.c_void_p(z.ZSTD_createCCtx())
for i, level in enumerate((1, 3, 19)):
    plain = b''.join(record() for _ in range(40 + 300 * i))
    dst = ctypes.create_string_buffer(len(plain) + 1024)
    m = z.ZSTD_compress_usingDict(cctx, dst, ctypes.c_size_t(len(dst)), plain, ctypes.c_size_t(len(plain)), dic, ctypes.c_size_t(len(dic)), level)
    assert not z.ZSTD_isError(ctypes.c_size_t(m))
    open(os.path.join(out, f'dict_{i}.zst'), 'wb').write(dst.raw[:m])
    open(os.path.join(out, f'dict_{i}.raw'), 'wb').write(plain)
    print(i, level, len(plain), m)
print('dictionary', len(dic), 'bytes, id', int.from_bytes(dic[4:8], 'little'))
