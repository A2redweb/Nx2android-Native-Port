#!/usr/bin/env python3
"""Regenerates crates/nx-zstd/tests/data/*.zst with libzstd (through ctypes). The same deterministic inputs are rebuilt in crates/nx-zstd/tests/vectors.rs, so only
the compressed files are stored. Needs libzstd.so.1 on the machine; run it only to change the vectors."""
import ctypes, os, sys
M = (1 << 64) - 1


def lcg(seed):
    x = seed
    while True:
        x = (x * 6364136223846793005 + 1442695040888963407) & M
        yield x >> 33


WORDS = [b"the", b"quick", b"brown", b"fox", b"jumps", b"over", b"lazy", b"dog", b"switch", b"android", b"vulkan", b"shader", b"nso", b"romfs", b"\n", b"  "]


def text():
    g, out = lcg(1), bytearray()
    while len(out) < 200000:
        out += WORDS[next(g) % len(WORDS)] + b" "
    return bytes(out[:200000])


def runs():
    g, out = lcg(2), bytearray()
    while len(out) < 150000:
        b, n = next(g) & 0xFF, 1 + next(g) % 3000
        out += bytes([b]) * n
    return bytes(out[:150000])


def noise():
    g = lcg(3)
    return bytes((next(g) >> 8) & 0xFF for _ in range(100000))


def records():
    g, out = lcg(4), bytearray()
    for i in range(300000 // 16):
        out += i.to_bytes(4, "little") + ((i * i) & 0xFFFFFFFF).to_bytes(4, "little") + (next(g) & 0x3FF).to_bytes(2, "little") + bytes([i % 251]) * 6
    return bytes(out)


if __name__ == "__main__":
    z = ctypes.CDLL("libzstd.so.1")
    z.ZSTD_compress.restype = ctypes.c_size_t
    z.ZSTD_compressBound.restype = ctypes.c_size_t
    z.ZSTD_isError.restype = ctypes.c_uint
    d = os.path.join(os.path.dirname(__file__), "..", "crates", "nx-zstd", "tests", "data")
    for name, fn, levels in (("text", text, (1, 3, 9, 19)), ("runs", runs, (1, 3, 19)), ("noise", noise, (1,)), ("records", records, (1, 3, 9, 19))):
        src = fn()
        for lv in levels:
            cap = z.ZSTD_compressBound(ctypes.c_size_t(len(src)))
            buf = ctypes.create_string_buffer(cap)
            n = z.ZSTD_compress(buf, ctypes.c_size_t(cap), src, ctypes.c_size_t(len(src)), ctypes.c_int(lv))
            assert not z.ZSTD_isError(ctypes.c_size_t(n)), (name, lv)
            open(os.path.join(d, f"{name}_{lv}.zst"), "wb").write(buf.raw[:n])
            print(name, lv, len(src), "->", n)
