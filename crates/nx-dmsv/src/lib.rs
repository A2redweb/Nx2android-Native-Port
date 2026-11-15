//! The Vulkan shader bundle, `shaders.dmsv` (format of upstream's `dm/android/tools/dmsv.py`, read by `dm/android/gfx_vk_shader.c`):
//! `"DMSV"`, u32 version, u32 count, then count 32-byte entries `{u64 key, u32 stage, u32 spv_off, u32 spv_len, u32 refl_off, u32 refl_len, u32 spv_size}` sorted by key (strictly
//! increasing: the host's binary search and its index check need it), then each entry's SPIR-V and reflection in that order. `spv_size` 0: the SPIR-V is stored as it is;
//! otherwise it is one zstd frame of `spv_len` bytes that decompresses to `spv_size` bytes (version 2).
//! The writer here writes version 1 (no zstd encoder in this workspace yet) and streams: each shader's bytes go to a body file as they are added, so a bundle of a gigabyte never sits in memory.

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const HDR: u32 = 12;
pub const ENT: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: u64,
    pub stage: u32,
    pub spv_off: u32,
    pub spv_len: u32,
    pub refl_off: u32,
    pub refl_len: u32,
    pub spv_size: u32,
}

pub struct Builder {
    dest: PathBuf,
    body_path: PathBuf,
    body: BufWriter<File>,
    /// (key, stage, spv offset in the body, spv length, refl length)
    ents: Vec<(u64, u32, u64, u32, u32)>,
    pos: u64,
}

impl Builder {
    pub fn new(dest: &Path) -> io::Result<Builder> {
        let body_path = dest.with_extension("dmsv.body");
        Ok(Builder { dest: dest.to_path_buf(), body: BufWriter::new(File::create(&body_path)?), body_path, ents: vec![], pos: 0 })
    }

    pub fn len(&self) -> usize {
        self.ents.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ents.is_empty()
    }

    /// Adds one shader: its SPIR-V (a module as translated, stored as it is) and its binary reflection.
    pub fn add(&mut self, key: u64, stage: u32, spv: &[u8], refl: &[u8]) -> io::Result<()> {
        if stage > 5 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("shader {key:016x}: stage {stage}")));
        }
        if refl.len() < 20 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("shader {key:016x}: reflection of {} bytes", refl.len())));
        }
        self.ents.push((key, stage, self.pos, spv.len() as u32, refl.len() as u32));
        self.body.write_all(spv)?;
        self.body.write_all(refl)?;
        self.pos += (spv.len() + refl.len()) as u64;
        Ok(())
    }

    /// Writes the bundle: header, the index sorted by key, then the shaders in index order. A key added twice keeps its last version (a capture of a shader the package already had).
    /// Returns the number of shaders.
    pub fn finish(mut self) -> io::Result<usize> {
        self.body.flush()?;
        drop(self.body);
        let mut order: Vec<usize> = (0..self.ents.len()).collect();
        order.sort_by_key(|&i| (self.ents[i].0, i));
        let mut keep: Vec<usize> = vec![];
        for i in order {
            match keep.last() {
                Some(&l) if self.ents[l].0 == self.ents[i].0 => *keep.last_mut().unwrap() = i,
                _ => keep.push(i),
            }
        }
        let table_end = HDR as u64 + ENT as u64 * keep.len() as u64;
        let tmp = self.dest.with_extension("dmsv.partial");
        let mut out = BufWriter::new(File::create(&tmp)?);
        out.write_all(b"DMSV")?;
        out.write_all(&1u32.to_le_bytes())?;
        out.write_all(&(keep.len() as u32).to_le_bytes())?;
        let mut off = table_end;
        for &i in &keep {
            let (key, stage, _, sl, rl) = self.ents[i];
            let (spv_off, refl_off) = (off, off + sl as u64);
            if refl_off + rl as u64 > u32::MAX as u64 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "the bundle would pass 4 GB (offsets are 32 bits)"));
            }
            for v in [key.to_le_bytes().to_vec(), stage.to_le_bytes().to_vec(), (spv_off as u32).to_le_bytes().to_vec(), sl.to_le_bytes().to_vec(), (refl_off as u32).to_le_bytes().to_vec(), rl.to_le_bytes().to_vec(), 0u32.to_le_bytes().to_vec()] {
                out.write_all(&v)?;
            }
            off = refl_off + rl as u64;
        }
        let mut body = File::open(&self.body_path)?;
        let mut buf = vec![0u8; 1 << 20];
        for &i in &keep {
            let (_, _, bo, sl, rl) = self.ents[i];
            body.seek(SeekFrom::Start(bo))?;
            let mut left = (sl + rl) as usize;
            while left > 0 {
                let n = left.min(buf.len());
                body.read_exact(&mut buf[..n])?;
                out.write_all(&buf[..n])?;
                left -= n;
            }
        }
        out.flush()?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        drop(body);
        std::fs::remove_file(&self.body_path)?;
        std::fs::rename(&tmp, &self.dest)?;
        Ok(keep.len())
    }
}

#[derive(Debug)]
pub struct Bundle<'a> {
    pub version: u32,
    pub entries: Vec<Entry>,
    data: &'a [u8],
}

impl<'a> Bundle<'a> {
    pub fn parse(d: &'a [u8]) -> Result<Bundle<'a>, String> {
        if d.len() < 12 || &d[..4] != b"DMSV" {
            return Err("not a shader bundle".into());
        }
        let u = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        let (version, n) = (u(4), u(8) as usize);
        if !(1..=2).contains(&version) {
            return Err(format!("shader bundle version {version}"));
        }
        let table_end = HDR as usize + ENT as usize * n;
        if table_end > d.len() {
            return Err(format!("{n} entries run past the file"));
        }
        let mut entries: Vec<Entry> = Vec::with_capacity(n);
        for i in 0..n {
            let o = HDR as usize + 32 * i;
            let e = Entry { key: u64::from_le_bytes(d[o..o + 8].try_into().unwrap()), stage: u(o + 8), spv_off: u(o + 12), spv_len: u(o + 16), refl_off: u(o + 20), refl_len: u(o + 24), spv_size: u(o + 28) };
            if e.stage > 5 || (i > 0 && e.key <= entries[i - 1].key) || (version == 1 && e.spv_size != 0) || (e.spv_off as usize) < table_end || e.spv_off as usize + e.spv_len as usize > d.len() || (e.refl_off as usize) < table_end || e.refl_off as usize + e.refl_len as usize > d.len() || e.refl_len < 20 {
                return Err(format!("invalid index record {i} (key {:016x})", e.key));
            }
            entries.push(e);
        }
        Ok(Bundle { version, entries, data: d })
    }

    pub fn find(&self, key: u64) -> Option<&Entry> {
        self.entries.binary_search_by_key(&key, |e| e.key).ok().map(|i| &self.entries[i])
    }

    pub fn spv(&self, e: &Entry) -> Result<Vec<u8>, String> {
        let b = &self.data[e.spv_off as usize..(e.spv_off + e.spv_len) as usize];
        if e.spv_size == 0 {
            return Ok(b.to_vec());
        }
        let out = nx_zstd::decompress_vec(b).map_err(|x| format!("shader {:016x}: {x}", e.key))?;
        if out.len() != e.spv_size as usize {
            return Err(format!("shader {:016x} decompresses to {} bytes, its entry says {}", e.key, out.len(), e.spv_size));
        }
        Ok(out)
    }

    pub fn refl(&self, e: &Entry) -> &[u8] {
        &self.data[e.refl_off as usize..(e.refl_off + e.refl_len) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nxdmsv-{}-{n}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        d
    }

    #[test]
    fn round_trip_sorted_with_last_duplicate_winning() {
        let p = tmp("a");
        let mut b = Builder::new(&p).unwrap();
        b.add(9, 1, b"spv-nine", &[9; 20]).unwrap();
        b.add(3, 0, b"spv-three", &[3; 24]).unwrap();
        b.add(9, 5, b"spv-nine-again", &[8; 20]).unwrap();
        assert_eq!(b.finish().unwrap(), 2);
        let d = std::fs::read(&p).unwrap();
        let bu = Bundle::parse(&d).unwrap();
        assert_eq!((bu.version, bu.entries.len()), (1, 2));
        assert_eq!(bu.entries[0].key, 3);
        let e = bu.find(9).unwrap();
        assert_eq!((e.stage, bu.spv(e).unwrap(), bu.refl(e)), (5, b"spv-nine-again".to_vec(), &[8u8; 20][..]));
        assert!(bu.find(4).is_none());
        assert!(!p.with_extension("dmsv.body").exists() && !p.with_extension("dmsv.partial").exists());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn bad_input_is_refused() {
        let p = tmp("b");
        let mut b = Builder::new(&p).unwrap();
        assert!(b.add(1, 6, b"x", &[0; 20]).is_err());
        assert!(b.add(1, 0, b"x", &[0; 19]).is_err());
        drop(b);
        let _ = std::fs::remove_file(p.with_extension("dmsv.body"));
        assert!(Bundle::parse(b"DMSV\x01\0\0\0\x01\0\0\0").is_err());
        assert!(Bundle::parse(b"nope").is_err());
    }

    #[test]
    fn reads_a_python_written_layout() {
        // dmsv.py version 1, one entry: key 7, stage 1, spv at 44 (12 + 32), 4 bytes, refl at 48, 20 bytes
        let mut d = b"DMSV".to_vec();
        for v in [1u32, 1] {
            d.extend(v.to_le_bytes());
        }
        d.extend(7u64.to_le_bytes());
        for v in [1u32, 44, 4, 48, 20, 0] {
            d.extend(v.to_le_bytes());
        }
        d.extend(b"SPV!");
        d.extend([5u8; 20]);
        let bu = Bundle::parse(&d).unwrap();
        assert_eq!(bu.spv(&bu.entries[0]).unwrap(), b"SPV!");
    }
}
