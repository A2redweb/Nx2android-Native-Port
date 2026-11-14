//! Opening a dump (NSP, XCI) and writing out a game: the Rust counterpart of upstream's `nx_extract` for a base game.
//!
//! What it does: lists the dump's files, reads the common tickets, reads every NCA header, picks the Program NCA (lowest title ID, as upstream's `Pick`) and the Control NCA,
//! opens their sections with the right key (key area or title key) and writes `ExeFS/`, `RomFS/` and `control.nacp`.
//! Not done yet: an update layered over the base (BKTR), compressed dumps (NSZ/XCZ), hash checks of the section data, add-on content.

use nx_container::{dump_files, Entry, Kind};
use nx_fs::SubReader;
use nx_keys::Keys;
use nx_nca::section::{read_full_header, section_key, FullHeader, Section, SectionReader};
use nx_nca::ticket::Ticket;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// a message for the person, already worded
    Msg(String),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Msg(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

fn msg<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(m.into()))
}

#[derive(Debug, Clone)]
pub struct Nca {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub header: FullHeader,
}

pub struct Package<R> {
    pub r: R,
    pub ncas: Vec<Nca>,
    pub tickets: Vec<Ticket>,
}

impl<R: Read + Seek> Package<R> {
    /// Reads the container's file list, the common tickets and every NCA header. An NCA whose header does not open with this `header_key` is an error that names it.
    pub fn open(mut r: R, keys: &Keys, input: &str) -> Result<Self, Error> {
        let Some(hk) = keys.header_key() else { return msg("prod.keys has no header_key") };
        let files: Vec<Entry> = dump_files(&mut r)?;
        let (mut ncas, mut tickets) = (Vec::new(), Vec::new());
        for e in &files {
            match nx_container::kind(&e.name) {
                Kind::Ticket if e.name.to_ascii_lowercase().ends_with(".tik") => {
                    r.seek(SeekFrom::Start(e.offset))?;
                    let mut b = vec![0u8; e.size.min(0x400) as usize];
                    r.read_exact(&mut b)?;
                    match Ticket::parse(&b) {
                        Some(t) if t.common => tickets.push(t),
                        Some(_) => eprintln!("[RED] {} is not a common ticket; supply the title key separately", e.name),
                        None => eprintln!("[RED] {} is not a readable ticket", e.name),
                    }
                }
                Kind::Nca | Kind::Cnmt => match read_full_header(&mut r, e.offset, hk)? {
                    Some(header) => ncas.push(Nca { name: e.name.clone(), offset: e.offset, size: e.size, header }),
                    None => return msg(format!("cannot open NCA entry '{}' in '{input}': the header does not decrypt with header_key (wrong keys, or not an NCA)", e.name)),
                },
                Kind::Ncz => return msg(format!("'{}' in '{input}' is compressed (NCZ); decompression is not implemented yet", e.name)),
                _ => {}
            }
        }
        Ok(Package { r, ncas, tickets })
    }

    /// The NCA of a content type; with no title ID the one with the lowest title ID (program index 0, the game), else the one of that title ID.
    pub fn pick(&self, content_type: u8, title: Option<u64>) -> Option<&Nca> {
        self.ncas
            .iter()
            .filter(|n| n.header.content_type == content_type && title.map_or(true, |t| n.header.title_id == t))
            .min_by_key(|n| n.header.title_id)
    }

    fn key_for(&self, nca: &Nca, keys: &Keys) -> Result<[u8; 16], Error> {
        let h = &nca.header;
        let tk = if h.uses_rights_id() { self.tickets.iter().find(|t| t.rights_id == h.rights_id).and_then(|t| t.title_key(keys, h.key_generation)) } else { None };
        match section_key(h, keys, tk) {
            Some(k) => Ok(k),
            None if h.uses_rights_id() => msg(format!("{}: no common ticket for its rights ID (or the master key / titlekek_source is missing in prod.keys)", nca.name)),
            None => msg(format!("{}: prod.keys lacks the key area key for key generation {} (master_key_{:02x} and the three source keys)", nca.name, h.key_generation, h.key_generation.saturating_sub(1))),
        }
    }

    /// The file system data of a section (past the hash tables) as a reader.
    pub fn data<'a>(&'a mut self, nca: &Nca, sec: &Section, keys: &Keys) -> Result<SubReader<SectionReader<&'a mut R>>, Error> {
        let key = self.key_for(nca, keys)?;
        let sr = SectionReader::new(&mut self.r, nca.offset, sec.clone(), &key)?;
        Ok(SubReader::new(sr, sec.data_offset, sec.data_size))
    }
}

/// `fs_type` of a section header: RomFS.
pub const FS_ROMFS: u8 = 0;
/// `fs_type` of a section header: PFS0 (ExeFS, logo).
pub const FS_PFS0: u8 = 1;

pub const CONTENT_PROGRAM: u8 = 0;
pub const CONTENT_CONTROL: u8 = 2;

#[derive(Debug, Default)]
pub struct Report {
    pub title_id: u64,
    pub exefs_files: usize,
    pub romfs_files: usize,
    pub romfs_bytes: u64,
    pub nacp: bool,
}

/// Writes `ExeFS/`, `RomFS/` and `control.nacp` of the base game in `r` under `out`.
pub fn extract_base<R: Read + Seek>(r: R, keys: &Keys, input: &str, out: &Path) -> Result<Report, Error> {
    let mut pkg = Package::open(r, keys, input)?;
    let Some(prog) = pkg.pick(CONTENT_PROGRAM, None).cloned() else { return msg("no Program NCA in the dump") };
    let mut rep = Report { title_id: prog.header.title_id, ..Report::default() };
    fs::create_dir_all(out)?;
    let exefs = prog.header.sections.iter().find(|s| s.fs_type == FS_PFS0).cloned();
    let romfs = prog.header.sections.iter().find(|s| s.fs_type == FS_ROMFS).cloned();
    match exefs {
        Some(s) => rep.exefs_files = nx_fs::extract_pfs0(&mut pkg.data(&prog, &s, keys)?, &out.join("ExeFS"))?,
        None => return msg("the Program NCA has no ExeFS section"),
    }
    fs::create_dir_all(out.join("RomFS"))?; // a program may embed all its assets in its code modules: the layout stays
    if let Some(s) = romfs {
        let (n, b) = nx_fs::extract_romfs(&mut pkg.data(&prog, &s, keys)?, &out.join("RomFS"))?;
        rep.romfs_files = n;
        rep.romfs_bytes = b;
    }
    if let Some(ctrl) = pkg.pick(CONTENT_CONTROL, Some(prog.header.title_id)).cloned() {
        if let Some(s) = ctrl.header.sections.iter().find(|s| s.fs_type == FS_ROMFS).cloned() {
            let mut d = pkg.data(&ctrl, &s, keys)?;
            let rom = nx_fs::RomFs::parse(&mut d)?;
            if let Some(f) = rom.find("control.nacp") {
                nx_fs::copy_out(&mut d, f.offset, f.size, &out.join("control.nacp"))?;
                rep.nacp = true;
            }
            const LANGS: [&str; 16] = ["AmericanEnglish", "BritishEnglish", "Japanese", "French", "German", "LatinAmericanSpanish", "Spanish", "Italian", "Dutch", "CanadianFrench", "Portuguese", "Russian", "Korean", "TraditionalChinese", "SimplifiedChinese", "BrazilianPortuguese"];
            if let Some(f) = LANGS.iter().find_map(|l| rom.find(&format!("icon_{l}.dat"))) {
                nx_fs::copy_out(&mut d, f.offset, f.size, &out.join("icon.jpg"))?;
            }
        }
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nx_crypto::{Aes128, Xts};
    use std::io::Cursor;

    const HK: [u8; 32] = [0x3c; 32];
    const MASTER: [u8; 16] = [0x10; 16];
    const KS: [u8; 16] = [0x20; 16];
    const KG: [u8; 16] = [0x30; 16];
    const SRC: [u8; 16] = [0x40; 16];
    const TKS: [u8; 16] = [0x50; 16];
    const TITLE: u64 = 0x0100_AAAA_0000_0000;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn dec(key: &[u8; 16], d: [u8; 16]) -> [u8; 16] {
        let mut b = d;
        Aes128::new(key).decrypt_block(&mut b);
        b
    }

    fn keys() -> Keys {
        Keys::parse(&format!(
            "header_key = {}\nmaster_key_01 = {}\naes_kek_generation_source = {}\naes_key_generation_source = {}\nkey_area_key_application_source = {}\ntitlekek_source = {}\n",
            hex(&HK), hex(&MASTER), hex(&KS), hex(&KG), hex(&SRC), hex(&TKS)
        ))
    }

    fn kak() -> [u8; 16] {
        dec(&dec(&dec(&MASTER, KS), SRC), KG)
    }

    /// An NCA3 with CTR sections `(fs_type, bytes)`, each at the next 0x200 boundary after 0xC00. `sec_key` encrypts the data; with no rights ID it also goes into the key area.
    fn nca(content_type: u8, title: u64, secs: &[(u8, Vec<u8>)], sec_key: [u8; 16], rights: [u8; 16]) -> Vec<u8> {
        let mut h = vec![0u8; 0xC00];
        h[0x200..0x204].copy_from_slice(b"NCA3");
        h[0x205] = content_type;
        h[0x206] = 2;
        h[0x220] = 2;
        h[0x210..0x218].copy_from_slice(&title.to_le_bytes());
        h[0x230..0x240].copy_from_slice(&rights);
        let mut k = sec_key;
        Aes128::new(&kak()).encrypt_block(&mut k);
        h[0x320..0x330].copy_from_slice(&k);
        let mut body = Vec::new();
        for (i, (fs, data)) in secs.iter().enumerate() {
            let start = 0xC00 + body.len();
            let mut d = data.clone();
            d.resize(d.len().div_ceil(0x200) * 0x200, 0);
            let end = start + d.len();
            h[0x240 + i * 0x10..0x244 + i * 0x10].copy_from_slice(&((start / 0x200) as u32).to_le_bytes());
            h[0x244 + i * 0x10..0x248 + i * 0x10].copy_from_slice(&((end / 0x200) as u32).to_le_bytes());
            let fh = 0x400 + i * 0x200;
            h[fh + 2] = *fs;
            h[fh + 3] = 1; // no hash tree
            h[fh + 4] = 3;
            let ctr8 = [i as u8 + 1; 8];
            h[fh + 0x140..fh + 0x148].copy_from_slice(&ctr8);
            let mut ctr = [0u8; 16];
            for (j, b) in ctr8.iter().enumerate() {
                ctr[7 - j] = *b;
            }
            ctr[8..].copy_from_slice(&((start as u64) >> 4).to_be_bytes());
            Aes128::new(&sec_key).ctr_apply(ctr, &mut d);
            body.extend_from_slice(&d);
        }
        let xts = Xts::new(&HK).unwrap();
        for s in 0..6 {
            xts.encrypt(Xts::tweak_be(s as u128), &mut h[s * 0x200..(s + 1) * 0x200]);
        }
        h.extend_from_slice(&body);
        h
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
        for x in [files.len() as u32, strs.len() as u32, 0] {
            out.extend_from_slice(&x.to_le_bytes());
        }
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

    /// A RomFS with the given root files (no directories).
    fn romfs(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let ent = |parent: u32, sibling: u32, child_dir: u32, child_file: u32, name: &str| {
            let mut v = Vec::new();
            for x in [parent, sibling, child_dir, child_file, 0xFFFF_FFFF, name.len() as u32] {
                v.extend_from_slice(&x.to_le_bytes());
            }
            v.extend_from_slice(name.as_bytes());
            while v.len() % 4 != 0 {
                v.push(0);
            }
            v
        };
        let mut ftab = Vec::new();
        let mut data = Vec::new();
        let mut offs = Vec::new();
        for (_, d) in files {
            offs.push(data.len() as u64);
            data.extend_from_slice(d);
        }
        let mut sizes = Vec::new();
        for (n, _) in files {
            sizes.push((0x20 + n.len()).div_ceil(4) * 4);
        }
        let mut at = 0u32;
        for (i, (n, d)) in files.iter().enumerate() {
            at += sizes[i] as u32;
            let sib = if i + 1 == files.len() { 0xFFFF_FFFF } else { at };
            ftab.extend_from_slice(&0u32.to_le_bytes());
            ftab.extend_from_slice(&sib.to_le_bytes());
            ftab.extend_from_slice(&offs[i].to_le_bytes());
            ftab.extend_from_slice(&(d.len() as u64).to_le_bytes());
            ftab.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            ftab.extend_from_slice(&(n.len() as u32).to_le_bytes());
            ftab.extend_from_slice(n.as_bytes());
            while ftab.len() % 4 != 0 {
                ftab.push(0);
            }
        }
        let dtab = ent(0, 0xFFFF_FFFF, 0xFFFF_FFFF, 0, "");
        let (dir_at, file_at) = (0x60u64, 0x60 + dtab.len() as u64);
        let data_at = (file_at + ftab.len() as u64 + 0xF) & !0xF;
        let mut img = vec![0u8; data_at as usize];
        img[0..8].copy_from_slice(&0x50u64.to_le_bytes());
        img[0x18..0x20].copy_from_slice(&dir_at.to_le_bytes());
        img[0x20..0x28].copy_from_slice(&(dtab.len() as u64).to_le_bytes());
        img[0x38..0x40].copy_from_slice(&file_at.to_le_bytes());
        img[0x40..0x48].copy_from_slice(&(ftab.len() as u64).to_le_bytes());
        img[0x48..0x50].copy_from_slice(&data_at.to_le_bytes());
        img[dir_at as usize..dir_at as usize + dtab.len()].copy_from_slice(&dtab);
        img[file_at as usize..file_at as usize + ftab.len()].copy_from_slice(&ftab);
        img.extend_from_slice(&data);
        img
    }

    fn tik(rights: [u8; 16], plain_key: [u8; 16]) -> Vec<u8> {
        let kek = dec(&MASTER, TKS);
        let mut stored = plain_key;
        Aes128::new(&kek).encrypt_block(&mut stored);
        let mut v = vec![0u8; 0x2c0];
        v[..4].copy_from_slice(&0x10004u32.to_le_bytes());
        v[0x180..0x190].copy_from_slice(&stored);
        v[0x2a0..0x2b0].copy_from_slice(&rights);
        v
    }

    #[test]
    fn extracts_a_base_game_from_a_synthetic_nsp() {
        let main = vec![0x4E, 0x53, 0x4F, 0x30, 1, 2, 3];
        let exefs = pfs0(&[("main", main.clone()), ("main.npdm", vec![9; 40])]);
        let rom = romfs(&[("a.txt", b"hello".to_vec()), ("data.bin", (0..700u32).map(|i| i as u8).collect())]);
        let program = nca(CONTENT_PROGRAM, TITLE, &[(FS_PFS0, exefs), (FS_ROMFS, rom.clone())], [0x77; 16], [0; 16]);
        // the control NCA is protected by a rights ID and a common ticket instead of the key area
        let rights = {
            let mut r = [0u8; 16];
            r[..8].copy_from_slice(&TITLE.to_be_bytes());
            r[15] = 2;
            r
        };
        let ctrl_rom = romfs(&[("control.nacp", vec![7; 0x4000]), ("icon_AmericanEnglish.dat", vec![0xFF, 0xD8, 1, 2])]);
        let control = nca(CONTROL_FOR_TEST, TITLE, &[(FS_ROMFS, ctrl_rom)], [0x99; 16], rights);
        let nsp = pfs0(&[("p.nca", program), ("c.nca", control), ("t.tik", tik(rights, [0x99; 16])), ("t.cert", vec![1])]);

        let out = std::env::temp_dir().join(format!("nx-extract-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&out);
        let rep = extract_base(Cursor::new(nsp), &keys(), "test.nsp", &out).unwrap();
        assert_eq!((rep.title_id, rep.exefs_files, rep.romfs_files, rep.romfs_bytes, rep.nacp), (TITLE, 2, 2, 705, true));
        assert_eq!(fs::read(out.join("ExeFS/main")).unwrap(), main);
        assert_eq!(fs::read(out.join("RomFS/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(out.join("RomFS/data.bin")).unwrap().len(), 700);
        assert_eq!(fs::read(out.join("control.nacp")).unwrap(), vec![7; 0x4000]);
        assert_eq!(fs::read(out.join("icon.jpg")).unwrap(), vec![0xFF, 0xD8, 1, 2]);
        fs::remove_dir_all(&out).unwrap();
    }

    const CONTROL_FOR_TEST: u8 = CONTENT_CONTROL;

    #[test]
    fn reports_missing_keys_and_wrong_header_key() {
        let program = nca(CONTENT_PROGRAM, TITLE, &[(FS_PFS0, pfs0(&[("main", vec![1])]))], [0x77; 16], [0; 16]);
        let nsp = pfs0(&[("p.nca", program)]);
        let out = std::env::temp_dir().join(format!("nx-extract-test2-{}", std::process::id()));
        let only_header = Keys::parse(&format!("header_key = {}\n", hex(&HK)));
        let e = extract_base(Cursor::new(&nsp), &only_header, "x.nsp", &out).unwrap_err().to_string();
        assert!(e.contains("key area key") && e.contains("p.nca"), "{e}");
        let wrong = Keys::parse(&format!("header_key = {}\n", hex(&[1u8; 32])));
        let e = extract_base(Cursor::new(&nsp), &wrong, "x.nsp", &out).unwrap_err().to_string();
        assert!(e.contains("cannot open NCA entry 'p.nca' in 'x.nsp'"), "{e}");
        assert!(extract_base(Cursor::new(&nsp), &Keys::parse(""), "x.nsp", &out).is_err());
        let _ = fs::remove_dir_all(&out);
    }
}
