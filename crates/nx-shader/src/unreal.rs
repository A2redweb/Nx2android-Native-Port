//! Unreal Engine games: pak files (version 11, UE 4.26 and later), IoStore containers (.utoc / .ucas, UE 5) and the shader code archives in them.
//! Octopath Traveler 0 (UE5): one pak with an AES-256 encrypted index (the key is in `main`) and Oodle-compressed shaders in stored archives.
//! Dragon Quest VII Reimagined (UE 4.27): stored archives, LZ4 shaders. Final Fantasy Resonance (UE 5.6): the shader libraries are IoStore chunks, every group Oodle-compressed.
//! Hogwarts Legacy and other UE4 games use the same pak and archive layouts. Oodle is decoded by the game's own decoder (`pack::Decoder`); LZ4 is decoded here.
//! The layouts follow the notes in upstream's `uepak.py`; nothing here prints or stores the AES key.

use crate::container::{u32le, u64le};
use crate::pack::{Decoder, HostSpec, Outcome};
use crate::scan::Appender;
use crate::split::{scan_blob, Found};
use nx_crypto::Aes256;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

pub const PAK_MAGIC: u32 = 0x5A6F_12E1;
pub const ARCHIVE_VERSION: u32 = 2;
const TOC_MAGIC: &[u8; 16] = b"-==--==--==--==-";
const IO_SHADER_LIBRARY: u8 = 8;

// ---------------------------------------------------------------- reading helpers
struct R<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> R<'a> {
    fn new(b: &'a [u8]) -> Self {
        R { b, o: 0 }
    }
    fn raw(&mut self, n: usize) -> Result<&'a [u8], String> {
        let e = self.o.checked_add(n).ok_or("length overflow")?;
        let v = self.b.get(self.o..e).ok_or("read past the end")?;
        self.o = e;
        Ok(v)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.raw(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.raw(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.raw(8)?.try_into().unwrap()))
    }
    /// FString: i32 length with the NUL, UTF-8; a negative length is UTF-16
    fn fstr(&mut self) -> Result<String, String> {
        let n = self.i32()?;
        if n < 0 {
            let w = self.raw((n.unsigned_abs() as usize).checked_mul(2).ok_or("string too long")?)?;
            let u: Vec<u16> = w.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            return Ok(String::from_utf16_lossy(&u).trim_end_matches('\0').to_string());
        }
        if n == 0 {
            return Ok(String::new());
        }
        let s = self.raw(n as usize)?;
        Ok(String::from_utf8_lossy(&s[..s.len() - 1]).into_owned())
    }
}

// ---------------------------------------------------------------- the AES key
/// 32-byte keys that main's code stores from two 16-byte literals (`stp qA, qB, [xN]` after `ldr qA` / `ldr qB` from adrp pages): clang may split the key array in two constants
fn code_keys(img: &[u8], lo: usize) -> Vec<[u8; 32]> {
    let n = lo.min(img.len()) / 4;
    let w: Vec<u32> = img[..n * 4].chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    let mut out = vec![];
    for (i, &x) in w.iter().enumerate() {
        if x & 0xFFFF_83E0 != 0xAD00_0000 {
            continue;
        }
        let mut page: HashMap<u32, i64> = HashMap::new();
        let mut half: HashMap<u32, i64> = HashMap::new();
        for j in i.saturating_sub(8)..i {
            let y = w[j];
            if y & 0x9F00_0000 == 0x9000_0000 {
                let imm = ((y >> 29) & 3) | (((y >> 5) & 0x7FFFF) << 2);
                let imm = if imm & (1 << 20) != 0 { imm as i64 - (1 << 21) } else { imm as i64 };
                page.insert(y & 31, ((j * 4) as i64 & !0xFFF) + (imm << 12));
            } else if y & 0xFFC0_0000 == 0x3DC0_0000 && page.contains_key(&((y >> 5) & 31)) {
                half.insert(y & 31, page[&((y >> 5) & 31)] + (((y >> 10) & 0xFFF) as i64) * 16);
            } else {
                page.remove(&(y & 31));
                half.remove(&(y & 31));
            }
        }
        if let (Some(&a), Some(&b)) = (half.get(&(x & 31)), half.get(&((x >> 10) & 31))) {
            if a >= 0 && b >= 0 && (a as usize) + 16 <= img.len() && (b as usize) + 16 <= img.len() {
                let mut k = [0u8; 32];
                k[..16].copy_from_slice(&img[a as usize..a as usize + 16]);
                k[16..].copy_from_slice(&img[b as usize..b as usize + 16]);
                out.push(k);
            }
        }
    }
    out
}

/// The AES key that decrypts `block` (the first 16 bytes of the pak's index) to a short mount point ("../.."): a key from main's code, else one of the 32-byte windows of its
/// read-only and data segments (4-aligned, at least 20 distinct bytes). `img` is main's memory image, `lo` where its read-only segment starts.
pub fn find_key(img: &[u8], lo: usize, block: &[u8; 16]) -> Option<Aes256> {
    let opens = |a: &Aes256| {
        let mut d = *block;
        a.decrypt_block(&mut d);
        let n = i32::from_le_bytes(d[..4].try_into().unwrap());
        n > 0 && n < 512 && &d[4..7] == b"../"
    };
    for k in code_keys(img, lo) {
        let a = Aes256::new(&k);
        if opens(&a) {
            return Some(a);
        }
    }
    let mut a = lo;
    while a + 32 <= img.len() {
        let w = &img[a..a + 32];
        let mut seen = [false; 256];
        let mut distinct = 0;
        for &b in w {
            if !seen[b as usize] {
                seen[b as usize] = true;
                distinct += 1;
            }
        }
        if distinct >= 20 {
            let k = Aes256::new(w.try_into().unwrap());
            if opens(&k) {
                return Some(k);
            }
        }
        a += 4;
    }
    None
}

// ---------------------------------------------------------------- pak (version 11)
#[derive(Debug, Clone)]
pub struct PakEntry {
    pub off: u64,
    pub raw: u64,
    pub size: u64,
    pub method: String,
    pub encrypted: bool,
    pub block_size: u64,
    /// (start from the entry, stored length)
    pub blocks: Vec<(u64, u64)>,
}

pub struct Pak {
    f: std::fs::File,
    key: Option<Aes256>,
    pub methods: Vec<String>,
    pub version: u32,
    enc: Vec<u8>,
    pub files: HashMap<String, usize>,
}

/// (distance of the footer's magic from the end, version)
fn pak_magic(f: &mut std::fs::File) -> Result<(u64, u32), String> {
    for back in [0xc8u64, 0xcc] {
        if f.seek(SeekFrom::End(-(back as i64))).is_err() {
            continue;
        }
        let mut b = [0u8; 8];
        if f.read_exact(&mut b).is_ok() && u32::from_le_bytes(b[..4].try_into().unwrap()) == PAK_MAGIC {
            return Ok((back, u32::from_le_bytes(b[4..].try_into().unwrap())));
        }
    }
    Err("no pak footer".into())
}

/// (index encrypted, index offset, index size, compression method names, version) of a version 11 pak
fn pak_footer(f: &mut std::fs::File) -> Result<(bool, u64, u64, Vec<String>, u32), String> {
    let (back, ver) = pak_magic(f)?;
    if ver != 11 {
        return Err(format!("pak version {ver}; only 11 is read"));
    }
    f.seek(SeekFrom::End(-(back as i64 + 21))).map_err(|e| e.to_string())?;
    let mut ft = vec![0u8; back as usize + 21];
    f.read_exact(&mut ft).map_err(|e| e.to_string())?;
    let k = 21;
    let ioff = u64le(&ft, k + 8)?;
    let isz = u64le(&ft, k + 16)?;
    let methods = (0..5)
        .map(|i| {
            let s = ft.get(k + 44 + 32 * i..k + 76 + 32 * i).unwrap_or(&[]);
            String::from_utf8_lossy(s).trim_end_matches('\0').to_string()
        })
        .collect();
    Ok((ft[k - 1] != 0, ioff, isz, methods, ver))
}

/// The first 16 bytes of an encrypted pak index, for `find_key`
pub fn pak_index_block(path: &Path) -> Result<Option<[u8; 16]>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (enc, ioff, ..) = pak_footer(&mut f)?;
    if !enc {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(ioff)).map_err(|e| e.to_string())?;
    let mut b = [0u8; 16];
    f.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(Some(b))
}

fn read_at(f: &mut std::fs::File, key: Option<&Aes256>, off: u64, n: usize, enc: bool) -> Result<Vec<u8>, String> {
    f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    let mut b = vec![0u8; if enc { (n + 15) & !15 } else { n }];
    f.read_exact(&mut b).map_err(|e| e.to_string())?;
    if enc {
        key.ok_or("the data is encrypted and no key was given")?.ecb_decrypt(&mut b);
        b.truncate(n);
    }
    Ok(b)
}

impl Pak {
    pub fn open(path: &Path, key: Option<Aes256>) -> Result<Pak, String> {
        let lab = |e: String| format!("{}: {e}", path.display());
        let mut f = std::fs::File::open(path).map_err(|e| lab(e.to_string()))?;
        let (enc, ioff, isz, methods, version) = pak_footer(&mut f).map_err(lab)?;
        if enc && key.is_none() {
            return Err(lab("the index is encrypted and no key was given".into()));
        }
        let idx = read_at(&mut f, key.as_ref(), ioff, isz as usize, enc).map_err(lab)?;
        let mut r = R::new(&idx);
        let parse = |r: &mut R, f: &mut std::fs::File| -> Result<(Vec<u8>, HashMap<String, usize>), String> {
            r.fstr()?; // mount point
            r.i32()?; // entry count
            r.u64()?; // path hash seed
            if r.i32()? != 0 {
                r.u64()?;
                r.u64()?;
                r.raw(20)?;
            }
            if r.i32()? == 0 {
                return Err("no full directory index".into());
            }
            let (doff, dsz) = (r.u64()?, r.u64()?);
            r.raw(20)?;
            let nenc = r.i32()?;
            let encoded = r.raw(usize::try_from(nenc).map_err(|_| "bad entry table")?)?.to_vec();
            if r.i32()? != 0 {
                return Err("unencoded entries are not read".into());
            }
            let dir = read_at(f, key.as_ref(), doff, dsz as usize, enc)?;
            let mut d = R::new(&dir);
            let mut files = HashMap::new();
            for _ in 0..d.i32()? {
                let dn = d.fstr()?;
                for _ in 0..d.i32()? {
                    let fnm = d.fstr()?;
                    let at = d.i32()?;
                    files.insert(format!("{dn}{fnm}").trim_start_matches('/').to_string(), at as usize);
                }
            }
            Ok((encoded, files))
        };
        let (encoded, files) = parse(&mut r, &mut f).map_err(lab)?;
        Ok(Pak { f, key, methods, version, enc: encoded, files })
    }

    pub fn entry(&self, path: &str) -> Result<PakEntry, String> {
        let at = *self.files.get(path).ok_or("no such file in the pak")?;
        let mut r = R { b: &self.enc, o: at };
        let v = r.u32()?;
        let mut bsz = ((v & 0x3f) as u64) << 11;
        if v & 0x3f == 0x3f {
            bsz = r.u32()? as u64;
        }
        let nblk = ((v >> 6) & 0xffff) as usize;
        let enc = (v >> 22) & 1 != 0;
        let meth = ((v >> 23) & 0x3f) as usize;
        let rd = |r: &mut R, wide: bool| -> Result<u64, String> { if wide { r.u32().map(u64::from) } else { r.u64() } };
        let off = rd(&mut r, (v >> 31) & 1 != 0)?;
        let raw = rd(&mut r, (v >> 30) & 1 != 0)?;
        let size = if meth != 0 { rd(&mut r, (v >> 29) & 1 != 0)? } else { raw };
        let mut cur = 53 + if meth != 0 { 4 + 16 * nblk as u64 } else { 0 };
        if nblk == 1 && bsz == 0 {
            bsz = raw;
        }
        let mut blocks = vec![];
        if nblk == 1 && !enc {
            blocks.push((cur, size));
        } else {
            for _ in 0..nblk {
                let s = r.u32()? as u64;
                blocks.push((cur, s));
                cur += if enc { (s + 15) & !15 } else { s };
            }
        }
        let method = if meth == 0 { String::new() } else { self.methods.get(meth - 1).cloned().unwrap_or_default() };
        Ok(PakEntry { off, raw, size, method, encrypted: enc, block_size: bsz, blocks })
    }

    /// A file's bytes; an Oodle-compressed one goes through `decode(block, raw length)`
    pub fn read(&mut self, path: &str, decode: &mut dyn FnMut(&[u8], usize) -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
        let e = self.entry(path)?;
        if e.method.is_empty() {
            return read_at(&mut self.f, self.key.as_ref(), e.off + 53, e.size as usize, e.encrypted);
        }
        if !e.method.eq_ignore_ascii_case("oodle") {
            return Err(format!("{path} is {}-compressed in the pak; only Oodle is read", e.method));
        }
        let mut out = vec![];
        let mut left = e.raw as usize;
        for (s, n) in &e.blocks {
            let k = left.min(e.block_size as usize);
            let b = read_at(&mut self.f, self.key.as_ref(), e.off + s, *n as usize, e.encrypted)?;
            out.extend(decode(&b, k)?);
            left -= k;
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------- shader code archive (version 2)
pub struct Archive {
    /// (offset, stored size, raw size, frequency)
    pub shaders: Vec<(u64, u32, u32, u8)>,
    pub code: usize,
}

pub fn archive(b: &[u8]) -> Result<Archive, String> {
    let mut r = R::new(b);
    if r.u32()? != ARCHIVE_VERSION {
        return Err("not a version 2 shader code archive".into());
    }
    let n = r.u32()? as usize;
    r.raw(n.checked_mul(20).ok_or("overflow")?)?;
    let n = r.u32()? as usize;
    r.raw(n.checked_mul(20).ok_or("overflow")?)?;
    let n = r.u32()? as usize;
    r.raw(n.checked_mul(16).ok_or("overflow")?)?;
    let n = r.u32()? as usize;
    let mut shaders = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let (o, s, raw, fr) = (r.u64()?, r.u32()?, r.u32()?, r.u8()?);
        shaders.push((o, s, raw, fr));
    }
    let n = r.u32()? as usize;
    r.raw(n.checked_mul(16).ok_or("overflow")?)?;
    let n = r.u32()? as usize;
    r.raw(n.checked_mul(4).ok_or("overflow")?)?;
    Ok(Archive { shaders, code: r.o })
}

/// One shader of an archive as an LSLG record: stored as is, an LZ4 block (UE 4.26 and later), else the game's Oodle
fn shader_bytes(b: &[u8], a: &Archive, i: usize, dec: &mut LazyDec) -> Result<Vec<u8>, String> {
    let (off, size, raw, _) = a.shaders[i];
    let at = a.code.checked_add(off as usize).ok_or("overflow")?;
    let c = b.get(at..at + size as usize).ok_or("shader runs past the archive")?;
    if size == raw {
        return Ok(c.to_vec());
    }
    if let Ok(d) = nx_nso::lz4_block(c, raw as usize) {
        if d.len() == raw as usize && d.get(..4) == Some(b"LSLG") {
            return Ok(d);
        }
    }
    dec.decode(c, raw as usize)
}

// ---------------------------------------------------------------- IoStore
pub struct Toc {
    path: PathBuf,
    key: Option<Aes256>,
    pub version: u8,
    encrypted: bool,
    block_size: u64,
    part_size: u64,
    pub ids: Vec<[u8; 12]>,
    ranges: Vec<(u64, u64)>,
    /// (offset in the ucas, stored size, raw size, method index)
    blocks: Vec<(u64, u32, u32, u8)>,
    methods: Vec<String>,
    index: HashMap<[u8; 12], usize>,
}

fn be40(b: &[u8]) -> u64 {
    b[..5].iter().fold(0u64, |a, &x| (a << 8) | x as u64)
}
fn le(b: &[u8]) -> u64 {
    b.iter().rev().fold(0u64, |a, &x| (a << 8) | x as u64)
}

impl Toc {
    pub fn open(path: &Path, key: Option<Aes256>) -> Result<Toc, String> {
        let lab = |e: String| format!("{}: {e}", path.display());
        let b = std::fs::read(path).map_err(|e| lab(e.to_string()))?;
        if b.get(..16) != Some(&TOC_MAGIC[..]) {
            return Err(lab("not an IoStore TOC".into()));
        }
        let version = b[16];
        let h = |i: usize| u32le(&b, 20 + 4 * i);
        let (hsz, n, nblk, bsz, nmeth, mlen, block_size, nparts) = (h(0).map_err(lab)? as usize, h(1).map_err(lab)? as usize, h(2).map_err(lab)? as usize, h(3).map_err(lab)?, h(4).map_err(lab)? as usize, h(5).map_err(lab)? as usize, h(6).map_err(lab)? as u64, h(8).map_err(lab)?);
        if bsz != 12 {
            return Err(lab(format!("compression block entries of {bsz} bytes")));
        }
        let encrypted = b.get(80).copied().unwrap_or(0) & 2 != 0;
        let nseeds = if version >= 4 { u32le(&b, 84).map_err(lab)? as usize } else { 0 };
        let part_size = if version >= 3 && nparts > 1 { u64le(&b, 88).map_err(lab)? } else { u64::MAX };
        let nover = if version >= 5 { u32le(&b, 96).map_err(lab)? as usize } else { 0 };
        if encrypted && key.is_none() {
            return Err(lab("the container is encrypted and no key was given".into()));
        }
        let need = hsz + 12 * n + 10 * n + 4 * nseeds + 4 * nover + 12 * nblk + mlen * nmeth;
        if n > 1 << 24 || nblk > 1 << 26 || b.len() < need {
            return Err(lab("the table of contents is cut short".into()));
        }
        let mut o = hsz;
        let ids: Vec<[u8; 12]> = (0..n).map(|i| b[o + 12 * i..o + 12 * i + 12].try_into().unwrap()).collect();
        o += 12 * n;
        let ranges = (0..n).map(|i| (be40(&b[o + 10 * i..]), be40(&b[o + 10 * i + 5..]))).collect();
        o += 10 * n + 4 * nseeds + 4 * nover;
        let blocks = (0..nblk)
            .map(|i| {
                let e = &b[o + 12 * i..o + 12 * i + 12];
                (le(&e[..5]), le(&e[5..8]) as u32, le(&e[8..11]) as u32, e[11])
            })
            .collect();
        o += 12 * nblk;
        let methods = (0..nmeth).map(|i| String::from_utf8_lossy(&b[o + mlen * i..o + mlen * (i + 1)]).trim_end_matches('\0').to_string()).collect();
        let index = ids.iter().enumerate().map(|(i, c)| (*c, i)).collect();
        Ok(Toc { path: path.to_path_buf(), key, version, encrypted, block_size, part_size, ids, ranges, blocks, methods, index })
    }

    /// Indices of the chunks of one EIoChunkType (UE5 numbering, TOC version 4 and later)
    pub fn of_type(&self, t: u8) -> Vec<usize> {
        if self.version < 4 {
            return vec![];
        }
        (0..self.ids.len()).filter(|&i| self.ids[i][11] == t).collect()
    }

    fn ucas(&self, part: u64) -> Result<std::fs::File, String> {
        let stem = self.path.with_extension("");
        let p = if part == 0 { stem.with_extension("ucas") } else { PathBuf::from(format!("{}_s{part}.ucas", stem.display())) };
        std::fs::File::open(&p).map_err(|e| format!("{}: {e}", p.display()))
    }

    /// Chunk `i`'s bytes; Oodle blocks go through `decode(block, raw length)`
    pub fn read(&self, i: usize, decode: &mut dyn FnMut(&[u8], usize) -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
        let (off, ln) = self.ranges[i];
        if ln == 0 {
            return Ok(vec![]);
        }
        let first = off / self.block_size;
        let last = (off + ln - 1) / self.block_size;
        let mut out = vec![];
        let mut open: HashMap<u64, std::fs::File> = HashMap::new();
        for k in first..=last {
            let &(at, size, raw, m) = self.blocks.get(k as usize).ok_or("block past the table")?;
            let part = at / self.part_size;
            if !open.contains_key(&part) {
                open.insert(part, self.ucas(part)?);
            }
            let f = open.get_mut(&part).unwrap();
            let n = size as usize;
            f.seek(SeekFrom::Start(at % self.part_size)).map_err(|e| e.to_string())?;
            let mut d = vec![0u8; if self.encrypted { (n + 15) & !15 } else { n }];
            f.read_exact(&mut d).map_err(|e| e.to_string())?;
            if self.encrypted {
                self.key.as_ref().ok_or("encrypted and no key")?.ecb_decrypt(&mut d);
                d.truncate(n);
            }
            let meth = if m == 0 { "" } else { self.methods.get(m as usize - 1).map(String::as_str).unwrap_or("?") };
            match meth {
                "" => out.extend_from_slice(&d[..(raw as usize).min(d.len())]),
                x if x.eq_ignore_ascii_case("oodle") => out.extend(decode(&d, raw as usize)?),
                x if x.eq_ignore_ascii_case("lz4") => out.extend(nx_nso::lz4_block(&d, raw as usize).map_err(|e| e.to_string())?),
                x => return Err(format!("compression method {x} is not read")),
            }
        }
        let s = (off - first * self.block_size) as usize;
        out.get(s..s + ln as usize).map(<[u8]>::to_vec).ok_or_else(|| "chunk shorter than its range".to_string())
    }
}

/// An IoStore shader library (UE5 FIoStoreShaderCodeArchive): the groups and the shaders' places in them
pub struct Library {
    ents: Vec<u64>,
    /// (first index, count, raw size, stored size)
    groups: Vec<(u32, u32, u32, u32)>,
    gids: Vec<[u8; 12]>,
    idx: Vec<u32>,
}

pub fn library(b: &[u8]) -> Result<Library, String> {
    let mut r = R::new(b);
    if r.u32()? != 1 {
        return Err("shader library chunk is not version 1".into());
    }
    let n = r.u32()? as usize;
    r.raw(n.checked_mul(20).ok_or("overflow")?)?;
    let nsh = r.u32()? as usize;
    r.raw(nsh.checked_mul(20).ok_or("overflow")?)?;
    let ng = r.u32()? as usize;
    let gids = (0..ng).map(|_| r.raw(12).map(|x| <[u8; 12]>::try_from(x).unwrap())).collect::<Result<Vec<_>, _>>()?;
    let nm = r.u32()? as usize;
    r.raw(nm.checked_mul(8).ok_or("overflow")?)?;
    let n = r.u32()? as usize;
    if n != nsh {
        return Err("shader library tables disagree".into());
    }
    let ents = (0..n).map(|_| r.u64()).collect::<Result<Vec<_>, _>>()?;
    let ngr = r.u32()? as usize;
    if ngr != ng {
        return Err("shader library groups disagree".into());
    }
    let groups = (0..ngr).map(|_| Ok((r.u32()?, r.u32()?, r.u32()?, r.u32()?))).collect::<Result<Vec<_>, String>>()?;
    let k = r.u32()? as usize;
    let idx = (0..k).map(|_| r.u32()).collect::<Result<Vec<_>, _>>()?;
    Ok(Library { ents, groups, gids, idx })
}

/// The decoded shader records of group `g`
fn group_shaders(toc: &Toc, lib: &Library, g: usize, dec: &mut LazyDec) -> Result<Vec<Vec<u8>>, String> {
    let (first, cnt, raw, stored) = lib.groups[g];
    let ci = *toc.index.get(&lib.gids[g]).ok_or_else(|| format!("shader group {g} has no chunk"))?;
    let mut d = toc.read(ci, &mut |b, n| dec.decode(b, n))?;
    if d.len() != stored as usize {
        return Err(format!("shader group {g} is {} bytes, its entry says {stored}", d.len()));
    }
    if stored < raw {
        d = dec.decode(&d, raw as usize)?;
    }
    let members = lib.idx.get(first as usize..(first + cnt) as usize).ok_or("group indices past the table")?;
    let mut mine: Vec<(u64, usize)> = members.iter().map(|&s| (lib.ents[s as usize] >> 34, s as usize)).collect();
    mine.sort();
    let mut out = vec![];
    for (k, &(at, s)) in mine.iter().enumerate() {
        if ((lib.ents[s] >> 4) & 0x3FFF_FFFF) as usize != g {
            return Err(format!("shader {s} is listed in group {g} but names another"));
        }
        let nxt = mine.get(k + 1).map(|x| x.0).unwrap_or(raw as u64);
        out.push(d.get(at as usize..nxt as usize).ok_or("shader past its group")?.to_vec());
    }
    Ok(out)
}

// ---------------------------------------------------------------- decoding and scanning
/// The game's Oodle, started on first use
pub struct LazyDec<'a> {
    host: &'a HostSpec,
    tag: String,
    d: Option<Decoder>,
}

impl LazyDec<'_> {
    pub fn decode(&mut self, comp: &[u8], raw: usize) -> Result<Vec<u8>, String> {
        if self.d.is_none() {
            self.d = Some(Decoder::start(&self.host.exe, &self.host.mods, &self.host.cwd, &self.host.game, &self.host.log_dir.join(format!("decoder_{}.log", self.tag)))?);
        }
        self.d.as_mut().unwrap().decode(comp, raw)
    }
}

type Work<'a> = &'a (dyn Fn(usize, &mut LazyDec) -> Result<Vec<Found>, String> + Sync);

/// Runs `work` over 0..total on `threads` decoder processes; what it finds goes to `add`. An error starting with NUL is one item that did not decode (counted, the rest go on).
fn par_scan(total: usize, threads: usize, host: &HostSpec, label: &str, work: Work, add: &mut Appender, progress: &(dyn Fn(usize, usize) + Sync), out: &mut Outcome) -> Result<(), String> {
    if total == 0 {
        return Ok(());
    }
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<Result<Vec<Found>, String>>();
    let mut result = Ok(());
    std::thread::scope(|sc| {
        for t in 0..threads.clamp(1, 4).min(total) {
            let (tx, next) = (tx.clone(), &next);
            sc.spawn(move || {
                let mut dec = LazyDec { host, tag: format!("{label}{t}"), d: None };
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= total {
                        break;
                    }
                    let r = work(i, &mut dec);
                    let dead = matches!(&r, Err(e) if !e.starts_with('\0'));
                    if tx.send(r).is_err() || dead {
                        return;
                    }
                }
            });
        }
        drop(tx);
        let mut seen = 0usize;
        while let Ok(r) = rx.recv() {
            match r {
                Ok(found) => {
                    for f in found {
                        if let Err(e) = add.add(f) {
                            result = Err(e);
                            next.store(usize::MAX / 2, Ordering::Relaxed);
                        }
                    }
                    seen += 1;
                    if seen % 64 == 0 {
                        progress(seen, total);
                    }
                }
                Err(e) if e.starts_with('\0') => {
                    out.failed += 1;
                    seen += 1;
                    if out.notes.len() < 3 {
                        out.notes.push(e[1..].to_string());
                    }
                }
                Err(e) => {
                    result = Err(e);
                    next.store(usize::MAX / 2, Ordering::Relaxed);
                }
            }
        }
    });
    progress(total, total);
    result
}

fn scan_record(d: &[u8], label: String) -> Vec<Found> {
    let mut found = vec![];
    if scan_blob(d, &label, &mut found).is_err() {
        found.clear();
    }
    found
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut kids: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
    kids.sort();
    for p in kids {
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// The version 11 paks and the IoStore containers with shader libraries under `romfs`
pub fn unreal_files(romfs: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut all = vec![];
    walk(romfs, &mut all);
    let (mut paks, mut tocs) = (vec![], vec![]);
    for p in all {
        let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if n.ends_with(".pak") {
            if let Ok(mut f) = std::fs::File::open(&p) {
                if matches!(pak_footer(&mut f), Ok((.., 11))) {
                    paks.push(p);
                }
            }
        } else if n.ends_with(".utoc") {
            if let Ok(t) = Toc::open(&p, None).or_else(|_| Toc::open(&p, Some(Aes256::new(&[0; 32])))) {
                if !t.of_type(IO_SHADER_LIBRARY).is_empty() {
                    tocs.push(p);
                }
            }
        }
    }
    (paks, tocs)
}

/// Reads every shader archive in the paks and IoStore libraries; what is found goes to `add`. `main` is the game's main NSO file (the AES key is looked for in it).
pub fn scan_unreal(romfs: &Path, main: &Path, host: &HostSpec, threads: usize, add: &mut Appender, progress: &(dyn Fn(&str, usize, usize) + Sync)) -> Result<Outcome, String> {
    let mut out = Outcome { members: 0, failed: 0, notes: vec![] };
    let _ = std::fs::create_dir_all(&host.log_dir);
    let (paks, tocs) = unreal_files(romfs);
    let mut img: Option<(Vec<u8>, usize)> = None;
    let mut key_for = |block: &[u8; 16]| -> Result<Aes256, String> {
        if img.is_none() {
            let d = std::fs::read(main).map_err(|e| format!("{}: {e}", main.display()))?;
            let nso = nx_nso::Nso::parse(&d).map_err(|e| e.to_string())?;
            img = Some((nso.image(&d).map_err(|e| e.to_string())?, nso.segments[1].mem_offset as usize));
        }
        let (im, lo) = img.as_ref().unwrap();
        find_key(im, *lo, block).ok_or_else(|| "the pak index is encrypted and no key in main opens it".to_string())
    };
    let mut cached: Option<Aes256> = None;
    for tp in &tocs {
        let t = Toc::open(tp, None).or_else(|_| Toc::open(tp, Some(Aes256::new(&[0; 32]))))?;
        let rel = tp.strip_prefix(romfs).unwrap_or(tp).to_string_lossy().replace('\\', "/");
        for ci in t.of_type(IO_SHADER_LIBRARY) {
            let lib_bytes = {
                let mut d = LazyDec { host, tag: "lib".into(), d: None };
                t.read(ci, &mut |b, n| d.decode(b, n))?
            };
            let lib = library(&lib_bytes).map_err(|e| format!("{rel}: {e}"))?;
            let hex: String = t.ids[ci][..8].iter().map(|b| format!("{b:02x}")).collect();
            let label = format!("/{rel}#lib{hex}");
            let work = |g: usize, dec: &mut LazyDec| -> Result<Vec<Found>, String> {
                let recs = group_shaders(&t, &lib, g, dec).map_err(|e| if e == "the decoder ended" { e } else { format!("\0{label} group {g}: {e}") })?;
                Ok(recs.into_iter().enumerate().flat_map(|(k, r)| scan_record(&r, format!("{label}/g{g}/{k}"))).collect())
            };
            par_scan(lib.groups.len(), threads, host, "io", &work, add, &|d, n| progress("reading the game's shader libraries", d, n), &mut out)?;
            out.members += lib.ents.len();
        }
    }
    for pp in &paks {
        let enc_block = pak_index_block(pp)?;
        if let Some(b) = enc_block {
            if cached.is_none() {
                cached = Some(key_for(&b).map_err(|e| format!("{}: {e}", pp.display()))?);
            }
        }
        let mut pak = Pak::open(pp, if enc_block.is_some() { cached.clone() } else { None })?;
        let mut names: Vec<String> = pak.files.keys().filter(|n| n.ends_with(".ushaderbytecode")).cloned().collect();
        names.sort();
        let rel = pp.strip_prefix(romfs).unwrap_or(pp).to_string_lossy().replace('\\', "/");
        for name in names {
            let mut dec = LazyDec { host, tag: "pak".into(), d: None };
            let bytes = pak.read(&name, &mut |b, n| dec.decode(b, n)).map_err(|e| format!("{rel}: {name}: {e}"))?;
            drop(dec);
            let a = archive(&bytes).map_err(|e| format!("{rel}: {name}: {e}"))?;
            let label = format!("/{rel}#{name}");
            let work = |i: usize, dec: &mut LazyDec| -> Result<Vec<Found>, String> {
                let r = shader_bytes(&bytes, &a, i, dec).map_err(|e| if e == "the decoder ended" { e } else { format!("\0{label} shader {i}: {e}") })?;
                Ok(scan_record(&r, format!("{label}/{i}")))
            };
            par_scan(a.shaders.len(), threads, host, "pak", &work, add, &|d, n| progress("reading the game's shader archives", d, n), &mut out)?;
            out.members += a.shaders.len();
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fstring_both_encodings() {
        let mut b = vec![];
        b.extend(4i32.to_le_bytes());
        b.extend(b"abc\0");
        b.extend((-3i32).to_le_bytes());
        for c in "hi\0".encode_utf16() {
            b.extend(c.to_le_bytes());
        }
        let mut r = R::new(&b);
        assert_eq!(r.fstr().unwrap(), "abc");
        assert_eq!(r.fstr().unwrap(), "hi");
    }

    #[test]
    fn archive_tables_are_read() {
        let mut b = vec![];
        b.extend(2u32.to_le_bytes());
        b.extend(0u32.to_le_bytes()); // shader maps
        b.extend(1u32.to_le_bytes()); // shader hashes
        b.extend([7u8; 20]);
        b.extend(0u32.to_le_bytes()); // map entries
        b.extend(1u32.to_le_bytes()); // shader entries
        b.extend(0u64.to_le_bytes());
        b.extend(5u32.to_le_bytes());
        b.extend(9u32.to_le_bytes());
        b.push(3);
        b.extend(0u32.to_le_bytes()); // preload
        b.extend(0u32.to_le_bytes()); // indices
        let a = archive(&b).unwrap();
        assert_eq!(a.shaders, vec![(0, 5, 9, 3)]);
        assert_eq!(a.code, b.len());
    }

    #[test]
    fn the_key_is_found_in_a_window() {
        let key: Vec<u8> = (0u8..32).map(|i| i.wrapping_mul(7).wrapping_add(3)).collect();
        let aes = Aes256::new(key.as_slice().try_into().unwrap());
        // an index start: i32 length 5, "../../" ... encrypted
        let mut blk = [0u8; 16];
        blk[..4].copy_from_slice(&10i32.to_le_bytes());
        blk[4..10].copy_from_slice(b"../../");
        let mut enc = blk;
        aes.encrypt_block(&mut enc);
        let mut img = vec![0x55u8; 64];
        img.extend(&key);
        img.extend([0x55u8; 40]);
        assert!(find_key(&img, 0, &enc).is_some());
        assert!(find_key(&vec![0x55u8; 200], 0, &enc).is_none());
    }

    #[test]
    fn library_groups_are_cut_by_offsets() {
        let mut b = vec![];
        b.extend(1u32.to_le_bytes());
        b.extend(0u32.to_le_bytes()); // maps
        b.extend(2u32.to_le_bytes());
        b.extend([0u8; 40]); // shader hashes
        b.extend(1u32.to_le_bytes());
        b.extend([9u8; 12]); // group id
        b.extend(0u32.to_le_bytes()); // map entries
        b.extend(2u32.to_le_bytes());
        b.extend((3u64 | (0u64 << 4) | (0u64 << 34)).to_le_bytes());
        b.extend((3u64 | (0u64 << 4) | (6u64 << 34)).to_le_bytes());
        b.extend(1u32.to_le_bytes());
        for v in [0u32, 2, 10, 10] {
            b.extend(v.to_le_bytes());
        }
        b.extend(2u32.to_le_bytes());
        b.extend(0u32.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        let l = library(&b).unwrap();
        assert_eq!(l.groups, vec![(0, 2, 10, 10)]);
        assert_eq!(l.ents[1] >> 34, 6);
    }
}
