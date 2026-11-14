//! Readers for what an NCA section holds: a window over any `Read + Seek`, the RomFS tables, and extraction to a folder.
//!
//! RomFS layout (public Switch documentation, as in LibHac's `RomFsHeader`): a 0x50-byte header (header size, then offset and size of the directory hash table, directory table,
//! file hash table, file table, and the offset of the file data), directory entries (parent, sibling, child dir, child file, next hash, name length, name) and file entries
//! (parent, sibling, data offset u64, data size u64, next hash, name length, name). 0xFFFFFFFF ends a chain. Checked on synthetic images only.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

/// `[start, start + len)` of an inner reader, as its own reader.
pub struct SubReader<R> {
    inner: R,
    start: u64,
    len: u64,
    pos: u64,
}

impl<R: Read + Seek> SubReader<R> {
    pub fn new(inner: R, start: u64, len: u64) -> Self {
        SubReader { inner, start, len, pos: 0 }
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<R: Read + Seek> Read for SubReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (buf.len() as u64).min(self.len.saturating_sub(self.pos)) as usize;
        if n == 0 {
            return Ok(0);
        }
        self.inner.seek(SeekFrom::Start(self.start + self.pos))?;
        let k = self.inner.read(&mut buf[..n])?;
        self.pos += k as u64;
        Ok(k)
    }
}

impl<R: Read + Seek> Seek for SubReader<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
        };
        if p < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn at<R: Read + Seek>(f: &mut R, off: u64, len: usize) -> io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(off))?;
    let mut v = vec![0u8; len];
    f.read_exact(&mut v)?;
    Ok(v)
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

const NONE: u32 = 0xFFFF_FFFF;
/// Upper bound for a table, so a corrupt header cannot ask for gigabytes.
const MAX_TABLE: u64 = 256 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomFile {
    /// path from the root, `/`-separated, no leading slash
    pub path: String,
    /// offset in the RomFS reader
    pub offset: u64,
    pub size: u64,
}

/// A RomFS: its files by path. Directories are implied by the paths (empty directories are listed in `dirs`).
#[derive(Debug, Clone, Default)]
pub struct RomFs {
    pub files: Vec<RomFile>,
    pub dirs: Vec<String>,
}

impl RomFs {
    pub fn parse<R: Read + Seek>(f: &mut R) -> io::Result<RomFs> {
        let h = at(f, 0, 0x50)?;
        if u64le(&h, 0) != 0x50 {
            return Err(bad("not a RomFS: header size is not 0x50"));
        }
        let (dir_off, dir_len, file_off, file_len, data_off) = (u64le(&h, 0x18), u64le(&h, 0x20), u64le(&h, 0x38), u64le(&h, 0x40), u64le(&h, 0x48));
        if dir_len > MAX_TABLE || file_len > MAX_TABLE || dir_len < 0x18 {
            return Err(bad("RomFS table sizes are not plausible"));
        }
        let dirs = at(f, dir_off, dir_len as usize)?;
        let files = at(f, file_off, file_len as usize)?;
        let mut out = RomFs::default();
        let mut seen = 0usize; // a corrupt table can loop; one visit per entry at most
        let limit = (dir_len as usize + file_len as usize) / 0x14 + 2;
        // iterative walk: (dir entry offset, path prefix)
        let mut stack: Vec<(u32, String)> = vec![(0, String::new())];
        while let Some((d, prefix)) = stack.pop() {
            let d = d as usize;
            if d + 0x18 > dirs.len() {
                return Err(bad("RomFS directory entry out of range"));
            }
            let (mut child_dir, mut child_file) = (u32le(&dirs, d + 8), u32le(&dirs, d + 12));
            let mut had = false;
            while child_file != NONE {
                let o = child_file as usize;
                if o + 0x20 > files.len() {
                    return Err(bad("RomFS file entry out of range"));
                }
                let nlen = u32le(&files, o + 0x1c) as usize;
                if o + 0x20 + nlen > files.len() {
                    return Err(bad("RomFS file name out of range"));
                }
                let name = String::from_utf8_lossy(&files[o + 0x20..o + 0x20 + nlen]).into_owned();
                let (off, size) = (u64le(&files, o + 8), u64le(&files, o + 0x10));
                out.files.push(RomFile { path: format!("{prefix}{name}"), offset: data_off.checked_add(off).ok_or_else(|| bad("RomFS offset overflows"))?, size });
                had = true;
                child_file = u32le(&files, o + 4);
                seen += 1;
                if seen > limit {
                    return Err(bad("RomFS file table loops"));
                }
            }
            while child_dir != NONE {
                let o = child_dir as usize;
                if o + 0x18 > dirs.len() {
                    return Err(bad("RomFS directory entry out of range"));
                }
                let nlen = u32le(&dirs, o + 0x14) as usize;
                if o + 0x18 + nlen > dirs.len() {
                    return Err(bad("RomFS directory name out of range"));
                }
                let name = String::from_utf8_lossy(&dirs[o + 0x18..o + 0x18 + nlen]).into_owned();
                stack.push((child_dir, format!("{prefix}{name}/")));
                had = true;
                child_dir = u32le(&dirs, o + 4);
                seen += 1;
                if seen > limit {
                    return Err(bad("RomFS directory table loops"));
                }
            }
            if !had && !prefix.is_empty() {
                out.dirs.push(prefix.trim_end_matches('/').to_string());
            }
        }
        out.files.sort_by(|a, b| a.path.cmp(&b.path));
        out.dirs.sort();
        Ok(out)
    }

    pub fn find(&self, path: &str) -> Option<&RomFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// A path from an archive made safe to join onto an output folder: no `..`, no absolute parts, no empty names. `None` when it is not.
pub fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    let mut any = false;
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(n) if !n.is_empty() => {
                out.push(n);
                any = true;
            }
            _ => return None,
        }
    }
    any.then_some(out)
}

/// Copies `size` bytes at `offset` of `f` to `dst`, creating parent folders.
pub fn copy_out<R: Read + Seek>(f: &mut R, offset: u64, size: u64, dst: &Path) -> io::Result<()> {
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p)?;
    }
    f.seek(SeekFrom::Start(offset))?;
    let mut o = io::BufWriter::new(fs::File::create(dst)?);
    let mut buf = vec![0u8; 1 << 20];
    let mut left = size;
    while left > 0 {
        let n = (buf.len() as u64).min(left) as usize;
        let k = f.read(&mut buf[..n])?;
        if k == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("{} ends {left} bytes early", dst.display())));
        }
        o.write_all(&buf[..k])?;
        left -= k as u64;
    }
    o.flush()
}

/// Writes every file of the RomFS under `out`; returns the number of files and bytes. A name that would leave `out` stops the extraction.
pub fn extract_romfs<R: Read + Seek>(f: &mut R, out: &Path) -> io::Result<(usize, u64)> {
    let rom = RomFs::parse(f)?;
    fs::create_dir_all(out)?;
    for d in &rom.dirs {
        fs::create_dir_all(safe_join(out, d).ok_or_else(|| bad("unsafe directory name in the RomFS"))?)?;
    }
    let mut bytes = 0;
    for e in &rom.files {
        let dst = safe_join(out, &e.path).ok_or_else(|| bad("unsafe file name in the RomFS"))?;
        copy_out(f, e.offset, e.size, &dst)?;
        bytes += e.size;
    }
    Ok((rom.files.len(), bytes))
}

/// Writes every file of a PFS0 (an ExeFS) under `out`.
pub fn extract_pfs0<R: Read + Seek>(f: &mut R, out: &Path) -> io::Result<usize> {
    let files = nx_container::dump_files(f)?;
    fs::create_dir_all(out)?;
    for e in &files {
        let dst = safe_join(out, &e.name).ok_or_else(|| bad("unsafe file name in the PFS0"))?;
        copy_out(f, e.offset, e.size, &dst)?;
    }
    Ok(files.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct B {
        dirs: Vec<u8>,
        files: Vec<u8>,
        data: Vec<u8>,
    }

    fn dir_entry(parent: u32, sibling: u32, child_dir: u32, child_file: u32, name: &str) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [parent, sibling, child_dir, child_file, NONE, name.len() as u32] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(name.as_bytes());
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v
    }

    fn file_entry(parent: u32, sibling: u32, off: u64, size: u64, name: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&parent.to_le_bytes());
        v.extend_from_slice(&sibling.to_le_bytes());
        v.extend_from_slice(&off.to_le_bytes());
        v.extend_from_slice(&size.to_le_bytes());
        v.extend_from_slice(&NONE.to_le_bytes());
        v.extend_from_slice(&(name.len() as u32).to_le_bytes());
        v.extend_from_slice(name.as_bytes());
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v
    }

    /// root: file "a.txt", dir "sub" { file "b.bin" }, dir "empty"
    fn image() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let a = b"hello".to_vec();
        let b: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        let mut bld = B { dirs: vec![], files: vec![], data: vec![] };
        // directory table: root at 0, sub, empty
        let root_len = dir_entry(0, NONE, 0, 0, "").len(); // 0x18
        let sub_off = root_len as u32;
        let sub_len = dir_entry(0, 0, NONE, 0, "sub").len() as u32;
        let empty_off = sub_off + sub_len;
        bld.dirs.extend(dir_entry(0, NONE, sub_off, 0, "")); // root: child dir sub, child file at 0
        bld.dirs.extend(dir_entry(0, empty_off, NONE, 0x28, "sub")); // the second file entry
        bld.dirs.extend(dir_entry(0, NONE, NONE, NONE, "empty"));
        let a_len = file_entry(0, NONE, 0, 5, "a.txt").len();
        assert_eq!(a_len, 0x28);
        bld.files.extend(file_entry(0, NONE, 0, a.len() as u64, "a.txt"));
        bld.files.extend(file_entry(sub_off, NONE, 0x10, b.len() as u64, "b.bin"));
        bld.data.extend_from_slice(&a);
        bld.data.resize(0x10, 0);
        bld.data.extend_from_slice(&b);
        // layout: header 0x50, dir table at 0x60, file table after it, data after that
        let dir_at = 0x60u64;
        let file_at = dir_at + bld.dirs.len() as u64;
        let data_at = (file_at + bld.files.len() as u64 + 0xF) & !0xF;
        let mut img = vec![0u8; data_at as usize];
        img[0..8].copy_from_slice(&0x50u64.to_le_bytes());
        img[0x18..0x20].copy_from_slice(&dir_at.to_le_bytes());
        img[0x20..0x28].copy_from_slice(&(bld.dirs.len() as u64).to_le_bytes());
        img[0x38..0x40].copy_from_slice(&file_at.to_le_bytes());
        img[0x40..0x48].copy_from_slice(&(bld.files.len() as u64).to_le_bytes());
        img[0x48..0x50].copy_from_slice(&data_at.to_le_bytes());
        img[dir_at as usize..dir_at as usize + bld.dirs.len()].copy_from_slice(&bld.dirs);
        img[file_at as usize..file_at as usize + bld.files.len()].copy_from_slice(&bld.files);
        img.extend_from_slice(&bld.data);
        (img, a, b)
    }

    #[test]
    fn walks_the_tables() {
        let (img, a, b) = image();
        let r = RomFs::parse(&mut Cursor::new(&img)).unwrap();
        assert_eq!(r.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a.txt", "sub/b.bin"]);
        assert_eq!(r.dirs, ["empty"]);
        let fa = r.find("a.txt").unwrap();
        assert_eq!(&img[fa.offset as usize..(fa.offset + fa.size) as usize], &a[..]);
        let fb = r.find("sub/b.bin").unwrap();
        assert_eq!(&img[fb.offset as usize..(fb.offset + fb.size) as usize], &b[..]);
    }

    #[test]
    fn extracts_to_disk() {
        let (img, a, b) = image();
        let dir = std::env::temp_dir().join(format!("nx-fs-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (n, bytes) = extract_romfs(&mut Cursor::new(&img), &dir).unwrap();
        assert_eq!((n, bytes), (2, 305));
        assert_eq!(fs::read(dir.join("a.txt")).unwrap(), a);
        assert_eq!(fs::read(dir.join("sub/b.bin")).unwrap(), b);
        assert!(dir.join("empty").is_dir());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corrupt_tables_are_errors_not_loops() {
        let (mut img, ..) = image();
        assert!(RomFs::parse(&mut Cursor::new(&img[..0x30])).is_err());
        img[0..8].copy_from_slice(&0x51u64.to_le_bytes());
        assert!(RomFs::parse(&mut Cursor::new(&img)).is_err());
        let (mut img, ..) = image();
        // make the first file point to itself as its sibling
        let file_at = u64le(&img, 0x38) as usize;
        img[file_at + 4..file_at + 8].copy_from_slice(&0u32.to_le_bytes());
        assert!(RomFs::parse(&mut Cursor::new(&img)).is_err());
        // a child dir pointing outside the table
        let (mut img, ..) = image();
        let dir_at = u64le(&img, 0x18) as usize;
        img[dir_at + 8..dir_at + 12].copy_from_slice(&0x7000u32.to_le_bytes());
        assert!(RomFs::parse(&mut Cursor::new(&img)).is_err());
    }

    #[test]
    fn unsafe_names_are_refused() {
        let base = Path::new("/out");
        assert_eq!(safe_join(base, "a/b.bin"), Some(PathBuf::from("/out/a/b.bin")));
        for bad in ["../x", "a/../../x", "/etc/passwd", "", "a//", "."] {
            assert!(safe_join(base, bad).is_none() || bad == "a//", "{bad}");
        }
        assert_eq!(safe_join(base, "a//"), Some(PathBuf::from("/out/a"))); // trailing slashes carry no component
    }

    #[test]
    fn sub_reader_is_a_window() {
        let data: Vec<u8> = (0..100).collect();
        let mut r = SubReader::new(Cursor::new(data), 10, 20);
        let mut v = Vec::new();
        r.read_to_end(&mut v).unwrap();
        assert_eq!(v, (10..30).collect::<Vec<u8>>());
        r.seek(SeekFrom::End(-5)).unwrap();
        let mut b = [0u8; 10];
        assert_eq!(r.read(&mut b).unwrap(), 5);
        assert_eq!(b[0], 25);
    }
}
