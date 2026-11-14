//! NSZ / XCZ to NSP: the Rust counterpart of upstream's `porter/dumps.py`.
//!
//! An NSZ (or XCZ) is an NSP (or XCI) whose NCAs are NCZ files. An NCZ keeps the NCA's first 0x4000 bytes as they are, then a section table (`NCZSECTN`: offset, size, crypto type, key and
//! counter of each section) and the rest of the NCA decrypted and zstd-compressed, either as one stream or as independent blocks (`NCZBLOCK`). Converting back means decompressing and
//! encrypting each section again with AES-128-CTR (counter = the first 8 bytes of the stored counter, then the NCA offset / 16). The result is an NSP of the container's files; every NCA
//! made is checked against its name, the first 16 bytes of its SHA-256 (not for an NSZ with no ticket: converting its NCAs from title-key to standard crypto rewrote their headers).
//! Layouts as nsz 4.x / 5.x write them. Tested here against synthetic NCZ files, including a zstd stream made by libzstd; no real NSZ has been available.

use nx_crypto::{Aes128, Sha256};
use nx_zstd::Decoder;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};

const HEAD: u64 = 0x4000;
const EXPECT_NAME_LEN: usize = 32;

fn bad<T>(m: String) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidData, m))
}

pub fn is_compressed(path: &std::path::Path) -> bool {
    path.extension().map(|x| matches!(x.to_string_lossy().to_ascii_lowercase().as_str(), "nsz" | "xcz")).unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sec {
    offset: u64,
    size: u64,
    crypto: u64,
    key: [u8; 16],
    ctr: [u8; 16],
}

struct Block {
    size: u64,
    sizes: Vec<u32>,
    bs: u64,
}

struct Head {
    secs: Vec<Sec>,
    block: Option<Block>,
}

fn rd<R: Read>(r: &mut R, n: usize) -> io::Result<Vec<u8>> {
    let mut v = vec![0u8; n];
    r.read_exact(&mut v)?;
    Ok(v)
}

fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// The section table and block header of the NCZ at `at`; the reader is left after both.
fn head<R: Read + Seek>(r: &mut R, at: u64, name: &str) -> io::Result<Head> {
    r.seek(SeekFrom::Start(at + HEAD))?;
    let h = rd(r, 16)?;
    if &h[..8] != b"NCZSECTN" {
        return bad(format!("{name}: not an NCZ (no NCZSECTN at {at:#x})"));
    }
    let n = u64le(&h, 8);
    if n > 64 {
        return bad(format!("{name}: an NCZ with {n} sections is not plausible"));
    }
    let mut secs = Vec::new();
    for _ in 0..n {
        let e = rd(r, 0x40)?;
        secs.push(Sec { offset: u64le(&e, 0), size: u64le(&e, 8), crypto: u64le(&e, 16), key: e[32..48].try_into().unwrap(), ctr: e[48..64].try_into().unwrap() });
    }
    if let Some(first) = secs.first() {
        if first.offset > HEAD {
            // older compressors start the first section after the header: the gap is plain
            let gap = Sec { offset: HEAD, size: first.offset - HEAD, crypto: 1, key: [0; 16], ctr: [0; 16] };
            secs.insert(0, gap);
        }
    }
    let pos = r.stream_position()?;
    let mut magic = [0u8; 8];
    let got = read_up_to(r, &mut magic)?;
    let block = if got == 8 && &magic == b"NCZBLOCK" {
        let b = rd(r, 16)?;
        let (exp, nb, size) = (b[3], u32::from_le_bytes(b[4..8].try_into().unwrap()), u64le(&b, 8));
        if !(14..=32).contains(&exp) {
            return bad(format!("{name}: a damaged NCZBLOCK header (block size 2^{exp})"));
        }
        if nb as u64 > (1 << 24) {
            return bad(format!("{name}: an NCZBLOCK header with {nb} blocks is not plausible"));
        }
        let raw = rd(r, 4 * nb as usize)?;
        Some(Block { size, sizes: raw.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect(), bs: 1 << exp })
    } else {
        r.seek(SeekFrom::Start(pos))?;
        None
    };
    Ok(Head { secs, block })
}

fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// The size of the NCA an NCZ expands to.
fn nca_size(h: &Head) -> u64 {
    match &h.block {
        Some(b) => HEAD + b.size,
        None => h.secs.iter().map(|s| s.offset + s.size).chain([HEAD]).max().unwrap(),
    }
}

/// Encrypts the decompressed stream again as it passes through, hashes the result, and writes it on.
struct Sink<'a, W: Write> {
    w: &'a mut W,
    /// (start, end, key and counter for AES-CTR, or none for plain), in order, from `HEAD` on
    segs: Vec<(u64, u64, Option<Aes128>, [u8; 8])>,
    pos: u64,
    end: u64,
    sha: Sha256,
    progress: &'a mut dyn FnMut(u64),
}

impl<'a, W: Write> Sink<'a, W> {
    fn new(w: &'a mut W, h: &Head, name: &str, progress: &'a mut dyn FnMut(u64)) -> io::Result<Self> {
        let total = nca_size(h);
        let mut segs = Vec::new();
        let mut pos = HEAD;
        for s in &h.secs {
            let (mut o, mut size) = (s.offset, s.size);
            if o < pos && pos == HEAD {
                // a first section that starts inside the header (0xC00, nsz 5.0): its part there is already in the header
                size = (o + size).saturating_sub(pos);
                o = pos;
            }
            if o < pos {
                return bad(format!("{name}: NCZ sections overlap at {o:#x}"));
            }
            if o > pos {
                segs.push((pos, o, None, [0; 8])); // a gap: plain
            }
            let enc = matches!(s.crypto, 3 | 4);
            if enc && o & 15 != 0 {
                return bad(format!("{name}: an NCZ section at {o:#x} is not 16-byte aligned"));
            }
            segs.push((o, o + size, enc.then(|| Aes128::new(&s.key)), s.ctr[..8].try_into().unwrap()));
            pos = o + size;
        }
        if pos < total {
            segs.push((pos, total, None, [0; 8])); // past the last section, when the block header counts more
        }
        Ok(Sink { w, segs, pos: HEAD, end: total, sha: Sha256::new(), progress })
    }

    fn put(&mut self, d: &[u8]) -> io::Result<()> {
        self.sha.update(d);
        self.w.write_all(d)?;
        (self.progress)(d.len() as u64);
        Ok(())
    }
}

impl<W: Write> Write for Sink<'_, W> {
    fn write(&mut self, mut d: &[u8]) -> io::Result<usize> {
        let n = d.len();
        while !d.is_empty() {
            if self.pos >= self.end {
                return bad("the stream is longer than the NCA it should hold".into());
            }
            let i = self.segs.partition_point(|s| s.1 <= self.pos);
            let Some((start, end, aes, ctr8)) = self.segs.get(i).map(|s| (s.0, s.1, s.2.clone(), s.3)) else {
                return bad("the stream is longer than its sections".into());
            };
            debug_assert!(start <= self.pos);
            let k = (d.len() as u64).min(end - self.pos) as usize;
            match aes {
                None => self.put(&d[..k])?,
                Some(aes) => {
                    let skew = (self.pos & 15) as usize;
                    let mut tmp = vec![0u8; skew + k];
                    tmp[skew..].copy_from_slice(&d[..k]);
                    let mut ctr = [0u8; 16];
                    ctr[..8].copy_from_slice(&ctr8);
                    ctr[8..].copy_from_slice(&((self.pos - skew as u64) >> 4).to_be_bytes());
                    aes.ctr_apply(ctr, &mut tmp);
                    self.put(&tmp[skew..])?;
                }
            }
            self.pos += k as u64;
            d = &d[k..];
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Converts the NCZ at `at` (`len` bytes in the dump) to an NCA written to `w`; returns the SHA-256 of the NCA.
fn ncz<R: Read + Seek, W: Write>(r: &mut R, at: u64, len: u64, name: &str, w: &mut W, progress: &mut dyn FnMut(u64)) -> io::Result<[u8; 32]> {
    r.seek(SeekFrom::Start(at))?;
    let hd = rd(r, HEAD as usize)?;
    let h = head(r, at, name)?;
    let mut sink = Sink::new(w, &h, name, progress)?;
    sink.put(&hd)?;
    match &h.block {
        None => {
            let left = (at + len).saturating_sub(r.stream_position()?);
            let take = r.by_ref().take(left);
            let mut input = BufReader::with_capacity(1 << 20, take);
            Decoder::default().decode(&mut input, &mut sink).map_err(|e| io::Error::new(e.kind(), format!("{name} does not decompress ({e}): a damaged dump")))?;
        }
        Some(b) => {
            let last = b.sizes.len().saturating_sub(1);
            for (i, &cs) in b.sizes.iter().enumerate() {
                let want = if i < last { b.bs } else { b.size % b.bs + if b.size % b.bs == 0 { b.bs } else { 0 } };
                let c = rd(r, cs as usize)?;
                if (cs as u64) < want {
                    let mut out = Vec::with_capacity(want as usize);
                    nx_zstd::decompress(&mut &c[..], &mut out).map_err(|e| io::Error::new(e.kind(), format!("{name} block {i} does not decompress ({e}): a damaged dump")))?;
                    sink.write_all(&out)?;
                } else {
                    sink.write_all(&c)?; // stored as it is when compressing did not make it smaller
                }
            }
        }
    }
    if sink.pos != sink.end {
        return bad(format!("{name}: an NCZ ends {} bytes before its NCA does (a damaged or cut-short dump)", sink.end - sink.pos));
    }
    Ok(sink.sha.finish())
}

/// The header of a PFS0 holding these files in this order; its string table pads the header to a multiple of 0x20 (as upstream writes it).
fn pfs0_header(names: &[String], sizes: &[u64]) -> Vec<u8> {
    let mut strs: Vec<u8> = names.iter().flat_map(|n| n.bytes().chain([0])).collect();
    let hlen = 16 + 0x18 * names.len() + strs.len();
    strs.resize(strs.len() + (0x20 - hlen % 0x20) % 0x20, 0);
    let mut out = b"PFS0".to_vec();
    out.extend((names.len() as u32).to_le_bytes());
    out.extend((strs.len() as u32).to_le_bytes());
    out.extend(0u32.to_le_bytes());
    let (mut off, mut name_off) = (0u64, 0u32);
    for (n, s) in names.iter().zip(sizes) {
        out.extend(off.to_le_bytes());
        out.extend(s.to_le_bytes());
        out.extend(name_off.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        off += s;
        name_off += n.len() as u32 + 1;
    }
    out.extend(strs);
    out
}

/// What a converted dump will hold, from the headers alone: `(name in the NSP, offset in the dump, size in the dump, size in the NSP, is an NCZ)`.
pub fn plan<R: Read + Seek>(r: &mut R) -> io::Result<Vec<(String, u64, u64, u64, bool)>> {
    let mut out = Vec::new();
    for e in nx_container::dump_files(r)? {
        let is_ncz = e.name.to_ascii_lowercase().ends_with(".ncz");
        let (name, size) = if is_ncz { (format!("{}.nca", &e.name[..e.name.len() - 4]), nca_size(&head(r, e.offset, &e.name)?)) } else { (e.name.clone(), e.size) };
        out.push((name, e.offset, e.size, size, is_ncz));
    }
    Ok(out)
}

/// The size of the NSP `decompress` will write.
pub fn size<R: Read + Seek>(r: &mut R) -> io::Result<u64> {
    let p = plan(r)?;
    let names: Vec<String> = p.iter().map(|e| e.0.clone()).collect();
    let sizes: Vec<u64> = p.iter().map(|e| e.3).collect();
    Ok(pfs0_header(&names, &sizes).len() as u64 + sizes.iter().sum::<u64>())
}

/// Writes the NSP made from the compressed dump `r` to `w`. `input` names the dump in error messages; `progress` is called with each number of bytes written.
pub fn decompress<R: Read + Seek, W: Write>(r: &mut R, w: &mut W, input: &str, progress: &mut dyn FnMut(u64)) -> io::Result<()> {
    let p = plan(r)?;
    let names: Vec<String> = p.iter().map(|e| e.0.clone()).collect();
    let sizes: Vec<u64> = p.iter().map(|e| e.3).collect();
    // a card dump has no tickets and is never converted, so it keeps the name check; an NSZ with no ticket does not
    r.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    let ticketless = &magic == b"PFS0" && !p.iter().any(|e| e.0.to_ascii_lowercase().ends_with(".tik"));
    let hdr = pfs0_header(&names, &sizes);
    w.write_all(&hdr)?;
    progress(hdr.len() as u64);
    for (name, off, len, want, is_ncz) in &p {
        let mut count = 0u64;
        if *is_ncz {
            let mut prog = |n: u64| {
                count += n;
                progress(n);
            };
            let mut counted = Counter { w: &mut *w, n: 0 };
            let sha = ncz(r, *off, *len, &format!("{input}: {name}"), &mut counted, &mut prog).map_err(|e| io::Error::new(e.kind(), format!("{input}: {e}")))?;
            let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
            if !ticketless && stem.len() == EXPECT_NAME_LEN && stem.bytes().all(|b| b.is_ascii_hexdigit()) && hex(&sha)[..32] != stem {
                return bad(format!("{input}: {name} decompressed to an NCA whose SHA-256 is not its name ({}): a damaged dump", &hex(&sha)[..32]));
            }
            if counted.n != *want {
                return bad(format!("{input}: {name} came out {} bytes, its header says {want}", counted.n));
            }
        } else {
            r.seek(SeekFrom::Start(*off))?;
            let mut left = *len;
            let mut buf = vec![0u8; 1 << 20];
            while left > 0 {
                let k = (buf.len() as u64).min(left) as usize;
                r.read_exact(&mut buf[..k])?;
                w.write_all(&buf[..k])?;
                progress(k as u64);
                left -= k as u64;
            }
        }
    }
    w.flush()
}

struct Counter<'a, W: Write> {
    w: &'a mut W,
    n: u64,
}
impl<W: Write> Write for Counter<'_, W> {
    fn write(&mut self, d: &[u8]) -> io::Result<usize> {
        self.w.write_all(d)?;
        self.n += d.len() as u64;
        Ok(d.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const KEY: [u8; 16] = [0x42; 16];
    const CTR: [u8; 16] = [9, 8, 7, 6, 5, 4, 3, 2, 0, 0, 0, 0, 0, 0, 0, 0];

    /// A zstd frame of raw blocks (a valid "compressor" that does not compress).
    fn store(d: &[u8]) -> Vec<u8> {
        let mut out = vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, 0x58]; // no size fields, window 2^21
        let chunks: Vec<&[u8]> = if d.is_empty() { vec![&[][..]] } else { d.chunks(1 << 16).collect() };
        for (i, c) in chunks.iter().enumerate() {
            let h = (c.len() as u32) << 3 | ((i + 1 == chunks.len()) as u32);
            out.extend(&h.to_le_bytes()[..3]);
            out.extend(*c);
        }
        out
    }

    fn pfs0(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let names: Vec<String> = files.iter().map(|f| f.0.to_string()).collect();
        let sizes: Vec<u64> = files.iter().map(|f| f.1.len() as u64).collect();
        let mut out = pfs0_header(&names, &sizes);
        for (_, d) in files {
            out.extend(d);
        }
        out
    }

    fn sect(o: u64, s: u64, crypto: u64) -> Vec<u8> {
        let mut v = o.to_le_bytes().to_vec();
        v.extend(s.to_le_bytes());
        v.extend(crypto.to_le_bytes());
        v.extend(0u64.to_le_bytes());
        v.extend(KEY);
        v.extend(CTR);
        v
    }

    /// The NCA an NCZ must expand to: header, then a plain part and an encrypted part, built independently of the code under test.
    fn expected(header: &[u8], plain: &[u8], clear_len: usize) -> Vec<u8> {
        let mut nca = header.to_vec();
        let mut body = plain.to_vec();
        let enc_off = HEAD + clear_len as u64;
        let mut ctr = CTR;
        ctr[8..].copy_from_slice(&(enc_off >> 4).to_be_bytes());
        Aes128::new(&KEY).ctr_apply(ctr, &mut body[clear_len..]);
        nca.extend(body);
        nca
    }

    fn sha_name(nca: &[u8]) -> String {
        hex(&Sha256::digest(nca))[..32].to_string()
    }

    /// An NCZ: header, NCZSECTN with a plain section then a CTR section, then the compressed stream.
    fn make_ncz(header: &[u8], plain: &[u8], clear_len: usize, stream: Vec<u8>, block: Option<Vec<u8>>) -> Vec<u8> {
        let mut v = header.to_vec();
        v.extend(b"NCZSECTN");
        v.extend(2u64.to_le_bytes());
        v.extend(sect(HEAD, clear_len as u64, 0));
        v.extend(sect(HEAD + clear_len as u64, (plain.len() - clear_len) as u64, 3));
        match block {
            Some(b) => v.extend(b),
            None => v.extend(stream),
        }
        v
    }

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 13 + 5) as u8).collect()
    }

    #[test]
    fn stream_ncz_becomes_the_original_nca() {
        let header = data(HEAD as usize);
        let clear = 5008; // plain bytes, then 777 encrypted; the encrypted part must start 16-byte aligned (0x4000 + 5008 does)
        let plain = data(clear + 777);
        let plain = &plain[..];
        let nca = expected(&header, plain, clear);
        let name = format!("{}.ncz", sha_name(&nca));
        let nsz = pfs0(&[(&name, make_ncz(&header, plain, clear, store(plain), None)), ("x.tik", vec![1, 2, 3])]);
        let mut out = Vec::new();
        let mut total = 0;
        decompress(&mut Cursor::new(&nsz), &mut out, "t.nsz", &mut |n| total += n).unwrap();
        assert_eq!(total, out.len() as u64);
        assert_eq!(size(&mut Cursor::new(&nsz)).unwrap(), out.len() as u64);
        let files = nx_container::dump_files(&mut Cursor::new(&out)).unwrap();
        assert_eq!(files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), [format!("{}.nca", sha_name(&nca)).as_str(), "x.tik"]);
        assert_eq!(&out[files[0].offset as usize..(files[0].offset + files[0].size) as usize], &nca[..]);
        assert_eq!(&out[files[1].offset as usize..], &[1, 2, 3]);
    }

    #[test]
    fn block_ncz_with_stored_and_compressed_blocks() {
        let header = data(HEAD as usize);
        let clear = 0x4000usize; // plain part and encrypted part, in 16 KiB blocks
        let plain: Vec<u8> = (0..clear + 0x4000 + 100).map(|i| if i < clear { 7 } else { (i / 3) as u8 }).collect(); // the first block compresses (RLE), the others are stored
        let nca = expected(&header, &plain, clear);
        let bs = 1u64 << 14;
        let body = &plain[..];
        let mut blocks = Vec::new();
        let mut sizes = Vec::new();
        for (i, c) in body.chunks(bs as usize).enumerate() {
            if i == 0 {
                // a real zstd frame: one RLE block of 0x4000 sevens (window descriptor, block header 0x20001 << ... as RLE)
                let mut f = vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, 0x58];
                let h = (c.len() as u32) << 3 | 2 | 1; // RLE, last
                f.extend(&h.to_le_bytes()[..3]);
                f.push(7);
                sizes.push(f.len() as u32);
                blocks.extend(f);
            } else {
                sizes.push(c.len() as u32); // not smaller: stored as it is
                blocks.extend(c);
            }
        }
        let mut bh = b"NCZBLOCK".to_vec();
        bh.extend([2, 1, 0, 14]);
        bh.extend((sizes.len() as u32).to_le_bytes());
        bh.extend((plain.len() as u64).to_le_bytes());
        for s in &sizes {
            bh.extend(s.to_le_bytes());
        }
        bh.extend(blocks);
        let ncz = make_ncz(&header, &plain, clear, vec![], Some(bh));
        let nsz = pfs0(&[(&format!("{}.ncz", sha_name(&nca)), ncz), ("a.tik", vec![0])]);
        let mut out = Vec::new();
        decompress(&mut Cursor::new(&nsz), &mut out, "t.nsz", &mut |_| {}).unwrap();
        let f = &nx_container::dump_files(&mut Cursor::new(&out)).unwrap()[0];
        assert!(out[f.offset as usize..(f.offset + f.size) as usize] == nca[..]);
    }

    #[test]
    fn a_libzstd_stream_goes_through() {
        let header = data(HEAD as usize);
        let src = std::fs::read(format!("{}/../nx-zstd/tests/data/runs_1.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut plain = Vec::new();
        nx_zstd::decompress(&mut &src[..], &mut plain).unwrap();
        let nca = expected(&header, &plain, 0x10);
        let nsz = pfs0(&[(&format!("{}.ncz", sha_name(&nca)), make_ncz(&header, &plain, 0x10, src, None))]);
        let mut out = Vec::new();
        decompress(&mut Cursor::new(&nsz), &mut out, "t.nsz", &mut |_| {}).unwrap();
        let f = &nx_container::dump_files(&mut Cursor::new(&out)).unwrap()[0];
        assert!(out[f.offset as usize..(f.offset + f.size) as usize] == nca[..]);
    }

    #[test]
    fn damage_is_reported() {
        let header = data(HEAD as usize);
        let plain = data(0x400);
        let nca = expected(&header, &plain, 0x10);
        let good = make_ncz(&header, &plain, 0x10, store(&plain), None);
        // a wrong name: the SHA-256 check fires (with a ticket present)
        let wrong = pfs0(&[("00000000000000000000000000000000.ncz", good.clone()), ("a.tik", vec![0])]);
        let e = decompress(&mut Cursor::new(&wrong), &mut Vec::new(), "t.nsz", &mut |_| {}).unwrap_err().to_string();
        assert!(e.contains("SHA-256 is not its name") && e.contains(&sha_name(&nca)), "{e}");
        // the same file with no ticket is accepted
        let ticketless = pfs0(&[("00000000000000000000000000000000.ncz", good.clone())]);
        decompress(&mut Cursor::new(&ticketless), &mut Vec::new(), "t.nsz", &mut |_| {}).unwrap();
        // cut short
        let mut cut = good.clone();
        cut.truncate(cut.len() - 100);
        let nsz = pfs0(&[(&format!("{}.ncz", sha_name(&nca)), cut), ("a.tik", vec![0])]);
        assert!(decompress(&mut Cursor::new(&nsz), &mut Vec::new(), "t.nsz", &mut |_| {}).is_err());
        // not an NCZ at all
        let junk = pfs0(&[("a.ncz", vec![0; 0x5000])]);
        assert!(plan(&mut Cursor::new(&junk)).is_err());
    }

    #[test]
    fn container_plumbing() {
        assert!(is_compressed(std::path::Path::new("A.NSZ")) && is_compressed(std::path::Path::new("b.xcz")) && !is_compressed(std::path::Path::new("c.nsp")));
        let h = pfs0_header(&["a.nca".to_string()], &[5]);
        assert_eq!(h.len() % 0x20, 0);
    }
}
