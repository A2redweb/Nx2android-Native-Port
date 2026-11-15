//! Function boundaries from `.eh_frame_hdr`, and data blocks in the text (upstream's `nso_code.py`).

use crate::pat::I;

pub const DATA_MIN: i64 = 0x1000;

fn u32at(img: &[u8], o: i64) -> Option<u32> {
    img.get(usize::try_from(o).ok()?..)?.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

/// (eh_frame_hdr offset, its end) from the MOD0 header
pub fn eh_frame_hdr(img: &[u8]) -> Result<(i64, i64), String> {
    let m0 = u32at(img, 4).ok_or("image too small")? as i64;
    if img.get(m0 as usize..m0 as usize + 4) != Some(b"MOD0") {
        return Err("no MOD0".into());
    }
    let a = u32at(img, m0 + 16).ok_or("MOD0 truncated")? as i32 as i64;
    let b = u32at(img, m0 + 20).ok_or("MOD0 truncated")? as i32 as i64;
    Ok((m0 + a, m0 + b))
}

fn read(img: &[u8], o: i64, enc: u8, ehs: i64) -> Result<(i64, i64), String> {
    let get = |n: usize| img.get(o as usize..o as usize + n).ok_or_else(|| format!("eh_frame read past the image at {o:#x}"));
    let (mut v, sz) = match enc & 0xF {
        0x3 => (u32::from_le_bytes(get(4)?.try_into().unwrap()) as i64, 4),
        0xB => (i32::from_le_bytes(get(4)?.try_into().unwrap()) as i64, 4),
        0x0 | 0x4 => (u64::from_le_bytes(get(8)?.try_into().unwrap()) as i64, 8),
        0xC => (i64::from_le_bytes(get(8)?.try_into().unwrap()), 8),
        _ => return Err(format!("pointer encoding {enc:#x} at {o:#x}")),
    };
    match enc & 0x70 {
        0x10 => v += o,
        0x30 => v += ehs,
        _ => {}
    }
    Ok((v, sz))
}

fn uleb(img: &[u8], mut o: i64) -> (u64, i64) {
    let (mut r, mut sh) = (0u64, 0);
    loop {
        let b = img.get(o as usize).copied().unwrap_or(0);
        o += 1;
        r |= ((b & 0x7F) as u64) << sh;
        sh += 7;
        if b & 0x80 == 0 || sh > 63 {
            return (r, o);
        }
    }
}

/// [(function start, FDE offset)] from the binary search table; empty when the module has no valid eh_frame_hdr
pub fn fde_table(img: &[u8]) -> Vec<(i64, i64)> {
    let Ok((ehs, ehe)) = eh_frame_hdr(img) else { return vec![] };
    if ehe - ehs < 12 || ehs < 0 || ehs as usize + 4 > img.len() || img[ehs as usize] != 1 || matches!(img[ehs as usize + 3], 0xFF | 0) {
        return vec![];
    }
    let (penc, cenc, tenc) = (img[ehs as usize + 1], img[ehs as usize + 2], img[ehs as usize + 3]);
    let mut o = ehs + 4;
    let Ok((_, s)) = read(img, o, penc, ehs) else { return vec![] };
    o += s;
    let Ok((n, s)) = read(img, o, cenc, ehs) else { return vec![] };
    o += s;
    let mut out = Vec::new();
    for _ in 0..n.max(0) {
        let (Ok((a, s1)), ) = (read(img, o, tenc, ehs),) else { break };
        o += s1;
        let Ok((f, s2)) = read(img, o, tenc, ehs) else { break };
        o += s2;
        out.push((a, f));
    }
    out
}

/// sorted [(start, end)] of every function the FDEs describe
pub fn fde_ranges(img: &[u8]) -> Vec<(i64, i64)> {
    let Ok((ehs, _)) = eh_frame_hdr(img) else { return vec![] };
    let mut cies: std::collections::HashMap<i64, u8> = Default::default();
    let mut out = Vec::new();
    for (_, f) in fde_table(img) {
        let cptr = f + 4;
        let Some(back) = u32at(img, cptr) else { continue };
        let cie = cptr - back as i64;
        let enc = *cies.entry(cie).or_insert_with(|| cie_encoding(img, cie, ehs));
        let Ok((st, s)) = read(img, f + 8, enc, ehs) else { continue };
        let Ok((ln, _)) = read(img, f + 8 + s, enc & 0xF, ehs) else { continue };
        out.push((st, st + ln));
    }
    out.sort_unstable();
    out
}

fn cie_encoding(img: &[u8], cie: i64, ehs: i64) -> u8 {
    let mut p = cie + 8;
    let ver = img.get(p as usize).copied().unwrap_or(1);
    p += 1;
    let aug_start = p as usize;
    let Some(len) = img.get(aug_start..).and_then(|s| s.iter().position(|&b| b == 0)) else { return 0 };
    let aug = &img[aug_start..aug_start + len];
    p += len as i64 + 1;
    p = uleb(img, p).1; // code alignment
    while img.get(p as usize).copied().unwrap_or(0) & 0x80 != 0 {
        p += 1; // data alignment (sleb)
    }
    p += 1;
    p = if ver == 1 { p + 1 } else { uleb(img, p).1 }; // return address register
    let mut enc = 0;
    if aug.first() == Some(&b'z') {
        p = uleb(img, p).1;
        for &ch in &aug[1..] {
            match ch {
                b'R' => {
                    enc = img.get(p as usize).copied().unwrap_or(0);
                    p += 1;
                }
                b'P' => {
                    let pe = img.get(p as usize).copied().unwrap_or(0);
                    p += 1;
                    p += read(img, p, pe, ehs).map(|x| x.1).unwrap_or(0);
                }
                b'L' => p += 1,
                _ => {}
            }
        }
    }
    enc
}

/// Sorted [(start, end)] of the data blocks in text [0, text_end): a stretch of at least DATA_MIN bytes between FDE ranges in which more than a quarter of the words do not decode
pub fn data_blocks(img: &[u8], text_end: i64) -> Vec<(i64, i64)> {
    let mut gaps = Vec::new();
    let mut last = 0;
    for (s, e) in fde_ranges(img) {
        if s >= text_end {
            break;
        }
        if s > last {
            gaps.push((last, s));
        }
        last = last.max(e);
    }
    if last < text_end {
        gaps.push((last, text_end));
    }
    let mut out = Vec::new();
    for (s, e) in gaps {
        let (s, e) = ((s + 3) & !3, e & !3);
        if e - s < DATA_MIN {
            continue;
        }
        let bad = (s..e).step_by(4).filter(|&a| !I::decodes(u32::from_le_bytes(img[a as usize..a as usize + 4].try_into().unwrap()))).count() as i64;
        if bad * 4 > (e - s) / 4 {
            out.push((s, e));
        }
    }
    out
}
