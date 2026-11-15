//! Checking a section's data against its hash tree: finds damaged blocks (bit rot, a bad copy) that the extraction itself would not notice.
//!
//! Type 2 (ExeFS): SHA-256 of every block of the data (the last one at its real size) must equal the entries of the layer-0 table, and the master hash must equal SHA-256 of that table.
//! Type 3 (IVFC, RomFS): six levels; the blocks of each level (block size 2^n, the last one zero-padded) are hashed into the level above, level 0 into the master hash.
//! Layout from the public documentation and LibHac's reading of it; checked here on synthetic trees only, so a mismatch on a real dump may be this code's error rather than the dump's.

use crate::section::{HashInfo, Section};
use nx_crypto::Sha256;
use std::io::{self, Read, Seek, SeekFrom};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// the hash levels above the data (the table, the master hash) are consistent
    pub upper_ok: bool,
    /// damaged blocks of the data level as (offset inside the data, length)
    pub bad: Vec<(u64, u64)>,
}

fn read_at<R: Read + Seek>(r: &mut R, off: u64, len: usize) -> io::Result<Vec<u8>> {
    r.seek(SeekFrom::Start(off))?;
    let mut v = vec![0u8; len];
    r.read_exact(&mut v)?;
    Ok(v)
}

/// Hashes the blocks of `size` bytes at `off` and compares them with `expect` (32 bytes per block). Returns the damaged blocks. `pad` zero-fills the last block.
fn blocks<R: Read + Seek>(r: &mut R, off: u64, size: u64, block: u64, expect: &[u8], pad: bool) -> io::Result<Vec<(u64, u64)>> {
    let mut bad = Vec::new();
    let n = size.div_ceil(block);
    if (expect.len() as u64) < n * 32 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "hash table is shorter than the data it covers"));
    }
    let mut buf = vec![0u8; block as usize];
    r.seek(SeekFrom::Start(off))?;
    for i in 0..n {
        let len = block.min(size - i * block) as usize;
        r.read_exact(&mut buf[..len])?;
        let h = if pad {
            buf[len..].fill(0);
            Sha256::digest(&buf)
        } else {
            Sha256::digest(&buf[..len])
        };
        if h[..] != expect[(i * 32) as usize..(i * 32 + 32) as usize] {
            bad.push((i * block, len as u64));
        }
    }
    Ok(bad)
}

/// Verifies `sec` through `r`, a reader over the whole decrypted section (for a patch, the merged one). A section with no hash tree gives `None`.
pub fn verify<R: Read + Seek>(r: &mut R, sec: &Section) -> io::Result<Option<Verdict>> {
    match sec.hash {
        HashInfo::None => Ok(None),
        HashInfo::Sha256 { block, master, table, data } => {
            if block == 0 || table.1 > (64 << 20) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "implausible SHA-256 hash layer"));
            }
            let t = read_at(r, table.0, table.1 as usize)?;
            let upper_ok = Sha256::digest(&t) == master;
            Ok(Some(Verdict { upper_ok, bad: blocks(r, data.0, data.1, block as u64, &t, false)? }))
        }
        HashInfo::Ivfc { levels, master, master_len } => {
            let mut upper_ok = true;
            if master_len == 0 || master_len > 32 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "implausible IVFC master hash size"));
            }
            // level 0 against the master hash
            let l0 = levels[0];
            if l0.2 == 0 || l0.2 > 30 || levels.iter().any(|l| l.2 == 0 || l.2 > 30) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "implausible IVFC block size"));
            }
            upper_ok &= blocks(r, l0.0, l0.1, 1 << l0.2, &master, true)?.is_empty();
            for i in 1..5 {
                let (up, lv) = (levels[i - 1], levels[i]);
                if up.1 > (256 << 20) {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "implausible IVFC level size"));
                }
                let table = read_at(r, up.0, up.1 as usize)?;
                upper_ok &= blocks(r, lv.0, lv.1, 1 << lv.2, &table, true)?.is_empty();
            }
            let (up, data) = (levels[4], levels[5]);
            let table = read_at(r, up.0, up.1 as usize)?;
            Ok(Some(Verdict { upper_ok, bad: blocks(r, data.0, data.1, 1 << data.2, &table, true)? }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sec(hash: HashInfo) -> Section {
        Section { index: 0, start: 0, end: 0, fs_type: 0, hash_type: 0, encryption: 1, counter: [0; 8], data_offset: 0, data_size: 0, patch: None, hash }
    }

    fn sha(d: &[u8]) -> [u8; 32] {
        Sha256::digest(d)
    }

    #[test]
    fn sha256_layers() {
        let block = 0x40usize;
        let data: Vec<u8> = (0..0x100 + 0x13).map(|i| (i * 7) as u8).collect(); // the last block is short
        let table: Vec<u8> = data.chunks(block).flat_map(|c| sha(c)).collect();
        let mut img = table.clone();
        img.resize(0x100, 0);
        let data_off = img.len() as u64;
        img.extend(&data);
        let info = HashInfo::Sha256 { block: block as u32, master: sha(&table), table: (0, table.len() as u64), data: (data_off, data.len() as u64) };
        let v = verify(&mut Cursor::new(&img), &sec(info)).unwrap().unwrap();
        assert_eq!(v, Verdict { upper_ok: true, bad: vec![] });
        let mut bad = img.clone();
        bad[data_off as usize + 0x45] ^= 1; // block 1
        bad[data_off as usize + 0x105] ^= 1; // the short last block
        let v = verify(&mut Cursor::new(&bad), &sec(info)).unwrap().unwrap();
        assert_eq!(v, Verdict { upper_ok: true, bad: vec![(0x40, 0x40), (0x100, 0x13)] });
        let mut bad = img.clone();
        bad[3] ^= 1; // the table itself
        let v = verify(&mut Cursor::new(&bad), &sec(info)).unwrap().unwrap();
        assert!(!v.upper_ok && !v.bad.is_empty());
    }

    #[test]
    fn ivfc_levels() {
        let log2 = 6u32; // 64-byte blocks for the test
        let b = 1usize << log2;
        let data: Vec<u8> = (0..b * 5 + 10).map(|i| (i * 11 + 1) as u8).collect();
        let pad = |mut v: Vec<u8>| {
            v.resize(v.len().div_ceil(b) * b, 0);
            v
        };
        let hashes = |d: &[u8]| -> Vec<u8> { pad(d.to_vec()).chunks(b).flat_map(|c| sha(c)).collect() };
        let l5 = pad(data.clone());
        let l4 = pad(hashes(&data));
        let l3 = pad(hashes(&l4));
        let l2 = pad(hashes(&l3));
        let l1 = pad(hashes(&l2));
        let l0 = pad(hashes(&l1));
        let master: [u8; 32] = sha(&l0);
        let mut img = Vec::new();
        let mut levels = [(0u64, 0u64, log2); 6];
        for (i, l) in [&l0, &l1, &l2, &l3, &l4, &l5].into_iter().enumerate() {
            levels[i] = (img.len() as u64, if i == 5 { data.len() as u64 } else { l.len() as u64 }, log2);
            img.extend(l.iter());
        }
        let info = HashInfo::Ivfc { levels, master, master_len: 32 };
        assert!(l0.len() <= b, "the test tree needs a one-block level 0");
        let v = verify(&mut Cursor::new(&img), &sec(info)).unwrap().unwrap();
        assert_eq!(v, Verdict { upper_ok: true, bad: vec![] });
        let mut bad = img.clone();
        bad[levels[5].0 as usize + 2 * b + 1] ^= 0xFF;
        let v = verify(&mut Cursor::new(&bad), &sec(info)).unwrap().unwrap();
        assert_eq!(v, Verdict { upper_ok: true, bad: vec![(2 * b as u64, b as u64)] });
        let mut bad = img.clone();
        bad[levels[3].0 as usize] ^= 1; // a middle level: its own block no longer matches the level above, and the data's table is wrong too
        assert!(!verify(&mut Cursor::new(&bad), &sec(info)).unwrap().unwrap().upper_ok);
    }

    #[test]
    fn no_tree_and_nonsense() {
        assert!(verify(&mut Cursor::new(vec![0u8; 10]), &sec(HashInfo::None)).unwrap().is_none());
        let bad = HashInfo::Sha256 { block: 0, master: [0; 32], table: (0, 0), data: (0, 0) };
        assert!(verify(&mut Cursor::new(vec![0u8; 10]), &sec(bad)).is_err());
        let short = HashInfo::Sha256 { block: 16, master: [0; 32], table: (0, 32), data: (32, 64) };
        assert!(verify(&mut Cursor::new(vec![0u8; 200]), &sec(short)).is_err()); // 4 blocks need 128 bytes of table
    }
}
