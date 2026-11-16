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
    unzs_with(d, &[])
}

pub type Dicts = std::sync::Arc<Vec<std::sync::Arc<nx_zstd::Dictionary>>>;

/// A zstd file decompressed: a frame that names a dictionary takes it from `dicts` (Tears of the Kingdom's shader packages name ID 1), and is an error when it is not there.
pub fn unzs_with(d: &[u8], dicts: &[std::sync::Arc<nx_zstd::Dictionary>]) -> Result<Vec<u8>, String> {
    nx_zstd::decompress_vec_with(d, dicts).map_err(|e| e.to_string())
}

/// The zstd dictionaries a RomFS ships: `Pack/ZsDic.pack.zs`, a SARC (itself plain zstd) of .zsdic files. Empty when there is no such file
pub fn zs_dicts(romfs: &std::path::Path) -> Result<Vec<std::sync::Arc<nx_zstd::Dictionary>>, String> {
    let p = romfs.join("Pack/ZsDic.pack.zs");
    if !p.is_file() {
        return Ok(vec![]);
    }
    let d = unzs(&std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?)?;
    let mut out = vec![];
    for (_, m) in sarc_members(&d)? {
        if m.get(..4) == Some(&[0x37, 0xA4, 0x30, 0xEC][..]) {
            out.push(std::sync::Arc::new(nx_zstd::Dictionary::parse(m).map_err(|e| e.to_string())?));
        }
    }
    Ok(out)
}

/// A file's bytes: read into memory when small, mapped read-only when large (a game's asset archive can be several GB, more than the phone has free; mapped pages are clean and the system takes them back)
pub enum FileBytes {
    Owned(Vec<u8>),
    #[cfg(unix)]
    Mapped(*mut u8, usize),
}

// the mapping is read-only and private to this value
unsafe impl Send for FileBytes {}
unsafe impl Sync for FileBytes {}

#[cfg(unix)]
extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
}

impl std::ops::Deref for FileBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            FileBytes::Owned(v) => v,
            #[cfg(unix)]
            FileBytes::Mapped(p, n) => unsafe { std::slice::from_raw_parts(*p, *n) },
        }
    }
}

#[cfg(unix)]
impl Drop for FileBytes {
    fn drop(&mut self) {
        if let FileBytes::Mapped(p, n) = self {
            unsafe { munmap(*p, *n) };
        }
    }
}

/// files from this size up are mapped, not read
pub const MAP_FROM: u64 = 256 << 20;

pub fn load_file(path: &std::path::Path) -> Result<FileBytes, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let n = f.metadata().map_err(|e| e.to_string())?.len();
    #[cfg(unix)]
    if n >= MAP_FROM {
        use std::os::fd::AsRawFd;
        // PROT_READ = 1, MAP_PRIVATE = 2 (the same on Linux and Android)
        let p = unsafe { mmap(std::ptr::null_mut(), n as usize, 1, 2, f.as_raw_fd(), 0) };
        if p as isize == -1 {
            return Err(format!("{}: cannot map {n} bytes", path.display()));
        }
        return Ok(FileBytes::Mapped(p, n as usize));
    }
    std::fs::read(path).map(FileBytes::Owned).map_err(|e| format!("{}: {e}", path.display()))
}

pub const LZ4_FRAME_MAGIC: [u8; 4] = [0x04, 0x22, 0x4D, 0x18];

/// The decompressed size a zstd or LZ4 frame header gives, or None when it gives none
pub fn frame_size(f: &[u8]) -> Option<u64> {
    if f.len() < 6 {
        return None;
    }
    if f[..4] == LZ4_FRAME_MAGIC {
        return (f[4] & 8 != 0 && f.len() >= 14).then(|| u64::from_le_bytes(f[6..14].try_into().unwrap()));
    }
    let fhd = f[4];
    let single = (fhd >> 5) & 1;
    let width = [single as usize, 2, 4, 8][(fhd >> 6) as usize];
    if width == 0 {
        return None;
    }
    let at = 5 + (single == 0) as usize + [0, 1, 2, 4][(fhd & 3) as usize];
    let b = f.get(at..at + width)?;
    let mut v = [0u8; 8];
    v[..width].copy_from_slice(b);
    Some(u64::from_le_bytes(v) + if width == 2 { 256 } else { 0 })
}

/// One LZ4 block appended to `out`; matches may reach into what `out` already holds (linked blocks)
fn lz4_block_into(src: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
    let (mut i, bad) = (0usize, || "lz4: block is damaged".to_string());
    while i < src.len() {
        let t = src[i];
        i += 1;
        let mut lit = (t >> 4) as usize;
        if lit == 15 {
            loop {
                let b = *src.get(i).ok_or_else(bad)?;
                i += 1;
                lit += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        out.extend(src.get(i..i + lit).ok_or_else(bad)?);
        i += lit;
        if i >= src.len() {
            break;
        }
        let dist = u16::from_le_bytes(src.get(i..i + 2).ok_or_else(bad)?.try_into().unwrap()) as usize;
        i += 2;
        let mut ml = (t & 15) as usize;
        if ml == 15 {
            loop {
                let b = *src.get(i).ok_or_else(bad)?;
                i += 1;
                ml += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        ml += 4;
        if dist == 0 || dist > out.len() {
            return Err(bad());
        }
        for _ in 0..ml {
            out.push(out[out.len() - dist]);
        }
    }
    Ok(())
}

/// An LZ4 frame decompressed
pub fn lz4_frame(d: &[u8]) -> Result<Vec<u8>, String> {
    if d.get(..4) != Some(&LZ4_FRAME_MAGIC[..]) || d.len() < 7 {
        return Err("lz4: not a frame".into());
    }
    let flg = d[4];
    let mut p = 6 + if flg & 8 != 0 { 8 } else { 0 } + if flg & 1 != 0 { 4 } else { 0 } + 1;
    let mut out = vec![];
    loop {
        let n = u32le(d, p)?;
        p += 4;
        if n == 0 {
            break;
        }
        let len = (n & 0x7fff_ffff) as usize;
        let b = d.get(p..p + len).ok_or("lz4: block is cut short")?;
        if n & 0x8000_0000 != 0 {
            out.extend(b);
        } else {
            lz4_block_into(b, &mut out)?;
        }
        p += len + if flg & 0x10 != 0 { 4 } else { 0 };
        if out.len() > 1 << 31 {
            return Err("lz4: implausible size".into());
        }
    }
    Ok(out)
}

/// HAL's .cmp container (Kirby and the Forgotten Land): a u32 decompressed size, then one zstd or LZ4 frame. → the decompressed bytes, or `d` itself when it is not that:
/// a frame header whose size differs from the prefix (Tears of the Kingdom's .quad files), a frame that fails or gives another size than the prefix. The second return says why it was left alone.
pub fn size_prefixed(d: &[u8], dicts: &[std::sync::Arc<nx_zstd::Dictionary>]) -> (Vec<u8>, Option<String>) {
    let (Some(p), Some(frame)) = (d.get(..4), d.get(4..)) else { return (d.to_vec(), None) };
    let size = u32::from_le_bytes(p.try_into().unwrap()) as u64;
    if !matches!(frame_size(frame), None) && frame_size(frame) != Some(size) {
        return (d.to_vec(), None);
    }
    let r = if frame.get(..4) == Some(&LZ4_FRAME_MAGIC[..]) { lz4_frame(frame) } else { unzs_with(frame, dicts) };
    match r {
        Ok(out) if out.len() as u64 == size => (out, None),
        Ok(out) => (d.to_vec(), Some(format!("{} bytes where the prefix gives {size}", out.len()))),
        Err(e) => (d.to_vec(), Some(e)),
    }
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

    #[test]
    fn sized_frames_and_cmp_containers() {
        // an LZ4 frame of one literal-only block, with content size and a second, uncompressed block
        let lit = b"hello hello hello ";
        let mut blk = vec![0xF0, (lit.len() - 15) as u8]; // 15 + extra literals
        blk.extend(lit);
        let mut f = LZ4_FRAME_MAGIC.to_vec();
        f.extend([0x68, 0x40]); // FLG: version 01, content size; BD
        f.extend(((lit.len() + 3) as u64).to_le_bytes());
        f.push(0);
        f.extend((blk.len() as u32).to_le_bytes());
        f.extend(&blk);
        f.extend((3u32 | 0x8000_0000).to_le_bytes());
        f.extend(b"end");
        f.extend(0u32.to_le_bytes());
        assert_eq!(frame_size(&f), Some(21));
        let want = b"hello hello hello end";
        assert_eq!(lz4_frame(&f).unwrap(), want);
        let mut cmp = 21u32.to_le_bytes().to_vec();
        cmp.extend(&f);
        assert_eq!(size_prefixed(&cmp, &[]), (want.to_vec(), None));
        // a prefix that disagrees with the frame's own size: left alone
        let mut other = 99u32.to_le_bytes().to_vec();
        other.extend(&f);
        assert_eq!(size_prefixed(&other, &[]), (other.clone(), None));
    }

    #[test]
    fn a_frame_that_names_a_dictionary_needs_it() {
        let dict = std::sync::Arc::new(nx_zstd::Dictionary::parse(include_bytes!("../../nx-zstd/tests/dict/dict.bin")).unwrap());
        let (z, raw) = (&include_bytes!("../../nx-zstd/tests/dict/dict_1.zst")[..], &include_bytes!("../../nx-zstd/tests/dict/dict_1.raw")[..]);
        assert_eq!(unzs_with(z, &[dict]).unwrap(), raw);
        assert!(unzs(z).unwrap_err().contains("needs dictionary"));
    }

    #[test]
    fn large_files_can_be_mapped() {
        let p = std::env::temp_dir().join(format!("nxmap-{}", std::process::id()));
        std::fs::write(&p, b"BNSH\0\0\0\0 some bytes").unwrap();
        assert!(matches!(load_file(&p).unwrap(), FileBytes::Owned(_)));
        // the mapped variant, forced: map the same file by hand
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let f = std::fs::File::open(&p).unwrap();
            let n = f.metadata().unwrap().len() as usize;
            let m = unsafe { mmap(std::ptr::null_mut(), n, 1, 2, f.as_raw_fd(), 0) };
            assert_ne!(m as isize, -1);
            let fb = FileBytes::Mapped(m, n);
            assert_eq!(&fb[..4], b"BNSH");
            assert_eq!(fb.len(), n);
        }
        let _ = std::fs::remove_file(&p);
    }
}
