//! NCA headers and the title ID of a dump: a port of `meta_title` and `title_of` in upstream's `build_ipa_from_nsp.py`.
//!
//! An NCA header is AES-XTS with `header_key` (from `prod.keys`) in sectors of 0x200 bytes whose tweak is the sector number big-endian (Nintendo's, not the standard's little-endian).
//! The second sector (file offset 0x200) holds the magic `NCA3` or `NCA2`, the content type at +5 (1 = meta) and the title ID at +0x10. Only a meta NCA's title ID is the dump's own:
//! an update's program and control NCAs carry the base game's title ID. An NSZ keeps its NCA's header as it is.

pub mod section;

use nx_container::{dump_files, Entry};
use nx_crypto::Xts;
use nx_keys::Keys;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// What the second header sector says about an NCA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// 3 for `NCA3`, 2 for `NCA2`
    pub version: u8,
    /// 0 program, 1 meta, 2 control, 3 manual, 4 data, 5 public data
    pub content_type: u8,
    pub title_id: u64,
}

impl Header {
    pub fn is_meta(&self) -> bool {
        self.content_type == 1
    }
}

/// Reads and decrypts the header sector at `at + 0x200`. `None` when the file ends early, the key is not 32 bytes, or the magic is not NCA3 / NCA2 (wrong key or not an NCA).
pub fn read_header<R: Read + Seek>(f: &mut R, at: u64, header_key: &[u8]) -> io::Result<Option<Header>> {
    let Some(xts) = Xts::new(header_key) else { return Ok(None) };
    f.seek(SeekFrom::Start(at + 0x200))?;
    let mut sector = vec![0u8; 0x200];
    let mut got = 0;
    while got < sector.len() {
        match f.read(&mut sector[got..])? {
            0 => return Ok(None),
            k => got += k,
        }
    }
    xts.decrypt(Xts::tweak_be(1), &mut sector);
    let version = match &sector[..4] {
        b"NCA3" => 3,
        b"NCA2" => 2,
        _ => return Ok(None),
    };
    Ok(Some(Header { version, content_type: sector[5], title_id: u64::from_le_bytes(sector[0x10..0x18].try_into().unwrap()) }))
}

/// The title ID of the content meta NCA at `at`, `None` for any other NCA.
pub fn meta_title<R: Read + Seek>(f: &mut R, at: u64, header_key: &[u8]) -> io::Result<Option<u64>> {
    Ok(read_header(f, at, header_key)?.filter(Header::is_meta).map(|h| h.title_id))
}

fn is_ticket_name(n: &str) -> bool {
    // [0-9A-Fa-f]{32}\.tik$  (matched from the start of the name)
    n.len() == 36 && n[..32].bytes().all(|b| b.is_ascii_hexdigit()) && n[32..].eq_ignore_ascii_case(".tik")
}

/// `[<16 hex digits>]` in a file name.
pub fn tag_in_name(name: &str) -> Option<u64> {
    let b = name.as_bytes();
    (0..b.len().saturating_sub(17)).find_map(|i| {
        if b[i] == b'[' && b[i + 17] == b']' && b[i + 1..i + 17].iter().all(u8::is_ascii_hexdigit) {
            u64::from_str_radix(&name[i + 1..i + 17], 16).ok()
        } else {
            None
        }
    })
}

/// Why a dump has the title ID it has, and any other titles found in the same file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title {
    pub id: u64,
    /// further titles in the same file (not taken: supply base game, update and DLC as separate files)
    pub others: Vec<u64>,
}

/// The title ID of a dump, in upstream's order: the ticket names (the first 16 digits of the rights ID are the title ID), then the content meta NCAs' headers (for a dump with no
/// ticket, and for a cartridge dump, which may carry its update's ticket only), then a `[title ID]` tag in the file name. A file name's tag can be wrong, so it comes last.
/// `keys` is optional: without `header_key` the NCA headers are not read.
pub fn title_of<R: Read + Seek>(f: &mut R, path: &Path, keys: Option<&Keys>) -> io::Result<Option<Title>> {
    let files: Vec<Entry> = dump_files(f)?;
    let mut tids: Vec<u64> = files.iter().filter(|e| is_ticket_name(&e.name)).filter_map(|e| u64::from_str_radix(&e.name[..16], 16).ok()).collect();
    let card = path.extension().map(|x| matches!(x.to_string_lossy().to_ascii_lowercase().as_str(), "xci" | "xcz")).unwrap_or(false);
    if let Some(hk) = keys.and_then(Keys::header_key) {
        if tids.is_empty() || card {
            for e in &files {
                let n = e.name.to_ascii_lowercase();
                if n.ends_with(".nca") || n.ends_with(".ncz") {
                    if let Some(t) = meta_title(f, e.offset, hk)? {
                        tids.push(t);
                    }
                }
            }
        }
    }
    if !tids.is_empty() {
        tids.sort_unstable();
        tids.dedup(); // several rights IDs can name the same title
        return Ok(Some(Title { id: tids[0], others: tids[1..].to_vec() })); // the base game ...000 before its update ...800 and DLC
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(tag_in_name(&name).map(|id| Title { id, others: Vec::new() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const HK: [u8; 32] = [0x5a; 32];

    /// An NCA whose second sector holds a header with the given content type and title ID, encrypted the way Nintendo's headers are.
    fn nca(content_type: u8, title: u64, magic: &[u8; 4]) -> Vec<u8> {
        let mut img = vec![0u8; 0x400];
        let mut s = vec![0u8; 0x200];
        s[..4].copy_from_slice(magic);
        s[5] = content_type;
        s[0x10..0x18].copy_from_slice(&title.to_le_bytes());
        Xts::new(&HK).unwrap().encrypt(Xts::tweak_be(1), &mut s);
        img[0x200..0x400].copy_from_slice(&s);
        img
    }

    fn pfs0(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut strs = Vec::new();
        let mut offs = Vec::new();
        for (n, _) in files {
            offs.push(strs.len() as u32);
            strs.extend_from_slice(n.as_bytes());
            strs.push(0);
        }
        let mut out = b"PFS0".to_vec();
        out.extend_from_slice(&(files.len() as u32).to_le_bytes());
        out.extend_from_slice(&(strs.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut off = 0u64;
        for (i, (_, d)) in files.iter().enumerate() {
            out.extend_from_slice(&off.to_le_bytes());
            out.extend_from_slice(&(d.len() as u64).to_le_bytes());
            out.extend_from_slice(&offs[i].to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            off += d.len() as u64;
        }
        out.extend_from_slice(&strs);
        for (_, d) in files {
            out.extend_from_slice(d);
        }
        out
    }

    fn keys() -> Keys {
        let hex: String = HK.iter().map(|b| format!("{b:02x}")).collect();
        Keys::parse(&format!("header_key = {hex}\n"))
    }

    #[test]
    fn header_fields() {
        let img = nca(1, 0x0100_0000_0001_0800, b"NCA3");
        let h = read_header(&mut Cursor::new(&img), 0, &HK).unwrap().unwrap();
        assert_eq!(h, Header { version: 3, content_type: 1, title_id: 0x0100_0000_0001_0800 });
        assert!(h.is_meta());
        let h2 = read_header(&mut Cursor::new(nca(0, 5, b"NCA2")), 0, &HK).unwrap().unwrap();
        assert_eq!((h2.version, h2.is_meta()), (2, false));
    }

    #[test]
    fn wrong_key_or_short_file_gives_none() {
        let img = nca(1, 9, b"NCA3");
        assert!(read_header(&mut Cursor::new(&img), 0, &[0x11; 32]).unwrap().is_none());
        assert!(read_header(&mut Cursor::new(&img[..0x300]), 0, &HK).unwrap().is_none());
        assert!(read_header(&mut Cursor::new(&img), 0, &HK[..16]).unwrap().is_none());
    }

    #[test]
    fn meta_title_ignores_other_content() {
        assert_eq!(meta_title(&mut Cursor::new(nca(1, 7, b"NCA3")), 0, &HK).unwrap(), Some(7));
        assert_eq!(meta_title(&mut Cursor::new(nca(0, 7, b"NCA3")), 0, &HK).unwrap(), None);
    }

    #[test]
    fn title_from_ticket_name() {
        let img = pfs0(&[("0100000000010000000000000000000b.tik", vec![1]), ("x.nca", vec![0; 4])]);
        let t = title_of(&mut Cursor::new(img), Path::new("game.nsp"), None).unwrap().unwrap();
        assert_eq!(t, Title { id: 0x0100_0000_0001_0000, others: vec![] });
    }

    #[test]
    fn title_from_meta_nca_without_a_ticket() {
        let img = pfs0(&[("a.cnmt.nca", nca(1, 0x0100_ABCD_0000_0800, b"NCA3")), ("b.nca", nca(0, 0x0100_ABCD_0000_0000, b"NCA3"))]);
        let t = title_of(&mut Cursor::new(&img), Path::new("game.nsp"), Some(&keys())).unwrap().unwrap();
        assert_eq!(t.id, 0x0100_ABCD_0000_0800); // the program NCA's id is not used
        // without header_key the NCAs are not read and the file name decides
        assert!(title_of(&mut Cursor::new(&img), Path::new("game.nsp"), None).unwrap().is_none());
        let t = title_of(&mut Cursor::new(&img), Path::new("Game [0100ABCD00000000][v0].nsp"), None).unwrap().unwrap();
        assert_eq!(t.id, 0x0100_ABCD_0000_0000);
    }

    #[test]
    fn several_titles_take_the_lowest_and_report_the_rest() {
        let img = pfs0(&[
            ("0100000000010800aaaaaaaaaaaaaaaa.tik", vec![]),
            ("0100000000010000bbbbbbbbbbbbbbbb.tik", vec![]),
            ("0100000000010000cccccccccccccccc.tik", vec![]),
        ]);
        let t = title_of(&mut Cursor::new(img), Path::new("x.nsp"), None).unwrap().unwrap();
        assert_eq!(t, Title { id: 0x0100_0000_0001_0000, others: vec![0x0100_0000_0001_0800] });
    }

    #[test]
    fn name_tags() {
        assert_eq!(tag_in_name("A [0100000000010000] [v393216].nsp"), Some(0x0100_0000_0001_0000));
        assert_eq!(tag_in_name("A [0100000000010].nsp"), None);
        assert_eq!(tag_in_name("A [01000000000100zz].nsp"), None);
        assert_eq!(tag_in_name("[0100000000010000]"), Some(0x0100_0000_0001_0000));
        assert!(is_ticket_name("0100000000010000000000000000000B.TIK") && !is_ticket_name("short.tik"));
    }
}
