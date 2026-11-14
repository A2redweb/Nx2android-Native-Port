//! The game's name and version from `control.nacp`.
//!
//! The NACP starts with 16 language entries of 0x300 bytes (name 0x200, publisher 0x100); the display version is at 0x3060. NACPs from newer SDKs compress those 0x3000 bytes:
//! a u16 size, then raw deflate, inflating to 32 languages x 0x300 (upstream's `TitleName`). The inflater below is a small one (as in zlib's `puff.c`) so no crate is needed.

fn bad<T>(m: &'static str) -> Result<T, &'static str> {
    Err(m)
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    bit: u32,
    cnt: u32,
}

impl Bits<'_> {
    fn get(&mut self, n: u32) -> Result<u32, &'static str> {
        while self.cnt < n {
            let b = *self.d.get(self.pos).ok_or("deflate data ends early")? as u32;
            self.pos += 1;
            self.bit |= b << self.cnt;
            self.cnt += 8;
        }
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.cnt -= n;
        Ok(v)
    }
}

struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}

fn build(lengths: &[u8]) -> Result<Huff, &'static str> {
    let mut count = [0u16; 16];
    for &l in lengths {
        count[l as usize] += 1;
    }
    let mut left: i32 = 1;
    for len in 1..16 {
        left = (left << 1) - count[len] as i32;
        if left < 0 {
            return bad("over-subscribed deflate code");
        }
    }
    let mut offs = [0u16; 16];
    for len in 1..15 {
        offs[len + 1] = offs[len] + count[len];
    }
    let mut symbol = vec![0u16; lengths.len()];
    for (s, &l) in lengths.iter().enumerate() {
        if l != 0 {
            symbol[offs[l as usize] as usize] = s as u16;
            offs[l as usize] += 1;
        }
    }
    Ok(Huff { count, symbol })
}

fn decode(b: &mut Bits, h: &Huff) -> Result<u16, &'static str> {
    let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
    for len in 1..16 {
        code |= b.get(1)? as i32;
        let c = h.count[len] as i32;
        if code - c < first {
            return Ok(h.symbol[(index + (code - first)) as usize]);
        }
        index += c;
        first += c;
        first <<= 1;
        code <<= 1;
    }
    bad("invalid deflate code")
}

const LBASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEXT: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DBASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DEXT: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// Inflates raw deflate data, refusing to produce more than `limit` bytes.
pub fn inflate(d: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    let mut b = Bits { d, pos: 0, bit: 0, cnt: 0 };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = b.get(1)?;
        match b.get(2)? {
            0 => {
                b.bit = 0;
                b.cnt = 0;
                let h = d.get(b.pos..b.pos + 4).ok_or("stored block header cut short")?;
                let (len, nlen) = (u16::from_le_bytes([h[0], h[1]]), u16::from_le_bytes([h[2], h[3]]));
                if len != !nlen {
                    return bad("stored block length check failed");
                }
                b.pos += 4;
                let body = d.get(b.pos..b.pos + len as usize).ok_or("stored block cut short")?;
                if out.len() + body.len() > limit {
                    return bad("inflated data too large");
                }
                out.extend_from_slice(body);
                b.pos += len as usize;
            }
            t @ (1 | 2) => {
                let (lit, dist) = if t == 1 {
                    let mut l = [8u8; 288];
                    l[144..256].fill(9);
                    l[256..280].fill(7);
                    (build(&l)?, build(&[5u8; 30])?)
                } else {
                    let (nlen, ndist, ncode) = (b.get(5)? as usize + 257, b.get(5)? as usize + 1, b.get(4)? as usize + 4);
                    if nlen > 286 || ndist > 30 {
                        return bad("too many deflate codes");
                    }
                    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                    let mut cl = [0u8; 19];
                    for &o in ORDER.iter().take(ncode) {
                        cl[o] = b.get(3)? as u8;
                    }
                    let clh = build(&cl)?;
                    let mut lens = vec![0u8; nlen + ndist];
                    let mut i = 0;
                    while i < nlen + ndist {
                        let s = decode(&mut b, &clh)?;
                        if s < 16 {
                            lens[i] = s as u8;
                            i += 1;
                        } else {
                            let (v, rep) = match s {
                                16 => (*lens.get(i.wrapping_sub(1)).ok_or("repeat with nothing before")?, 3 + b.get(2)?),
                                17 => (0, 3 + b.get(3)?),
                                _ => (0, 11 + b.get(7)?),
                            };
                            if i + rep as usize > nlen + ndist {
                                return bad("deflate length repeat overruns");
                            }
                            for _ in 0..rep {
                                lens[i] = v;
                                i += 1;
                            }
                        }
                    }
                    (build(&lens[..nlen])?, build(&lens[nlen..])?)
                };
                loop {
                    let s = decode(&mut b, &lit)? as usize;
                    if s < 256 {
                        if out.len() >= limit {
                            return bad("inflated data too large");
                        }
                        out.push(s as u8);
                    } else if s == 256 {
                        break;
                    } else {
                        let s = s - 257;
                        if s >= 29 {
                            return bad("invalid length code");
                        }
                        let len = LBASE[s] as usize + b.get(LEXT[s] as u32)? as usize;
                        let ds = decode(&mut b, &dist)? as usize;
                        if ds >= 30 {
                            return bad("invalid distance code");
                        }
                        let dd = DBASE[ds] as usize + b.get(DEXT[ds] as u32)? as usize;
                        if dd > out.len() {
                            return bad("distance reaches before the data");
                        }
                        if out.len() + len > limit {
                            return bad("inflated data too large");
                        }
                        for _ in 0..len {
                            let v = out[out.len() - dd];
                            out.push(v);
                        }
                    }
                }
            }
            _ => return bad("invalid deflate block type"),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Nacp {
    pub name: String,
    pub display_version: String,
}

fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// The first non-empty language name and the display version of a `control.nacp`.
pub fn parse(nacp: &[u8]) -> Nacp {
    let n = nacp.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).unwrap_or(0);
    let inflated = if n > 0 && n <= 0x2ffe { nacp.get(2..2 + n).and_then(|z| inflate(z, 0x6000).ok()).filter(|v| v.len() == 0x6000) } else { None };
    let titles: &[u8] = inflated.as_deref().unwrap_or(nacp); // a wrong length means an uncompressed name that happened to inflate
    let langs = if inflated.is_some() { 32 } else { 16 };
    let name = (0..langs).map(|i| titles.get(i * 0x300..i * 0x300 + 0x200).map(text).unwrap_or_default()).find(|n| !n.is_empty()).unwrap_or_default();
    Nacp { name, display_version: nacp.get(0x3060..0x3070).map(text).unwrap_or_default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORED: &[u8] = &[1, 18, 0, 237, 255, 104, 101, 108, 108, 111, 32, 115, 116, 111, 114, 101, 100, 32, 98, 108, 111, 99, 107];
    const FIXED: &[u8] = &[75, 76, 74, 78, 68, 69, 0];
    const DYNAMIC: &[u8] = &[
        11, 46, 45, 72, 45, 82, 240, 77, 44, 202, 204, 87, 240, 79, 169, 44, 46, 78, 173, 84, 8, 30, 21, 27, 21, 27, 21, 3, 139, 53, 109, 231, 123, 87, 47, 21, 96, 185, 239, 67, 221, 33, 147, 122, 182, 188,
        11, 253, 177, 199, 3, 85, 220, 31, 59, 56, 51, 48, 101, 231, 133, 132, 78, 89, 144, 154, 17, 155, 114, 100, 6, 247, 142, 43, 33, 94, 237, 138, 43, 103, 49, 174, 85, 156, 183, 117, 206, 183, 133, 113,
        223, 62, 250, 101, 230, 149, 164, 230, 165, 228, 43, 140, 50, 82, 242, 21, 0,
    ];

    const COMPRESSED: &[u8] = &[237,208,49,13,0,32,12,0,176,41,228,230,229,224,156,0,166,30,27,144,181,18,26,1,0,208,203,88,185,45,0,0,0,0,0,0,0,0,0,0,0,0,240,170,121,74,2,0,0,0,0,0,0,0,0,124,234,2];

    fn adler(d: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &x in d {
            a = (a + x as u32) % 65521;
            b = (b + a) % 65521;
        }
        b << 16 | a
    }

    #[test]
    fn inflates_zlib_streams() {
        assert_eq!(inflate(STORED, 100).unwrap(), b"hello stored block");
        assert_eq!(inflate(FIXED, 100).unwrap(), b"abcabcabcabcabcabc");
        let d = inflate(DYNAMIC, 5000).unwrap(); // made by zlib level 9: text, 60 random bytes, text
        assert_eq!((d.len(), adler(&d)), (1130, 3832784695));
        assert!(d.starts_with(b"Super Mario Odyssey Super Mario Odyssey ") && d.ends_with(b"Nintendo Nintendo "));
    }

    #[test]
    fn inflate_refuses_damage_and_bombs() {
        assert!(inflate(DYNAMIC, 1000).is_err()); // limit
        assert!(inflate(&DYNAMIC[..50], 5000).is_err());
        assert!(inflate(&[0x07], 10).is_err()); // reserved block type
        assert!(inflate(&[0x01, 5, 0, 0, 0], 10).is_err()); // stored length check
        for i in (0..DYNAMIC.len()).step_by(7) {
            let mut x = DYNAMIC.to_vec();
            x[i] ^= 0xA5;
            let _ = inflate(&x, 5000); // an error or other data, never a panic
        }
    }

    #[test]
    fn names_plain_and_compressed() {
        let mut n = vec![0u8; 0x4000];
        n[0x300..0x300 + 9].copy_from_slice(b"Zelda Two"); // language 1; language 0 is empty
        n[0x3060..0x3063].copy_from_slice(b"1.2");
        assert_eq!(parse(&n), Nacp { name: "Zelda Two".into(), display_version: "1.2".into() });
        // compressed (zlib level 9 of 32 language slots: "Name" in slot 3, "Xyz" in slot 20): u16 size, raw deflate, then the rest of the NACP
        let mut c = vec![0u8; 0x4000];
        c[..2].copy_from_slice(&(COMPRESSED.len() as u16).to_le_bytes());
        c[2..2 + COMPRESSED.len()].copy_from_slice(COMPRESSED);
        c[0x3060..0x3064].copy_from_slice(b"2.0\0");
        assert_eq!(parse(&c), Nacp { name: "Name".into(), display_version: "2.0".into() });
        assert_eq!(inflate(COMPRESSED, 0x6000).unwrap()[20 * 0x300..20 * 0x300 + 3], *b"Xyz");
        assert_eq!(parse(&[]), Nacp::default());
    }
}
