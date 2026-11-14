//! A patch (update) section: the base game's RomFS section with the update's changes laid over it.
//!
//! The patch section holds new data (AES-CTR with a per-range counter, "CTR-EX"), and two bucket trees: the relocation table says, for each range of the finished section, whether it
//! comes from the base (source index 0) or from the patch data (index 1) and at which offset; the subsection table gives the counter value of each range of the patch data.
//! Format as in the public Switch documentation, LibHac and Atmosphère; checked here on synthetic images only.
//!
//! Tree layout: the table starts with one 0x4000-byte offset node (index u32, count u32, end offset u64, then the first offset of each entry set), followed by the entry sets,
//! each 0x4000 bytes: a 0x10-byte header (index, count, end offset) then its entries. Relocation entries are 0x14 bytes (offset i64, source offset i64, source index u32),
//! subsection entries 0x10 bytes (offset i64, unused u32, counter u32).

use crate::section::{PatchInfo, Section, SectionReader};
use nx_crypto::Aes128;
use std::io::{self, Read, Seek, SeekFrom};

const NODE: usize = 0x4000;

fn bad(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.to_string())
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reloc {
    pub offset: u64,
    pub source_offset: u64,
    pub source: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sub {
    pub offset: u64,
    pub counter: u32,
}

/// Flattens a bucket tree into its entries, checking counts and that offsets rise. Returns the entries and the end offset of the last set.
fn tree<E>(t: &[u8], entry_size: usize, entries: u32, make: impl Fn(&[u8]) -> E, offset_of: impl Fn(&E) -> u64) -> io::Result<(Vec<E>, u64)> {
    let per = (NODE - 0x10) / entry_size;
    let sets = (entries as usize).div_ceil(per);
    if entries == 0 || t.len() < NODE * (1 + sets) {
        return Err(bad("bucket tree is shorter than its header says"));
    }
    let mut out = Vec::with_capacity(entries as usize);
    let mut end = 0;
    for s in 0..sets {
        let n = &t[NODE * (1 + s)..NODE * (2 + s)];
        let count = u32le(n, 4) as usize;
        let want = if s + 1 == sets { entries as usize - per * s } else { per };
        if count != want {
            return Err(bad("bucket tree entry set has an unexpected entry count"));
        }
        end = u64le(n, 8);
        for i in 0..count {
            out.push(make(&n[0x10 + i * entry_size..0x10 + (i + 1) * entry_size]));
        }
    }
    if out.windows(2).any(|w| offset_of(&w[0]) >= offset_of(&w[1])) {
        return Err(bad("bucket tree offsets do not rise"));
    }
    Ok((out, end))
}

pub fn parse_relocs(t: &[u8], entries: u32) -> io::Result<(Vec<Reloc>, u64)> {
    tree(t, 0x14, entries, |e| Reloc { offset: u64le(e, 0), source_offset: u64le(e, 8), source: u32le(e, 16) }, |r| r.offset)
}

pub fn parse_subs(t: &[u8], entries: u32) -> io::Result<(Vec<Sub>, u64)> {
    tree(t, 0x10, entries, |e| Sub { offset: u64le(e, 0), counter: u32le(e, 12) }, |s| s.offset)
}

/// The decrypted bytes of a patch section's table (plain AES-CTR with the section's own counter).
fn read_table<R: Read + Seek>(inner: R, nca_at: u64, sec: &Section, key: &[u8; 16], off: u64, size: u64) -> io::Result<Vec<u8>> {
    if size > (64 << 20) {
        return Err(bad("bucket tree table is implausibly large"));
    }
    let plain = Section { encryption: 3, patch: None, ..sec.clone() };
    let mut r = SectionReader::new(inner, nca_at, plain, key)?;
    r.seek(SeekFrom::Start(off))?;
    let mut v = vec![0u8; size as usize];
    r.read_exact(&mut v)?;
    Ok(v)
}

/// The patch section's own data, decrypted with per-range counters (CTR-EX). Offsets are from the start of the section.
pub struct PatchData<R> {
    inner: R,
    at: u64,
    sec: Section,
    aes: Aes128,
    subs: Vec<Sub>,
    pos: u64,
}

impl<R: Read + Seek> PatchData<R> {
    fn counter_for(&self, counter: u32, nca_off: u64) -> [u8; 16] {
        let mut c = [0u8; 16];
        for (i, b) in self.sec.counter.iter().enumerate() {
            c[7 - i] = *b;
        }
        c[4..8].copy_from_slice(&counter.to_be_bytes()); // the generation half is replaced by the range's counter
        c[8..].copy_from_slice(&(nca_off >> 4).to_be_bytes());
        c
    }
}

impl<R: Read + Seek> Read for PatchData<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.sec.len();
        if self.pos >= len || buf.is_empty() {
            return Ok(0);
        }
        let i = self.subs.partition_point(|s| s.offset <= self.pos).saturating_sub(1);
        let range_end = self.subs.get(i + 1).map_or(len, |s| s.offset).min(len);
        let want = (buf.len() as u64).min(range_end - self.pos) as usize;
        let nca_off = self.sec.start + self.pos;
        let skew = (nca_off & 15) as usize;
        let mut tmp = vec![0u8; skew + want];
        self.inner.seek(SeekFrom::Start(self.at + nca_off - skew as u64))?;
        let mut got = 0;
        while got < tmp.len() {
            match self.inner.read(&mut tmp[got..])? {
                0 => break,
                k => got += k,
            }
        }
        if got <= skew {
            return Ok(0);
        }
        tmp.truncate(got);
        let c = self.counter_for(self.subs[i].counter, nca_off - skew as u64);
        self.aes.ctr_apply(c, &mut tmp);
        let n = got - skew;
        buf[..n].copy_from_slice(&tmp[skew..]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for PatchData<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.sec.len() as i128 + d as i128,
        };
        if p < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

/// The finished section: base and patch merged. Read it whole, or wrap it in a `SubReader` over the patch section's `data_offset` / `data_size`.
pub struct Patched<B, P> {
    base: B,
    patch: PatchData<P>,
    relocs: Vec<Reloc>,
    size: u64,
    pos: u64,
}

impl<B: Read + Seek, P: Read + Seek> Patched<B, P> {
    /// `base` is the base NCA's section, decrypted and whole (not just its data level). `patch_inner` is the update's file, `sec` its BKTR section, `key` its section key.
    pub fn open(base: B, mut patch_inner: P, nca_at: u64, sec: &Section, key: &[u8; 16]) -> io::Result<Self>
    {
        let Some(PatchInfo { reloc_offset, reloc_size, reloc_entries, sub_offset, sub_size, sub_entries }) = sec.patch else {
            return Err(bad("the section has no BKTR tables"));
        };
        let rt = read_table(&mut patch_inner, nca_at, sec, key, reloc_offset, reloc_size)?;
        let st = read_table(&mut patch_inner, nca_at, sec, key, sub_offset, sub_size)?;
        let (relocs, size) = parse_relocs(&rt, reloc_entries)?;
        let (subs, _) = parse_subs(&st, sub_entries)?;
        if relocs[0].offset != 0 || subs[0].offset != 0 {
            return Err(bad("bucket trees do not start at offset 0"));
        }
        if relocs.iter().any(|r| r.source > 1) {
            return Err(bad("relocation entry names an unknown source"));
        }
        let patch = PatchData { inner: patch_inner, at: nca_at, sec: sec.clone(), aes: Aes128::new(key), subs, pos: 0 };
        Ok(Patched { base, patch, relocs, size, pos: 0 })
    }

    pub fn len(&self) -> u64 {
        self.size
    }
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
}

impl<B: Read + Seek, P: Read + Seek> Read for Patched<B, P> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let i = self.relocs.partition_point(|r| r.offset <= self.pos).saturating_sub(1);
        let e = self.relocs[i];
        let end = self.relocs.get(i + 1).map_or(self.size, |r| r.offset);
        let want = (buf.len() as u64).min(end - self.pos) as usize;
        let src = e.source_offset + (self.pos - e.offset);
        let n = if e.source == 0 {
            self.base.seek(SeekFrom::Start(src))?;
            self.base.read(&mut buf[..want])?
        } else {
            self.patch.seek(SeekFrom::Start(src))?;
            self.patch.read(&mut buf[..want])?
        };
        self.pos += n as u64;
        Ok(n)
    }
}

impl<B: Read + Seek, P: Read + Seek> Seek for Patched<B, P> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.size as i128 + d as i128,
        };
        if p < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn set(entries: &[Vec<u8>], end: u64) -> Vec<u8> {
        let mut n = vec![0u8; NODE];
        n[4..8].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        n[8..16].copy_from_slice(&end.to_le_bytes());
        for (i, e) in entries.iter().enumerate() {
            let sz = e.len();
            n[0x10 + i * sz..0x10 + (i + 1) * sz].copy_from_slice(e);
        }
        n
    }

    fn table(entries: &[Vec<u8>], end: u64) -> Vec<u8> {
        let mut t = set(&[], end); // the offset node: contents unused by the reader
        t.extend(set(entries, end));
        t
    }

    fn reloc(o: u64, s: u64, src: u32) -> Vec<u8> {
        let mut v = o.to_le_bytes().to_vec();
        v.extend(s.to_le_bytes());
        v.extend(src.to_le_bytes());
        v
    }
    fn sub(o: u64, c: u32) -> Vec<u8> {
        let mut v = o.to_le_bytes().to_vec();
        v.extend(0u32.to_le_bytes());
        v.extend(c.to_le_bytes());
        v
    }

    #[test]
    fn tables_parse_and_reject_bad_input() {
        let t = table(&[reloc(0, 5, 0), reloc(0x100, 0, 1)], 0x200);
        let (r, end) = parse_relocs(&t, 2).unwrap();
        assert_eq!((r.len(), end, r[1]), (2, 0x200, Reloc { offset: 0x100, source_offset: 0, source: 1 }));
        assert!(parse_relocs(&t, 3).is_err()); // count mismatch
        assert!(parse_relocs(&t[..NODE], 2).is_err());
        assert!(parse_relocs(&table(&[reloc(0x100, 0, 0), reloc(0x100, 0, 1)], 0x200), 2).is_err()); // offsets must rise
        assert!(parse_subs(&table(&[sub(0, 5)], 0x200), 0).is_err());
        let (s, _) = parse_subs(&table(&[sub(0, 5), sub(0x200, 9)], 0x400), 2).unwrap();
        assert_eq!(s[1], Sub { offset: 0x200, counter: 9 });
    }

    const KEY: [u8; 16] = [0x61; 16];
    const RAW: [u8; 8] = [10, 20, 30, 40, 50, 60, 70, 80]; // header counter bytes: generation 0x281E140A (LE), secure value 0x50443C32... as the file stores them
    const NCA_AT: u64 = 0x100; // the update NCA sits at 0x100 in its container
    const SEC_START: u64 = 0xC00;

    fn ctr(counter: Option<u32>, nca_off: u64) -> [u8; 16] {
        let mut c = [0u8; 16];
        for (i, b) in RAW.iter().enumerate() {
            c[7 - i] = *b;
        }
        if let Some(k) = counter {
            c[4..8].copy_from_slice(&k.to_be_bytes());
        }
        c[8..].copy_from_slice(&(nca_off >> 4).to_be_bytes());
        c
    }

    #[test]
    fn merges_base_and_patch_ranges() {
        // base section: 0x1000 bytes of a pattern
        let base: Vec<u8> = (0..0x1000u32).map(|i| (i * 3) as u8).collect();
        // patch physical section: data 0..0x400 (two counter ranges), reloc table at 0x400, sub table after it
        let new_data: Vec<u8> = (0..0x400u32).map(|i| (i * 5 + 1) as u8).collect();
        let reloc_off = 0x400u64;
        let rt = table(&[reloc(0, 0x100, 0), reloc(0x400, 0, 1), reloc(0x800, 0x500, 0)], 0xC00);
        let sub_off = reloc_off + rt.len() as u64;
        let phys_len = sub_off + 0x8000;
        let st = table(&[sub(0, 5), sub(0x200, 9), sub(reloc_off, 0x1234)], phys_len);
        assert_eq!(st.len(), 0x8000);
        // encrypt the physical section the way an update does
        let mut phys = new_data.clone();
        phys.extend(&rt);
        phys.extend(&st);
        let aes = Aes128::new(&KEY);
        aes.ctr_apply(ctr(Some(5), SEC_START), &mut phys[..0x200]);
        aes.ctr_apply(ctr(Some(9), SEC_START + 0x200), &mut phys[0x200..0x400]);
        // tables use the plain section counter
        aes.ctr_apply(ctr(None, SEC_START + reloc_off), &mut phys[0x400..]);
        let mut file = vec![0xEE; NCA_AT as usize + SEC_START as usize];
        file.extend(&phys);
        let sec = Section {
            index: 1,
            start: SEC_START,
            end: SEC_START + phys_len,
            fs_type: 0,
            hash_type: 3,
            encryption: 4,
            counter: RAW,
            data_offset: 0,
            data_size: 0xC00,
            patch: Some(PatchInfo { reloc_offset: reloc_off, reloc_size: rt.len() as u64, reloc_entries: 3, sub_offset: sub_off, sub_size: st.len() as u64, sub_entries: 3 }),
        };
        // `ctr(Some(..), ..)` above uses NCA-relative offsets, as the reader does
        let mut p = Patched::open(Cursor::new(base.clone()), Cursor::new(file.clone()), NCA_AT, &sec, &KEY).unwrap();
        assert_eq!(p.len(), 0xC00);
        let mut all = Vec::new();
        p.read_to_end(&mut all).unwrap();
        let mut want = base[0x100..0x500].to_vec();
        want.extend(&new_data);
        want.extend(&base[0x500..0x900]);
        assert_eq!(all.len(), want.len());
        assert!(all == want, "first difference at {:?}", all.iter().zip(&want).position(|(a, b)| a != b));
        for (off, n) in [(0x3F0u64, 0x20usize), (0x5, 0x7), (0x1F8, 0x10), (0x7FF, 3), (0xBF0, 0x10)] {
            p.seek(SeekFrom::Start(off)).unwrap();
            let mut b = vec![0u8; n];
            p.read_exact(&mut b).unwrap();
            assert_eq!(b, &want[off as usize..off as usize + n], "offset {off:#x}");
        }
    }

    #[test]
    fn a_section_without_tables_is_refused() {
        let sec = Section { index: 0, start: 0, end: 0x200, fs_type: 0, hash_type: 1, encryption: 3, counter: [0; 8], data_offset: 0, data_size: 0x200, patch: None };
        assert!(Patched::open(Cursor::new(vec![]), Cursor::new(vec![0u8; 0x300]), 0, &sec, &KEY).is_err());
    }
}
