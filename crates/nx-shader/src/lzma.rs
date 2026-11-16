//! LZMA1 decoder (raw stream, the size known), the compression of Unity's bundle block tables. Written from the format, std only.

struct Rc<'a> {
    d: &'a [u8],
    p: usize,
    range: u32,
    code: u32,
    over: bool,
}

impl<'a> Rc<'a> {
    fn new(d: &'a [u8]) -> Result<Self, String> {
        if d.len() < 5 || d[0] != 0 {
            return Err("lzma: bad stream start".into());
        }
        Ok(Rc { d, p: 5, range: 0xFFFF_FFFF, code: u32::from_be_bytes([d[1], d[2], d[3], d[4]]), over: false })
    }
    fn norm(&mut self) {
        if self.range < 1 << 24 {
            self.range <<= 8;
            let b = match self.d.get(self.p) {
                Some(&b) => b,
                None => {
                    self.over = true;
                    0
                }
            };
            self.p += 1;
            self.code = self.code << 8 | b as u32;
        }
    }
    fn bit(&mut self, prob: &mut u16) -> u32 {
        self.norm();
        let bound = (self.range >> 11) * *prob as u32;
        if self.code < bound {
            self.range = bound;
            *prob += (2048 - *prob) >> 5;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *prob -= *prob >> 5;
            1
        }
    }
    fn tree(&mut self, probs: &mut [u16], bits: u32) -> u32 {
        let mut m = 1usize;
        for _ in 0..bits {
            m = m << 1 | self.bit(&mut probs[m]) as usize;
        }
        (m - (1 << bits)) as u32
    }
    fn rtree(&mut self, probs: &mut [u16], bits: u32) -> u32 {
        let (mut m, mut sym) = (1usize, 0u32);
        for i in 0..bits {
            let b = self.bit(&mut probs[m]);
            m = m << 1 | b as usize;
            sym |= b << i;
        }
        sym
    }
    fn direct(&mut self, n: u32) -> u32 {
        let mut r = 0u32;
        for _ in 0..n {
            self.norm();
            self.range >>= 1;
            self.code = self.code.wrapping_sub(self.range);
            let t = 0u32.wrapping_sub(self.code >> 31);
            self.code = self.code.wrapping_add(self.range & t);
            r = (r << 1).wrapping_add(t.wrapping_add(1));
        }
        r
    }
}

struct Len {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; 16],
    mid: [[u16; 8]; 16],
    high: [u16; 256],
}

impl Len {
    fn new() -> Self {
        Len { choice: 1024, choice2: 1024, low: [[1024; 8]; 16], mid: [[1024; 8]; 16], high: [1024; 256] }
    }
    fn decode(&mut self, rc: &mut Rc, ps: usize) -> usize {
        if rc.bit(&mut self.choice) == 0 {
            rc.tree(&mut self.low[ps], 3) as usize
        } else if rc.bit(&mut self.choice2) == 0 {
            8 + rc.tree(&mut self.mid[ps], 3) as usize
        } else {
            16 + rc.tree(&mut self.high, 8) as usize
        }
    }
}

/// Decodes `data` (the stream after Unity's 5 property bytes) with `props` (lc + 9 lp + 45 pb) to exactly `size` bytes
pub fn decode(props: u8, data: &[u8], size: usize) -> Result<Vec<u8>, String> {
    if props >= 9 * 5 * 5 {
        return Err("lzma: bad properties".into());
    }
    let (lc, lp, pb) = ((props % 9) as u32, ((props / 9) % 5) as u32, (props / 45) as u32);
    let mut rc = Rc::new(data)?;
    let mut lit = vec![1024u16; 0x300 << (lc + lp)];
    let mut is_match = [[1024u16; 16]; 12];
    let (mut is_rep, mut g0, mut g1, mut g2) = ([1024u16; 12], [1024u16; 12], [1024u16; 12], [1024u16; 12]);
    let mut rep0_long = [[1024u16; 16]; 12];
    let mut slot = [[1024u16; 64]; 4];
    let mut special = [1024u16; 115];
    let mut align = [1024u16; 16];
    let (mut mlen, mut rlen) = (Len::new(), Len::new());
    let mut out: Vec<u8> = Vec::with_capacity(size);
    let (mut state, mut rep0, mut rep1, mut rep2, mut rep3) = (0usize, 0usize, 0usize, 0usize, 0usize);
    while out.len() < size {
        let ps = out.len() & ((1 << pb) - 1);
        if rc.bit(&mut is_match[state][ps]) == 0 {
            let prev = out.last().copied().unwrap_or(0) as usize;
            let ls = ((out.len() & ((1 << lp) - 1)) << lc) + (prev >> (8 - lc));
            let probs = &mut lit[0x300 * ls..0x300 * (ls + 1)];
            let mut sym = 1usize;
            if state >= 7 {
                let mut mb = *out.get(out.len().wrapping_sub(rep0 + 1)).ok_or("lzma: distance too far")? as usize;
                let mut offs = 0x100usize;
                while sym < 0x100 {
                    mb <<= 1;
                    let mbit = mb & offs;
                    let b = rc.bit(&mut probs[offs + mbit + sym]) as usize;
                    sym = sym << 1 | b;
                    if b == 0 {
                        offs &= !mbit;
                    } else {
                        offs &= mbit;
                    }
                }
            } else {
                while sym < 0x100 {
                    sym = sym << 1 | rc.bit(&mut probs[sym]) as usize;
                }
            }
            out.push(sym as u8);
            state = if state < 4 { 0 } else if state < 10 { state - 3 } else { state - 6 };
            continue;
        }
        let len;
        if rc.bit(&mut is_rep[state]) == 0 {
            rep3 = rep2;
            rep2 = rep1;
            rep1 = rep0;
            let l = mlen.decode(&mut rc, ps);
            state = if state < 7 { 7 } else { 10 };
            let s = rc.tree(&mut slot[l.min(3)], 6) as usize;
            if s < 4 {
                rep0 = s;
            } else {
                let nd = (s >> 1) as u32 - 1;
                let mut d = (2 | (s & 1)) << nd;
                if s < 14 {
                    d += rc.rtree(&mut special[d - s..], nd) as usize;
                } else {
                    d += (rc.direct(nd - 4) as usize) << 4;
                    d += rc.rtree(&mut align, 4) as usize;
                }
                rep0 = d;
                if rep0 == 0xFFFF_FFFF {
                    break; // end marker
                }
            }
            len = l + 2;
        } else {
            if out.is_empty() {
                return Err("lzma: repeat with no data".into());
            }
            if rc.bit(&mut g0[state]) == 0 {
                if rc.bit(&mut rep0_long[state][ps]) == 0 {
                    state = if state < 7 { 9 } else { 11 };
                    let b = *out.get(out.len().wrapping_sub(rep0 + 1)).ok_or("lzma: distance too far")?;
                    out.push(b);
                    continue;
                }
            } else {
                let dist;
                if rc.bit(&mut g1[state]) == 0 {
                    dist = rep1;
                } else {
                    if rc.bit(&mut g2[state]) == 0 {
                        dist = rep2;
                    } else {
                        dist = rep3;
                        rep3 = rep2;
                    }
                    rep2 = rep1;
                }
                rep1 = rep0;
                rep0 = dist;
            }
            len = rlen.decode(&mut rc, ps) + 2;
            state = if state < 7 { 8 } else { 11 };
        }
        if rep0 >= out.len() {
            return Err("lzma: distance too far".into());
        }
        for _ in 0..len.min(size - out.len()) {
            let b = out[out.len() - rep0 - 1];
            out.push(b);
        }
    }
    if rc.over {
        return Err("lzma: stream is cut short".into());
    }
    if out.len() != size {
        return Err(format!("lzma: {} bytes where {size} were expected", out.len()));
    }
    Ok(out)
}

/// Unity's LZMA: 5 property bytes (lc/lp/pb, dictionary size) and the raw stream
pub fn unity(raw: &[u8], size: usize) -> Result<Vec<u8>, String> {
    if raw.len() < 5 {
        return Err("lzma: no properties".into());
    }
    decode(raw[0], &raw[5..], size)
}

#[cfg(test)]
mod tests {
    fn gen(n: usize) -> Vec<u8> {
        let mut s: u64 = 12345;
        let mut out: Vec<u8> = vec![];
        let words: [&[u8]; 6] = [b"shader ", b"vertex ", b"fragment ", b"Unity ", b"\x00\x01\x02\x03", b"AABB\0"];
        while out.len() < n {
            s = (s * 1103515245 + 12345) & 0x7fffffff;
            if s % 7 == 0 {
                for _ in 0..1 + s % 13 {
                    out.push(((s >> 8) & 255) as u8);
                }
            } else {
                out.extend(words[((s >> 4) % 6) as usize]);
            }
        }
        out.truncate(n);
        out
    }

    #[test]
    fn matches_the_reference_encoder() {
        for (name, n, bytes) in [("a", 20097, &include_bytes!("../testdata/lzma_a.bin")[..]), ("b", 20098, &include_bytes!("../testdata/lzma_b.bin")[..]), ("c", 20099, &include_bytes!("../testdata/lzma_c.bin")[..])] {
            let got = super::unity(bytes, n).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(got, gen(n), "{name}");
        }
        assert!(super::unity(&include_bytes!("../testdata/lzma_a.bin")[..100], 20097).is_err());
    }
}
