//! Switch dump containers: the file list of an NSP / NSZ (a PFS0) or an XCI / XCZ (the card's secure partition, an HFS0 in the root HFS0 the card header points to).
//! A port of `dump_files` in upstream's `build_ipa_from_nsp.py`, with the entry sizes added. No dependencies; works over any `Read + Seek`.

use std::io::{self, Read, Seek, SeekFrom};

/// A file inside a dump: its name, where its bytes start in the dump and how many there are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn read_up_to<R: Read>(r: &mut R, n: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match r.read(&mut buf[got..])? {
            0 => break,
            k => got += k,
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// A PFS0 (entries of 0x18 bytes) or an HFS0 (0x40) at `at`; empty when there is none.
/// The last name may end the string table without a NUL (four of one publisher's NSZ files do).
fn fs<R: Read + Seek>(f: &mut R, at: u64, esize: usize) -> io::Result<Vec<Entry>> {
    f.seek(SeekFrom::Start(at))?;
    let h = read_up_to(f, 16)?;
    if h.len() < 16 || (&h[..4] != b"PFS0" && &h[..4] != b"HFS0") {
        return Ok(Vec::new());
    }
    let n = u32le(&h, 4) as usize;
    let st = u32le(&h, 8) as usize;
    // a corrupt header must not make us allocate gigabytes
    if n > 0xFFFF || st > 0x1000_0000 {
        return Ok(Vec::new());
    }
    let ent = read_up_to(f, n * esize)?;
    let strs = read_up_to(f, st)?;
    if ent.len() < n * esize {
        return Ok(Vec::new());
    }
    let data = at + 16 + (n * esize) as u64 + st as u64;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = &ent[i * esize..];
        let (off, size, name_off) = (u64le(e, 0), u64le(e, 8), u32le(e, 16) as usize);
        let name = if name_off <= strs.len() {
            let tail = &strs[name_off..];
            let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            String::from_utf8_lossy(&tail[..end]).into_owned()
        } else {
            String::new()
        };
        out.push(Entry { name, offset: data + off, size });
    }
    Ok(out)
}

/// The files of a dump; empty for anything that is not an NSP / NSZ / XCI / XCZ.
pub fn dump_files<R: Read + Seek>(f: &mut R) -> io::Result<Vec<Entry>> {
    let top = fs(f, 0, 0x18)?;
    if !top.is_empty() {
        return Ok(top);
    }
    // a card image, or one dumped with its 0x1000-byte key area first
    for card in [0u64, 0x1000] {
        f.seek(SeekFrom::Start(card + 0x100))?;
        if read_up_to(f, 4)? != b"HEAD" {
            continue;
        }
        f.seek(SeekFrom::Start(card + 0x130))?;
        let b = read_up_to(f, 8)?;
        if b.len() < 8 {
            return Ok(Vec::new());
        }
        let root = card + u64le(&b, 0);
        for e in fs(f, root, 0x40)? {
            if e.name == "secure" {
                return fs(f, e.offset, 0x40);
            }
        }
        return Ok(Vec::new());
    }
    Ok(Vec::new())
}

/// What kind of file a name inside a dump is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// an NCA (`.nca`, or `.cnmt.nca` for the title's metadata)
    Nca,
    /// the metadata NCA: `<id>.cnmt.nca`
    Cnmt,
    /// a ticket or its certificate chain
    Ticket,
    /// an NSZ's compressed NCA
    Ncz,
    Other,
}

pub fn kind(name: &str) -> Kind {
    let n = name.to_ascii_lowercase();
    if n.ends_with(".cnmt.nca") {
        Kind::Cnmt
    } else if n.ends_with(".nca") {
        Kind::Nca
    } else if n.ends_with(".ncz") {
        Kind::Ncz
    } else if n.ends_with(".tik") || n.ends_with(".cert") {
        Kind::Ticket
    } else {
        Kind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A PFS0 or HFS0 image: header, entry table, string table, then the file bytes.
    fn build(magic: &[u8; 4], esize: usize, files: &[(&str, &[u8])], nul_on_last: bool) -> Vec<u8> {
        let mut strs = Vec::new();
        let mut offs = Vec::new();
        for (i, (n, _)) in files.iter().enumerate() {
            offs.push(strs.len() as u32);
            strs.extend_from_slice(n.as_bytes());
            if i + 1 < files.len() || nul_on_last {
                strs.push(0);
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(magic);
        out.extend_from_slice(&(files.len() as u32).to_le_bytes());
        out.extend_from_slice(&(strs.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut off = 0u64;
        for (i, (_, d)) in files.iter().enumerate() {
            let mut e = vec![0u8; esize];
            e[0..8].copy_from_slice(&off.to_le_bytes());
            e[8..16].copy_from_slice(&(d.len() as u64).to_le_bytes());
            e[16..20].copy_from_slice(&offs[i].to_le_bytes());
            out.extend_from_slice(&e);
            off += d.len() as u64;
        }
        out.extend_from_slice(&strs);
        for (_, d) in files {
            out.extend_from_slice(d);
        }
        out
    }

    #[test]
    fn nsp_lists_files_with_offsets() {
        let img = build(b"PFS0", 0x18, &[("a.nca", b"AAAA"), ("b.cnmt.nca", b"BB"), ("c.tik", b"C")], true);
        let es = dump_files(&mut Cursor::new(&img)).unwrap();
        assert_eq!(es.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["a.nca", "b.cnmt.nca", "c.tik"]);
        for (e, want) in es.iter().zip([&b"AAAA"[..], b"BB", b"C"]) {
            assert_eq!(&img[e.offset as usize..(e.offset + e.size) as usize], want);
        }
        assert_eq!(kind("b.cnmt.nca"), Kind::Cnmt);
        assert_eq!(kind("A.NCA"), Kind::Nca);
        assert_eq!(kind("c.tik"), Kind::Ticket);
        assert_eq!(kind("d.ncz"), Kind::Ncz);
        assert_eq!(kind("readme.txt"), Kind::Other);
    }

    #[test]
    fn last_name_without_nul() {
        let img = build(b"PFS0", 0x18, &[("one.nca", b"1"), ("last.nca", b"2")], false);
        let es = dump_files(&mut Cursor::new(&img)).unwrap();
        assert_eq!(es[1].name, "last.nca");
    }

    #[test]
    fn xci_secure_partition() {
        let secure = build(b"HFS0", 0x40, &[("x.nca", b"XX"), ("y.cnmt.nca", b"YYY")], true);
        // root HFS0 with partitions "update", "secure"; entry offsets are relative to the root's data start
        let upd = build(b"HFS0", 0x40, &[], true);
        let root_parts: [(&str, &[u8]); 2] = [("update", &upd), ("secure", &secure)];
        let root = build(b"HFS0", 0x40, &root_parts, true);
        let root_at = 0x200u64;
        for card in [0u64, 0x1000] {
            let mut img = vec![0u8; (card + root_at) as usize];
            img[(card + 0x100) as usize..(card + 0x104) as usize].copy_from_slice(b"HEAD");
            img[(card + 0x130) as usize..(card + 0x138) as usize].copy_from_slice(&root_at.to_le_bytes());
            img.extend_from_slice(&root);
            let es = dump_files(&mut Cursor::new(&img)).unwrap();
            assert_eq!(es.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["x.nca", "y.cnmt.nca"], "card at {card:#x}");
            assert_eq!(&img[es[1].offset as usize..(es[1].offset + es[1].size) as usize], b"YYY");
        }
    }

    #[test]
    fn not_a_dump_and_corrupt_headers() {
        assert!(dump_files(&mut Cursor::new(b"hello".to_vec())).unwrap().is_empty());
        assert!(dump_files(&mut Cursor::new(Vec::new())).unwrap().is_empty());
        let mut huge = b"PFS0".to_vec();
        huge.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        huge.extend_from_slice(&0u32.to_le_bytes());
        huge.extend_from_slice(&0u32.to_le_bytes());
        assert!(dump_files(&mut Cursor::new(huge)).unwrap().is_empty());
        let mut cut = build(b"PFS0", 0x18, &[("a.nca", b"A")], true);
        cut.truncate(20); // entry table cut short
        assert!(dump_files(&mut Cursor::new(cut)).unwrap().is_empty());
    }
}
