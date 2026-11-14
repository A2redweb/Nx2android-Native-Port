//! A Zstandard decoder (RFC 8878) in plain Rust, for NSZ / XCZ dumps. Streaming: the input is read block by block, the output goes to a writer, and only the last `window` bytes are kept.
//!
//! Supported: all block types, raw / RLE / Huffman literals (one or four streams, repeated trees), predefined / RLE / FSE / repeated sequence tables, repeat offsets, multiple frames,
//! skippable frames. Not supported: dictionaries. The content checksum is skipped, not verified (NCZ files are checked against the NCA's SHA-256 afterwards).
//! Checked against the output of libzstd for several levels and data kinds (tests/data), plus corrupt inputs that must give errors, never panics.

use std::io::{self, Read, Write};

fn bad(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("zstd: {m}"))
}

type R<T> = io::Result<T>;

/// Largest window accepted (a corrupt header cannot make the decoder reserve more).
const MAX_WINDOW: u64 = 1 << 29;
const MAX_BLOCK: usize = 128 << 10;

// ---------------------------------------------------------------- bits

/// Reads a stream backwards from its last set bit (the end marker), the way FSE and Huffman streams are stored. Reading past the start gives zero bits and a negative `left()`.
struct Back<'a> {
    d: &'a [u8],
    pos: i64,
}

impl<'a> Back<'a> {
    fn new(d: &'a [u8]) -> R<Self> {
        let last = *d.last().ok_or_else(|| bad("empty bit stream"))?;
        if last == 0 {
            return Err(bad("bit stream has no end marker"));
        }
        Ok(Back { d, pos: (d.len() as i64) * 8 - 1 - last.leading_zeros() as i64 })
    }
    /// n bits (n <= 56) from `lo` upward as a little-endian number; bits below 0 are zero.
    fn get(&self, lo: i64, n: u32) -> u64 {
        if n == 0 {
            return 0;
        }
        let (lo, n, shift) = if lo < 0 { (0, n.saturating_sub((-lo) as u32), (-lo) as u32) } else { (lo, n, 0) };
        if n == 0 {
            return 0;
        }
        let byte = (lo / 8) as usize;
        let mut w = [0u8; 8];
        let end = (byte + 8).min(self.d.len());
        if byte < end {
            w[..end - byte].copy_from_slice(&self.d[byte..end]);
        }
        let v = (u64::from_le_bytes(w) >> (lo % 8)) & ((1u64 << n) - 1);
        v << shift
    }
    fn peek(&self, n: u32) -> u64 {
        self.get(self.pos - n as i64, n)
    }
    fn read(&mut self, n: u32) -> u64 {
        let v = self.peek(n);
        self.pos -= n as i64;
        v
    }
    fn left(&self) -> i64 {
        self.pos
    }
}

/// Reads a forward, least-significant-bit-first stream (the FSE table description).
struct Fwd<'a> {
    d: &'a [u8],
    bit: usize,
}

impl Fwd<'_> {
    fn peek(&self, n: u32) -> u32 {
        let mut v = 0u64;
        for i in 0..8 {
            if let Some(b) = self.d.get(self.bit / 8 + i) {
                v |= (*b as u64) << (8 * i);
            }
        }
        ((v >> (self.bit % 8)) & ((1u64 << n) - 1)) as u32
    }
    fn skip(&mut self, n: u32) {
        self.bit += n as usize;
    }
    fn bytes_used(&self) -> usize {
        self.bit.div_ceil(8)
    }
}

// ---------------------------------------------------------------- FSE

#[derive(Clone, Copy, Default)]
struct FseEntry {
    symbol: u8,
    nbits: u8,
    base: u16,
}

#[derive(Clone, Default)]
struct Fse {
    al: u32,
    t: Vec<FseEntry>,
}

/// Reads a normalised count table (`-1` = "less than one"). Returns the counts, the accuracy log and the bytes used.
fn read_counts(d: &[u8], max_symbol: usize, max_al: u32) -> R<(Vec<i32>, u32, usize)> {
    let mut f = Fwd { d, bit: 0 };
    let al = f.peek(4) + 5;
    f.skip(4);
    if al > max_al {
        return Err(bad("FSE accuracy log too large"));
    }
    let mut remaining: i32 = (1 << al) + 1;
    let mut threshold: i32 = 1 << al;
    let mut nbits = al + 1;
    let mut probs: Vec<i32> = Vec::new();
    let mut prev0 = false;
    while remaining > 1 && probs.len() <= max_symbol {
        if prev0 {
            let mut n0 = probs.len();
            loop {
                let v = f.peek(16);
                if v == 0xFFFF {
                    n0 += 24;
                    f.skip(16);
                } else {
                    break;
                }
            }
            loop {
                let v = f.peek(2);
                if v == 3 {
                    n0 += 3;
                    f.skip(2);
                } else {
                    n0 += v as usize;
                    f.skip(2);
                    break;
                }
            }
            if n0 > max_symbol + 1 {
                return Err(bad("FSE table names too many symbols"));
            }
            probs.resize(n0, 0);
            if probs.len() > max_symbol {
                break;
            }
        }
        let max = (2 * threshold - 1) - remaining;
        let low = f.peek(nbits - 1) as i32 & (threshold - 1);
        let count = if low < max {
            f.skip(nbits - 1);
            low
        } else {
            let mut c = f.peek(nbits) as i32 & (2 * threshold - 1);
            if c >= threshold {
                c -= max;
            }
            f.skip(nbits);
            c
        };
        let count = count - 1;
        remaining -= count.abs();
        probs.push(count);
        prev0 = count == 0;
        while remaining < threshold {
            nbits -= 1;
            threshold >>= 1;
        }
        if f.bit > d.len() * 8 {
            return Err(bad("FSE table description runs past its data"));
        }
    }
    if remaining != 1 {
        return Err(bad("FSE counts do not add up"));
    }
    Ok((probs, al, f.bytes_used()))
}

fn build_fse(probs: &[i32], al: u32) -> R<Fse> {
    let size = 1usize << al;
    let mut t = vec![FseEntry::default(); size];
    let mut next = vec![0u32; probs.len()];
    let mut high = size - 1;
    for (s, &p) in probs.iter().enumerate() {
        if p == -1 {
            t[high].symbol = s as u8;
            next[s] = 1;
            high = high.wrapping_sub(1);
        } else {
            next[s] = p as u32;
        }
    }
    let step = (size >> 1) + (size >> 3) + 3;
    let mask = size - 1;
    let mut pos = 0usize;
    for (s, &p) in probs.iter().enumerate() {
        for _ in 0..p.max(0) {
            t[pos].symbol = s as u8;
            pos = (pos + step) & mask;
            while pos > high && high != usize::MAX {
                pos = (pos + step) & mask;
            }
        }
    }
    if pos != 0 {
        return Err(bad("FSE table did not fill evenly"));
    }
    for e in t.iter_mut() {
        let s = e.symbol as usize;
        let ns = next[s];
        next[s] += 1;
        if ns == 0 {
            return Err(bad("FSE table is inconsistent"));
        }
        let nb = al - (31 - ns.leading_zeros());
        e.nbits = nb as u8;
        e.base = ((ns << nb) as usize).wrapping_sub(size) as u16;
    }
    Ok(Fse { al, t })
}

// ---------------------------------------------------------------- Huffman

#[derive(Clone, Default)]
struct Huff {
    bits: u32,
    /// per table index: (symbol, code length)
    t: Vec<(u8, u8)>,
}

fn build_huff(weights: &[u8]) -> R<Huff> {
    // weights of symbols 0..n-1; the last symbol's weight is implied
    let mut total: u32 = 0;
    for &w in weights {
        if w > 11 {
            return Err(bad("Huffman weight too large"));
        }
        if w > 0 {
            total += 1 << (w - 1);
        }
    }
    if total == 0 {
        return Err(bad("Huffman tree has no weights"));
    }
    let bits = 32 - total.leading_zeros(); // smallest b with 2^b > total
    if bits > 11 {
        return Err(bad("Huffman tree too deep"));
    }
    let rest = (1u32 << bits) - total;
    if !rest.is_power_of_two() {
        return Err(bad("Huffman weights do not complete a power of two"));
    }
    let mut w: Vec<u8> = weights.to_vec();
    w.push(rest.trailing_zeros() as u8 + 1);
    if w.len() > 256 {
        return Err(bad("Huffman tree has too many symbols"));
    }
    let mut rank = [0u32; 14];
    for &x in &w {
        rank[x as usize] += 1;
    }
    let mut start = [0u32; 14];
    let mut next = 0u32;
    for n in 1..=bits as usize {
        start[n] = next;
        next += rank[n] << (n - 1);
    }
    let mut t = vec![(0u8, 0u8); 1 << bits];
    for (s, &x) in w.iter().enumerate() {
        if x == 0 {
            continue;
        }
        let len = (1u32 << (x - 1)) as usize;
        let at = start[x as usize] as usize;
        for e in &mut t[at..at + len] {
            *e = (s as u8, (bits + 1 - x as u32) as u8);
        }
        start[x as usize] += len as u32;
    }
    Ok(Huff { bits, t })
}

fn read_huff(d: &[u8]) -> R<(Huff, usize)> {
    let hb = *d.first().ok_or_else(|| bad("missing Huffman tree"))? as usize;
    let mut weights: Vec<u8> = Vec::new();
    let used;
    if hb >= 128 {
        let n = hb - 127;
        let bytes = n.div_ceil(2);
        let b = d.get(1..1 + bytes).ok_or_else(|| bad("Huffman tree cut short"))?;
        for i in 0..n {
            weights.push(if i % 2 == 0 { b[i / 2] >> 4 } else { b[i / 2] & 15 });
        }
        used = 1 + bytes;
    } else {
        let comp = d.get(1..1 + hb).ok_or_else(|| bad("Huffman tree cut short"))?;
        let (probs, al, n) = read_counts(comp, 12, 6)?;
        let fse = build_fse(&probs, al)?;
        let mut br = Back::new(&comp[n..])?;
        let mut s1 = br.read(al) as usize;
        let mut s2 = br.read(al) as usize;
        loop {
            if weights.len() > 255 {
                return Err(bad("too many Huffman weights"));
            }
            let e = fse.t[s1];
            weights.push(e.symbol);
            s1 = e.base as usize + br.read(e.nbits as u32) as usize;
            if br.left() < 0 {
                weights.push(fse.t[s2].symbol);
                break;
            }
            let e = fse.t[s2];
            weights.push(e.symbol);
            s2 = e.base as usize + br.read(e.nbits as u32) as usize;
            if br.left() < 0 {
                weights.push(fse.t[s1].symbol);
                break;
            }
        }
        used = 1 + hb;
    }
    Ok((build_huff(&weights)?, used))
}

fn huff_stream(h: &Huff, d: &[u8], n: usize, out: &mut Vec<u8>) -> R<()> {
    let mut br = Back::new(d)?;
    for _ in 0..n {
        let (sym, len) = h.t[br.peek(h.bits) as usize];
        out.push(sym);
        br.pos -= len as i64;
        if br.pos < -(h.bits as i64) {
            return Err(bad("Huffman stream runs out"));
        }
    }
    if br.pos != 0 {
        return Err(bad("Huffman stream has unused bits"));
    }
    Ok(())
}

// ---------------------------------------------------------------- sequences

const LL_BASE: [(u32, u32); 36] = [
    (0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 0), (7, 0), (8, 0), (9, 0), (10, 0), (11, 0), (12, 0), (13, 0), (14, 0), (15, 0),
    (16, 1), (18, 1), (20, 1), (22, 1), (24, 2), (28, 2), (32, 3), (40, 3), (48, 4), (64, 6), (128, 7), (256, 8), (512, 9), (1024, 10),
    (2048, 11), (4096, 12), (8192, 13), (16384, 14), (32768, 15), (65536, 16),
];
const ML_BASE: [(u32, u32); 53] = [
    (3, 0), (4, 0), (5, 0), (6, 0), (7, 0), (8, 0), (9, 0), (10, 0), (11, 0), (12, 0), (13, 0), (14, 0), (15, 0), (16, 0), (17, 0), (18, 0),
    (19, 0), (20, 0), (21, 0), (22, 0), (23, 0), (24, 0), (25, 0), (26, 0), (27, 0), (28, 0), (29, 0), (30, 0), (31, 0), (32, 0), (33, 0), (34, 0),
    (35, 1), (37, 1), (39, 1), (41, 1), (43, 2), (47, 2), (51, 3), (59, 3), (67, 4), (83, 4), (99, 5), (131, 7), (259, 8), (515, 9), (1027, 10),
    (2051, 11), (4099, 12), (8195, 13), (16387, 14), (32771, 15), (65539, 16),
];
const LL_DEFAULT: [i32; 36] = [4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1, -1, -1, -1, -1];
const ML_DEFAULT: [i32; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
const OF_DEFAULT: [i32; 29] = [1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1];

// ---------------------------------------------------------------- decoder

pub struct Decoder {
    hist: Vec<u8>,
    flushed: usize,
    window: usize,
    huff: Option<Huff>,
    ll: Option<Fse>,
    of: Option<Fse>,
    ml: Option<Fse>,
    rep: [u32; 3],
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder { hist: Vec::new(), flushed: 0, window: 0, huff: None, ll: None, of: None, ml: None, rep: [1, 4, 8] }
    }
}

fn fse_for(mode: u8, d: &[u8], at: &mut usize, default: &[i32], default_al: u32, max_sym: usize, max_al: u32, prev: &Option<Fse>) -> R<Fse> {
    match mode {
        0 => build_fse(default, default_al),
        1 => {
            let s = *d.get(*at).ok_or_else(|| bad("sequence table cut short"))?;
            *at += 1;
            if s as usize > max_sym {
                return Err(bad("RLE symbol out of range"));
            }
            let mut probs = vec![0i32; s as usize + 1];
            probs[s as usize] = 1;
            // a one-symbol table: accuracy 0
            Ok(Fse { al: 0, t: vec![FseEntry { symbol: s, nbits: 0, base: 0 }] })
        }
        2 => {
            let (probs, al, n) = read_counts(d.get(*at..).ok_or_else(|| bad("sequence table cut short"))?, max_sym, max_al)?;
            *at += n;
            build_fse(&probs, al)
        }
        _ => prev.clone().ok_or_else(|| bad("repeated sequence table with none before")),
    }
}

impl Decoder {
    fn new_frame(&mut self, window: usize) {
        self.window = window;
        self.huff = None;
        self.ll = None;
        self.of = None;
        self.ml = None;
        self.rep = [1, 4, 8];
    }

    fn flush<W: Write>(&mut self, w: &mut W, keep_all: bool) -> R<u64> {
        let n = self.hist.len() - self.flushed;
        w.write_all(&self.hist[self.flushed..])?;
        self.flushed = self.hist.len();
        if !keep_all && self.hist.len() > self.window * 2 + (1 << 20) {
            let drop = self.hist.len() - self.window;
            self.hist.drain(..drop);
            self.flushed = self.hist.len();
        }
        Ok(n as u64)
    }

    fn literals(&mut self, b: &[u8]) -> R<(Vec<u8>, usize)> {
        let b0 = *b.first().ok_or_else(|| bad("empty literals section"))?;
        let (kind, fmt) = (b0 & 3, (b0 >> 2) & 3);
        match kind {
            0 | 1 => {
                let (size, hdr) = match fmt {
                    0 | 2 => ((b0 >> 3) as usize, 1),
                    1 => (((b0 >> 4) as usize) | ((*b.get(1).ok_or_else(|| bad("literals header cut short"))? as usize) << 4), 2),
                    _ => {
                        let x = b.get(1..3).ok_or_else(|| bad("literals header cut short"))?;
                        (((b0 >> 4) as usize) | ((x[0] as usize) << 4) | ((x[1] as usize) << 12), 3)
                    }
                };
                if size > MAX_BLOCK {
                    return Err(bad("literals larger than a block"));
                }
                if kind == 0 {
                    Ok((b.get(hdr..hdr + size).ok_or_else(|| bad("raw literals cut short"))?.to_vec(), hdr + size))
                } else {
                    let v = *b.get(hdr).ok_or_else(|| bad("RLE literal missing"))?;
                    Ok((vec![v; size], hdr + 1))
                }
            }
            _ => {
                let (nstreams, rbits, hdr) = match fmt {
                    0 => (1, 10, 3),
                    1 => (4, 10, 3),
                    2 => (4, 14, 4),
                    _ => (4, 18, 5),
                };
                let mut v = 0u64;
                for (i, x) in b.get(..hdr).ok_or_else(|| bad("literals header cut short"))?.iter().enumerate() {
                    v |= (*x as u64) << (8 * i);
                }
                v >>= 4;
                let regen = (v & ((1 << rbits) - 1)) as usize;
                let comp = ((v >> rbits) & ((1 << rbits) - 1)) as usize;
                if regen > MAX_BLOCK {
                    return Err(bad("literals larger than a block"));
                }
                let body = b.get(hdr..hdr + comp).ok_or_else(|| bad("compressed literals cut short"))?;
                let mut at = 0;
                if kind == 2 {
                    let (h, used) = read_huff(body)?;
                    self.huff = Some(h);
                    at = used;
                }
                let h = self.huff.as_ref().ok_or_else(|| bad("treeless literals with no tree before"))?;
                let data = &body[at..];
                let mut out = Vec::with_capacity(regen);
                if nstreams == 1 {
                    huff_stream(h, data, regen, &mut out)?;
                } else {
                    if data.len() < 6 {
                        return Err(bad("Huffman jump table cut short"));
                    }
                    let s1 = u16::from_le_bytes([data[0], data[1]]) as usize;
                    let s2 = u16::from_le_bytes([data[2], data[3]]) as usize;
                    let s3 = u16::from_le_bytes([data[4], data[5]]) as usize;
                    let rest = &data[6..];
                    if s1 + s2 + s3 > rest.len() {
                        return Err(bad("Huffman streams overrun their section"));
                    }
                    let per = regen.div_ceil(4);
                    let parts = [&rest[..s1], &rest[s1..s1 + s2], &rest[s1 + s2..s1 + s2 + s3], &rest[s1 + s2 + s3..]];
                    for (i, p) in parts.iter().enumerate() {
                        let n = if i < 3 { per } else { regen.saturating_sub(per * 3) };
                        huff_stream(h, p, n, &mut out)?;
                    }
                }
                Ok((out, hdr + comp))
            }
        }
    }

    fn block(&mut self, b: &[u8]) -> R<()> {
        let (lits, used) = self.literals(b)?;
        let mut at = used;
        let b0 = *b.get(at).ok_or_else(|| bad("sequences section missing"))? as usize;
        at += 1;
        let nseq = if b0 == 0 {
            0
        } else if b0 < 128 {
            b0
        } else if b0 < 255 {
            let b1 = *b.get(at).ok_or_else(|| bad("sequence count cut short"))? as usize;
            at += 1;
            ((b0 - 128) << 8) + b1
        } else {
            let x = b.get(at..at + 2).ok_or_else(|| bad("sequence count cut short"))?;
            at += 2;
            x[0] as usize + ((x[1] as usize) << 8) + 0x7F00
        };
        if nseq == 0 {
            self.hist.extend_from_slice(&lits);
            return Ok(());
        }
        let modes = *b.get(at).ok_or_else(|| bad("sequence modes missing"))?;
        at += 1;
        if modes & 3 != 0 {
            return Err(bad("reserved sequence mode bits set"));
        }
        let ll = fse_for(modes >> 6, b, &mut at, &LL_DEFAULT, 6, 35, 9, &self.ll)?;
        let of = fse_for((modes >> 4) & 3, b, &mut at, &OF_DEFAULT, 5, 31, 8, &self.of)?;
        let ml = fse_for((modes >> 2) & 3, b, &mut at, &ML_DEFAULT, 6, 52, 9, &self.ml)?;
        self.ll = Some(ll.clone());
        self.of = Some(of.clone());
        self.ml = Some(ml.clone());
        let mut br = Back::new(b.get(at..).ok_or_else(|| bad("sequence stream missing"))?)?;
        let (mut sl, mut so, mut sm) = (br.read(ll.al) as usize, br.read(of.al) as usize, br.read(ml.al) as usize);
        let mut lit_at = 0usize;
        for i in 0..nseq {
            let (oe, me, le) = (of.t[so], ml.t[sm], ll.t[sl]);
            if oe.symbol as u32 > 31 || me.symbol as usize >= ML_BASE.len() || le.symbol as usize >= LL_BASE.len() {
                return Err(bad("sequence code out of range"));
            }
            let ofv = (1u64 << oe.symbol) + br.read(oe.symbol as u32);
            let (mb, mbits) = ML_BASE[me.symbol as usize];
            let mlen = (mb as u64 + br.read(mbits)) as usize;
            let (lb, lbits) = LL_BASE[le.symbol as usize];
            let llen = (lb as u64 + br.read(lbits)) as usize;
            if i + 1 < nseq {
                sl = le.base as usize + br.read(le.nbits as u32) as usize;
                sm = me.base as usize + br.read(me.nbits as u32) as usize;
                so = oe.base as usize + br.read(oe.nbits as u32) as usize;
            }
            // the offset
            let offset: u32 = if ofv > 3 {
                let o = (ofv - 3) as u32;
                self.rep = [o, self.rep[0], self.rep[1]];
                o
            } else {
                let idx = ofv as usize - 1 + if llen == 0 { 1 } else { 0 };
                let o = if idx == 3 {
                    if self.rep[0] <= 1 {
                        return Err(bad("repeat offset of zero"));
                    }
                    self.rep[0] - 1
                } else {
                    self.rep[idx]
                };
                match idx {
                    0 => {}
                    1 => self.rep = [self.rep[1], self.rep[0], self.rep[2]],
                    2 => self.rep = [self.rep[2], self.rep[0], self.rep[1]],
                    _ => self.rep = [o, self.rep[0], self.rep[1]],
                }
                o
            };
            if lit_at + llen > lits.len() {
                return Err(bad("sequence takes more literals than there are"));
            }
            self.hist.extend_from_slice(&lits[lit_at..lit_at + llen]);
            lit_at += llen;
            let off = offset as usize;
            let avail = self.hist.len();
            if off == 0 || off > avail || off > self.window.max(1) && avail - self.flushed < off {
                // an offset beyond what has been kept (window or start) is corrupt
                if off == 0 || off > avail {
                    return Err(bad("match offset reaches before the start of the data"));
                }
            }
            if self.hist.len() - self.flushed + mlen > (1 << 29) {
                return Err(bad("block expands implausibly"));
            }
            let start = avail - off;
            if off >= mlen {
                self.hist.extend_from_within(start..start + mlen);
            } else {
                for k in 0..mlen {
                    let v = self.hist[start + k];
                    self.hist.push(v);
                }
            }
        }
        self.hist.extend_from_slice(&lits[lit_at..]);
        Ok(())
    }

    /// Decodes every frame of `r` into `w`; returns the bytes written. Stops at the end of the input.
    pub fn decode<Rd: Read, W: Write>(&mut self, r: &mut Rd, w: &mut W) -> R<u64> {
        let mut total = 0u64;
        loop {
            let mut m = [0u8; 4];
            match read_full(r, &mut m)? {
                0 => return Ok(total),
                4 => {}
                _ => return Err(bad("input ends inside a frame header")),
            }
            let magic = u32::from_le_bytes(m);
            if magic & 0xFFFF_FFF0 == 0x184D_2A50 {
                let mut sz = [0u8; 4];
                r.read_exact(&mut sz)?;
                io::copy(&mut r.by_ref().take(u32::from_le_bytes(sz) as u64), &mut io::sink())?;
                continue;
            }
            if magic != 0xFD2F_B528 {
                return Err(bad("not a zstd frame"));
            }
            total += self.frame(r, w)?;
        }
    }

    fn frame<Rd: Read, W: Write>(&mut self, r: &mut Rd, w: &mut W) -> R<u64> {
        let mut fd = [0u8; 1];
        r.read_exact(&mut fd)?;
        let fd = fd[0];
        if fd & 0x08 != 0 {
            return Err(bad("reserved frame header bit set"));
        }
        let (fcs_flag, single, checksum, dict) = (fd >> 6, fd & 0x20 != 0, fd & 4 != 0, fd & 3);
        let mut window = 0u64;
        if !single {
            let mut wd = [0u8; 1];
            r.read_exact(&mut wd)?;
            let base = 1u64 << (10 + (wd[0] >> 3));
            window = base + (base / 8) * (wd[0] & 7) as u64;
        }
        let dict_len = [0, 1, 2, 4][dict as usize];
        let mut dd = [0u8; 4];
        r.read_exact(&mut dd[..dict_len])?;
        if dd.iter().any(|&b| b != 0) {
            return Err(bad("dictionaries are not supported"));
        }
        let fcs_len = match fcs_flag {
            0 => single as usize,
            1 => 2,
            2 => 4,
            _ => 8,
        };
        let mut fb = [0u8; 8];
        r.read_exact(&mut fb[..fcs_len])?;
        let mut fcs = u64::from_le_bytes(fb);
        if fcs_flag == 1 {
            fcs += 256;
        }
        if single {
            window = fcs;
        }
        if window > MAX_WINDOW {
            return Err(bad("window larger than this decoder accepts"));
        }
        self.new_frame(window.max(1) as usize);
        self.hist.clear();
        self.flushed = 0;
        let mut written = 0u64;
        loop {
            let mut bh = [0u8; 3];
            r.read_exact(&mut bh)?;
            let h = bh[0] as u32 | (bh[1] as u32) << 8 | (bh[2] as u32) << 16;
            let (last, kind, size) = (h & 1 != 0, (h >> 1) & 3, (h >> 3) as usize);
            match kind {
                0 | 2 => {
                    if size > MAX_BLOCK {
                        return Err(bad("block larger than 128 KiB"));
                    }
                    let mut body = vec![0u8; size];
                    r.read_exact(&mut body)?;
                    if kind == 0 {
                        self.hist.extend_from_slice(&body);
                    } else {
                        self.block(&body)?;
                    }
                }
                1 => {
                    if size > MAX_BLOCK {
                        return Err(bad("block larger than 128 KiB"));
                    }
                    let mut v = [0u8; 1];
                    r.read_exact(&mut v)?;
                    self.hist.resize(self.hist.len() + size, v[0]);
                }
                _ => return Err(bad("reserved block type")),
            }
            written += self.flush(w, false)?;
            if last {
                break;
            }
        }
        if checksum {
            let mut c = [0u8; 4];
            r.read_exact(&mut c)?;
        }
        if single && written != fcs {
            return Err(bad("frame content size does not match the data"));
        }
        Ok(written)
    }
}

fn read_full<Rd: Read>(r: &mut Rd, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Decodes all frames of `r` into `w`.
pub fn decompress<Rd: Read, W: Write>(r: &mut Rd, w: &mut W) -> io::Result<u64> {
    Decoder::default().decode(r, w)
}

/// Decodes a single in-memory buffer.
pub fn decompress_vec(d: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    decompress(&mut &d[..], &mut out)?;
    Ok(out)
}
