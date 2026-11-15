//! File containers the shader readers look through: Yaz0, SARC, zstd (upstream's `shader_split.py` yaz0, sarc_members, unzs).

pub const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

fn u16le(d: &[u8], o: usize) -> Result<u32, String> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()) as u32).ok_or_else(|| format!("read past the end at {o:#x}"))
}
pub fn u32le(d: &[u8], o: usize) -> Result<u32, String> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("read past the end at {o:#x}"))
}
pub fn u64le(d: &[u8], o: usize) -> Result<u64, String> {
    d.get(o..o + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("read past the end at {o:#x}"))
}

/// Yaz0: header "Yaz0", big-endian decompressed size at +4, data from +16. Group header bits: 1 = one literal byte; 0 = back-reference {length high nibble, 12-bit distance - 1}; length 0 reads one more byte + 0x12, else + 2.
pub fn yaz0(s: &[u8]) -> Result<Vec<u8>, String> {
    let n = u32::from_be_bytes(s.get(4..8).ok_or("Yaz0 header")?.try_into().unwrap()) as usize;
    if n > 1 << 30 {
        return Err(format!("Yaz0 size {n}"));
    }
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let mut si = 16;
    let eof = || "Yaz0 data is cut short".to_string();
    while out.len() < n {
        let mut h = *s.get(si).ok_or_else(eof)?;
        si += 1;
        for _ in 0..8 {
            if h & 0x80 != 0 {
                out.push(*s.get(si).ok_or_else(eof)?);
                si += 1;
            } else {
                let (b1, b2) = (*s.get(si).ok_or_else(eof)? as usize, *s.get(si + 1).ok_or_else(eof)? as usize);
                si += 2;
                let dist = ((b1 & 15) << 8 | b2) + 1;
                let mut ln = b1 >> 4;
                if ln != 0 {
                    ln += 2;
                } else {
                    ln = *s.get(si).ok_or_else(eof)? as usize + 0x12;
                    si += 1;
                }
                if dist > out.len() {
                    return Err("Yaz0 back-reference before the start".into());
                }
                let st = out.len() - dist;
                for k in 0..ln {
                    let b = out[st + k];
                    out.push(b);
                }
            }
            if out.len() >= n {
                break;
            }
            h <<= 1;
        }
    }
    out.truncate(n);
    Ok(out)
}

/// The members of a SARC archive: (name or `#<index>`, bytes). Both are borrowed from the archive.
pub fn sarc_members(d: &[u8]) -> Result<Vec<(String, &[u8])>, String> {
    if d.get(..4) != Some(b"SARC") {
        return Err("not a SARC".into());
    }
    let hl = u16le(d, 4)? as usize;
    let doff = u32le(d, 12)? as usize;
    let n = u16le(d, hl + 6)? as usize;
    let names = hl + 12 + 16 * n + 8;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let o = hl + 12 + 16 * i;
        let (attr, s, e) = (u32le(d, o + 4)?, u32le(d, o + 8)? as usize, u32le(d, o + 12)? as usize);
        let nm = if attr >> 24 != 0 {
            let p = names + (attr & 0xFF_FFFF) as usize * 4;
            let tail = d.get(p..).ok_or("SARC name out of range")?;
            let z = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            String::from_utf8_lossy(&tail[..z]).into_owned()
        } else {
            format!("#{i}")
        };
        out.push((nm, d.get(doff + s..doff + e).ok_or_else(|| format!("SARC member {i} out of range"))?));
    }
    Ok(out)
}

/// A zstd file decompressed (no dictionary: a frame that names one is an error until the decoder supports them).
pub fn unzs(d: &[u8]) -> Result<Vec<u8>, String> {
    nx_zstd::decompress_vec(d).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaz0_literals_and_overlapping_copy() {
        // "ab" + a copy of length 6 at distance 2 → "abababab"
        let mut s = b"Yaz0".to_vec();
        s.extend(8u32.to_be_bytes());
        s.extend([0u8; 8]);
        // group header 0b11000000: two literals, then a reference (len nibble 4 → 6, distance 2 → 0x001), then padding bits 0
        s.extend([0b1100_0000, b'a', b'b', 0x40, 0x01]);
        assert_eq!(yaz0(&s).unwrap(), b"abababab");
        assert!(yaz0(&s[..10]).is_err());
    }

    #[test]
    fn sarc_with_names() {
        // header (0x14) + SFAT (0xC + 16) + SFNT (8 + name "a.bin\0\0\0") + data
        let mut d = b"SARC".to_vec();
        d.extend(0x14u16.to_le_bytes());
        d.extend(0xFEFFu16.to_le_bytes());
        d.extend(0u32.to_le_bytes()); // size (unused by the reader)
        let doff = 0x14 + 12 + 16 + 8 + 8;
        d.extend((doff as u32).to_le_bytes());
        d.extend([0u8; 4]);
        d.extend(b"SFAT");
        d.extend(0xCu16.to_le_bytes());
        d.extend(1u16.to_le_bytes());
        d.extend(0u32.to_le_bytes());
        d.extend(0u32.to_le_bytes()); // hash
        d.extend((1u32 << 24).to_le_bytes()); // attr: has a name at offset 0
        d.extend(0u32.to_le_bytes());
        d.extend(3u32.to_le_bytes());
        d.extend(b"SFNT");
        d.extend([8u8, 0, 0, 0]);
        d.extend(b"a.bin\0\0\0");
        d.extend(b"xyz");
        let m = sarc_members(&d).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].0.as_str(), m[0].1), ("a.bin", &b"xyz"[..]));
    }
}
