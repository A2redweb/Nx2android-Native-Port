//! NSO (and NRO) modules: header, LZ4 block decompression, the memory image the rewriter works on, and the build ID.
//!
//! Header, as read by upstream's `patch_nso.py`: magic `NSO0`, flags at 0xC (bit i: segment i is LZ4-compressed; bit 3+i: its SHA-256 is to be checked), three segments (text, ro, data) as
//! {file offset, memory offset, decompressed size} at 0x10 (0x10 bytes each), the bss size at 0x3C, the 32-byte build ID at 0x40, the three compressed sizes at 0x60 and the three segment
//! hashes at 0xA0. An NRO (a module the game loads at run time) is already an image: segments {offset, size} at 0x20, bss size at 0x38, build ID at 0x40.

use nx_crypto::Sha256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub file_offset: u32,
    pub mem_offset: u32,
    pub size: u32,
    pub compressed_size: u32,
    pub compressed: bool,
    pub hashed: bool,
    pub hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nso {
    pub is_nro: bool,
    pub segments: [Segment; 3],
    pub bss: u32,
    pub build_id: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    NotAnNso,
    Truncated(&'static str),
    Lz4(&'static str),
    HashMismatch(usize),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotAnNso => f.write_str("not an NSO0 or NRO0 file"),
            Error::Truncated(w) => write!(f, "NSO is cut short ({w})"),
            Error::Lz4(w) => write!(f, "LZ4 data is damaged ({w})"),
            Error::HashMismatch(i) => write!(f, "segment {i} does not match its SHA-256"),
        }
    }
}

impl std::error::Error for Error {}

fn u32le(d: &[u8], o: usize) -> Result<u32, Error> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or(Error::Truncated("header"))
}

impl Nso {
    pub fn parse(d: &[u8]) -> Result<Nso, Error> {
        if d.len() >= 0x14 && &d[0x10..0x14] == b"NRO0" {
            let mut segments = [Segment { file_offset: 0, mem_offset: 0, size: 0, compressed_size: 0, compressed: false, hashed: false, hash: [0; 32] }; 3];
            for (i, s) in segments.iter_mut().enumerate() {
                let (o, sz) = (u32le(d, 0x20 + 8 * i)?, u32le(d, 0x24 + 8 * i)?);
                *s = Segment { file_offset: o, mem_offset: o, size: sz, compressed_size: sz, compressed: false, hashed: false, hash: [0; 32] };
            }
            let build_id = d.get(0x40..0x60).ok_or(Error::Truncated("build id"))?.try_into().unwrap();
            return Ok(Nso { is_nro: true, segments, bss: u32le(d, 0x38)?, build_id });
        }
        if d.get(..4) != Some(b"NSO0") {
            return Err(Error::NotAnNso);
        }
        let flags = u32le(d, 0xC)?;
        let mut segments = [Segment { file_offset: 0, mem_offset: 0, size: 0, compressed_size: 0, compressed: false, hashed: false, hash: [0; 32] }; 3];
        for (i, s) in segments.iter_mut().enumerate() {
            *s = Segment {
                file_offset: u32le(d, 0x10 + i * 0x10)?,
                mem_offset: u32le(d, 0x14 + i * 0x10)?,
                size: u32le(d, 0x18 + i * 0x10)?,
                compressed_size: u32le(d, 0x60 + i * 4)?,
                compressed: flags >> i & 1 != 0,
                hashed: flags >> (3 + i) & 1 != 0,
                hash: d.get(0xA0 + i * 0x20..0xC0 + i * 0x20).ok_or(Error::Truncated("segment hashes"))?.try_into().unwrap(),
            };
        }
        let build_id = d.get(0x40..0x60).ok_or(Error::Truncated("build id"))?.try_into().unwrap();
        Ok(Nso { is_nro: false, segments, bss: u32le(d, 0x3C)?, build_id })
    }

    /// The build ID as the lowercase hex of its first 16 bytes, which is how upstream prints and keys it.
    pub fn build_id_hex(&self) -> String {
        self.build_id[..16].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The decompressed bytes of segment `i`. Checks its SHA-256 when the header asks for it.
    pub fn segment(&self, d: &[u8], i: usize) -> Result<Vec<u8>, Error> {
        let s = &self.segments[i];
        let raw = d.get(s.file_offset as usize..(s.file_offset as usize).checked_add(s.compressed_size as usize).ok_or(Error::Truncated("segment"))?).ok_or(Error::Truncated("segment data"))?;
        let out = if s.compressed { lz4_block(raw, s.size as usize)? } else { raw.to_vec() };
        if out.len() != s.size as usize && !self.is_nro {
            return Err(Error::Truncated("segment size"));
        }
        if s.hashed && Sha256::digest(&out) != s.hash {
            return Err(Error::HashMismatch(i));
        }
        Ok(out)
    }

    /// The memory image: text, ro and data placed at their memory offsets, `data end` long (the bss, zero pages the loader supplies, is not included).
    pub fn image(&self, d: &[u8]) -> Result<Vec<u8>, Error> {
        let end = self.segments[2].mem_offset as usize + self.segments[2].size as usize;
        if end > 1 << 31 {
            return Err(Error::Truncated("implausible image size"));
        }
        let mut img = vec![0u8; end];
        for i in 0..3 {
            let seg = self.segment(d, i)?;
            let o = self.segments[i].mem_offset as usize;
            img.get_mut(o..o + seg.len()).ok_or(Error::Truncated("segment outside the image"))?.copy_from_slice(&seg);
        }
        Ok(img)
    }
}

/// Decompresses one LZ4 block (no frame) that must expand to exactly `size` bytes.
pub fn lz4_block(src: &[u8], size: usize) -> Result<Vec<u8>, Error> {
    let mut out: Vec<u8> = Vec::with_capacity(size);
    let mut i = 0;
    let byte = |i: usize| src.get(i).copied().ok_or(Error::Lz4("input ends early"));
    while i < src.len() {
        let token = src[i];
        i += 1;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            loop {
                let b = byte(i)?;
                i += 1;
                lit += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        let l = src.get(i..i.checked_add(lit).ok_or(Error::Lz4("length overflow"))?).ok_or(Error::Lz4("literals run past the input"))?;
        if out.len() + lit > size {
            return Err(Error::Lz4("output larger than declared"));
        }
        out.extend_from_slice(l);
        i += lit;
        if i >= src.len() {
            break; // the last sequence has literals only
        }
        let off = u16::from_le_bytes([byte(i)?, byte(i + 1)?]) as usize;
        i += 2;
        let mut ml = (token & 15) as usize;
        if ml == 15 {
            loop {
                let b = byte(i)?;
                i += 1;
                ml += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        ml += 4;
        if off == 0 || off > out.len() {
            return Err(Error::Lz4("match offset outside the output"));
        }
        if out.len() + ml > size {
            return Err(Error::Lz4("output larger than declared"));
        }
        let start = out.len() - off;
        if off >= ml {
            out.extend_from_within(start..start + ml);
        } else {
            for k in 0..ml {
                let v = out[start + k];
                out.push(v);
            }
        }
    }
    if out.len() != size {
        return Err(Error::Lz4("output shorter than declared"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small greedy LZ4 block compressor, for round trips (the decoder is what ships).
    fn lz4_compress(d: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut table = vec![usize::MAX; 1 << 12];
        let (mut i, mut anchor) = (0, 0);
        let hash = |w: u32| ((w.wrapping_mul(2654435761)) >> 20) as usize;
        let emit = |out: &mut Vec<u8>, lits: &[u8], m: Option<(usize, usize)>| {
            let ml = m.map_or(0, |x| x.1 - 4);
            out.push((lits.len().min(15) as u8) << 4 | ml.min(15) as u8);
            if lits.len() >= 15 {
                let mut r = lits.len() - 15;
                while r >= 255 {
                    out.push(255);
                    r -= 255;
                }
                out.push(r as u8);
            }
            out.extend_from_slice(lits);
            if let Some((off, len)) = m {
                out.extend((off as u16).to_le_bytes());
                if len - 4 >= 15 {
                    let mut r = len - 4 - 15;
                    while r >= 255 {
                        out.push(255);
                        r -= 255;
                    }
                    out.push(r as u8);
                }
            }
        };
        while i + 12 < d.len() {
            let w = u32::from_le_bytes(d[i..i + 4].try_into().unwrap());
            let h = hash(w);
            let c = table[h];
            table[h] = i;
            if c != usize::MAX && i - c < 65536 && d[c..c + 4] == d[i..i + 4] {
                let mut len = 4;
                while i + len < d.len() - 5 && d[c + len] == d[i + len] {
                    len += 1;
                }
                emit(&mut out, &d[anchor..i], Some((i - c, len)));
                i += len;
                anchor = i;
            } else {
                i += 1;
            }
        }
        emit(&mut out, &d[anchor..], None);
        out
    }

    fn sample(n: usize) -> Vec<u8> {
        let mut x = 7u32;
        (0..n)
            .map(|i| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                if (i / 64) % 3 == 0 { (x >> 16) as u8 } else { (i % 17) as u8 }
            })
            .collect()
    }

    #[test]
    fn lz4_hand_made_vectors() {
        // "abcabcabcabc": 3 literals, then a match of length 9 at distance 3 (needs overlap), then the last literals
        let v = [0x35, b'a', b'b', b'c', 3, 0, 0x10, b'!'];
        assert_eq!(lz4_block(&v, 3 + 9 + 1).unwrap(), b"abcabcabcabc!");
        assert_eq!(lz4_block(&[0x00], 0).unwrap(), b""); // a lone empty token
        assert_eq!(lz4_block(&[0x40, 1, 2, 3, 4], 4).unwrap(), [1, 2, 3, 4]);
    }

    #[test]
    fn lz4_round_trips_and_rejects_damage() {
        for n in [0usize, 1, 11, 12, 13, 100, 5000, 70000] {
            let d = sample(n);
            let c = lz4_compress(&d);
            assert_eq!(lz4_block(&c, n).unwrap(), d, "n={n}");
            assert!(lz4_block(&c, n + 1).is_err());
            if n > 0 {
                assert!(lz4_block(&c, n - 1).is_err());
            }
        }
        let c = lz4_compress(&sample(5000));
        for cut in [1, 2, c.len() / 2, c.len() - 1] {
            let _ = lz4_block(&c[..cut], 5000); // must not panic
        }
        assert!(lz4_block(&[0x01, b'a', 5, 0], 10).is_err()); // offset beyond the output
        assert!(lz4_block(&[0x01, b'a', 0, 0, 0], 10).is_err()); // offset 0
    }

    fn make_nso(segs: [&[u8]; 3], compress: [bool; 3], hashed: [bool; 3], bss: u32, bid: u8) -> Vec<u8> {
        let mut h = vec![0u8; 0x100];
        h[..4].copy_from_slice(b"NSO0");
        let flags: u32 = (0..3).map(|i| (compress[i] as u32) << i | (hashed[i] as u32) << (3 + i)).sum();
        h[0xC..0x10].copy_from_slice(&flags.to_le_bytes());
        let mut body = Vec::new();
        let mem = [0u32, 0x4000, 0x8000];
        for i in 0..3 {
            let data = if compress[i] { lz4_compress(segs[i]) } else { segs[i].to_vec() };
            h[0x10 + i * 0x10..0x14 + i * 0x10].copy_from_slice(&(0x100 + body.len() as u32).to_le_bytes());
            h[0x14 + i * 0x10..0x18 + i * 0x10].copy_from_slice(&mem[i].to_le_bytes());
            h[0x18 + i * 0x10..0x1C + i * 0x10].copy_from_slice(&(segs[i].len() as u32).to_le_bytes());
            h[0x60 + i * 4..0x64 + i * 4].copy_from_slice(&(data.len() as u32).to_le_bytes());
            h[0xA0 + i * 0x20..0xC0 + i * 0x20].copy_from_slice(&Sha256::digest(segs[i]));
            body.extend(data);
        }
        h[0x3C..0x40].copy_from_slice(&bss.to_le_bytes());
        h[0x40..0x60].fill(bid);
        h.extend(body);
        h
    }

    #[test]
    fn parses_and_builds_the_image() {
        let (t, r, dd) = (sample(0x3000), sample(0x1000), sample(0x500));
        let f = make_nso([&t, &r, &dd], [true, true, false], [true, true, true], 0x2000, 0xAB);
        let n = Nso::parse(&f).unwrap();
        assert_eq!((n.bss, n.is_nro, n.build_id_hex()), (0x2000, false, "ab".repeat(16)));
        assert_eq!((n.segments[0].compressed, n.segments[2].compressed, n.segments[1].hashed), (true, false, true));
        let img = n.image(&f).unwrap();
        assert_eq!(img.len(), 0x8000 + 0x500);
        assert_eq!(&img[..0x3000], &t[..]);
        assert_eq!(&img[0x4000..0x5000], &r[..]);
        assert_eq!(&img[0x8000..], &dd[..]);
        assert!(img[0x3000..0x4000].iter().all(|&b| b == 0));
    }

    #[test]
    fn hash_and_header_checks() {
        let (t, r, dd) = (sample(0x300), sample(0x100), sample(0x50));
        let mut f = make_nso([&t, &r, &dd], [false; 3], [true; 3], 0, 1);
        let n = Nso::parse(&f).unwrap();
        let last = f.len() - 1;
        f[last] ^= 1; // damage the data segment
        assert_eq!(n.image(&f).unwrap_err(), Error::HashMismatch(2));
        let g = make_nso([&t, &r, &dd], [false; 3], [false; 3], 0, 1);
        let mut g2 = g.clone();
        let l = g2.len() - 1;
        g2[l] ^= 1;
        assert!(Nso::parse(&g2).unwrap().image(&g2).is_ok()); // no hash flag: not checked
        assert_eq!(Nso::parse(b"MZ").unwrap_err(), Error::NotAnNso);
        assert!(Nso::parse(&g[..0x80]).is_err());
        assert!(n.image(&g[..0x120]).is_err()); // segment data cut off
    }

    #[test]
    fn nro_is_an_image_already() {
        let mut f = vec![0u8; 0x100];
        f[0x10..0x14].copy_from_slice(b"NRO0");
        for (i, (o, s)) in [(0x100u32, 0x40u32), (0x140, 0x20), (0x160, 0x10)].into_iter().enumerate() {
            f[0x20 + i * 8..0x24 + i * 8].copy_from_slice(&o.to_le_bytes());
            f[0x24 + i * 8..0x28 + i * 8].copy_from_slice(&s.to_le_bytes());
        }
        f[0x38..0x3C].copy_from_slice(&0x500u32.to_le_bytes());
        f[0x40..0x60].fill(0x77);
        f.extend((0..0x70u32).map(|i| i as u8));
        let n = Nso::parse(&f).unwrap();
        assert!(n.is_nro && n.bss == 0x500);
        let img = n.image(&f).unwrap();
        assert_eq!(img.len(), 0x170);
        assert_eq!(&img[0x100..], &f[0x100..]);
    }
}
