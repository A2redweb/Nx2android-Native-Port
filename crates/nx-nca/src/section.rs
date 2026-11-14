//! The whole NCA header, the key that opens its sections, and a reader for a decrypted section.
//!
//! Layout (offsets in the decrypted header, from the public Switch documentation and hactool's `nca.h`; checked here only on synthetic images, not yet on a real dump):
//! 0x205 content type, 0x206 old key generation, 0x207 key area index (0 application, 1 ocean, 2 system), 0x208 content size, 0x210 program ID, 0x220 key generation,
//! 0x230 rights ID, 0x240 four section entries (start and end in 0x200-byte units), 0x300 four encrypted 16-byte keys, 0x400 four 0x200-byte section headers.
//! A section header: +3 encryption (1 none, 2 XTS, 3 CTR, 4 CTR with a patch's BKTR tables), +4 hash type, +0x140 the 8-byte section counter.
//! NCA3 encrypts the 0xC00 header as one XTS run, sector numbers 0 to 5; NCA2 encrypts each section header on its own as sector 0.

use nx_crypto::{Aes128, Xts};
use nx_keys::Keys;
use std::io::{self, Read, Seek, SeekFrom};

const UNIT: u64 = 0x200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub index: usize,
    /// byte offsets inside the NCA
    pub start: u64,
    pub end: u64,
    /// 0 RomFS, 1 PFS0
    pub fs_type: u8,
    pub hash_type: u8,
    /// 1 none, 2 XTS, 3 CTR, 4 CTR with BKTR tables (a patch)
    pub encryption: u8,
    /// the 8 bytes at +0x140 of the section header
    pub counter: [u8; 8],
    /// where the file system itself sits inside the section, past the hash tables: for a hash tree of type 2 (SHA-256, ExeFS) the second layer region, for type 3 (IVFC, RomFS)
    /// the last of six levels; with no hash tree the whole section. Offset from the start of the section.
    pub data_offset: u64,
    pub data_size: u64,
    /// a patch's BKTR tables (encryption type 4)
    pub patch: Option<PatchInfo>,
}

/// Where a patch section keeps its two bucket trees (offsets from the start of the section) and how many entries each holds (section header +0x100 and +0x120).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchInfo {
    pub reloc_offset: u64,
    pub reloc_size: u64,
    pub reloc_entries: u32,
    pub sub_offset: u64,
    pub sub_size: u64,
    pub sub_entries: u32,
}

impl Section {
    pub fn len(&self) -> u64 {
        self.end - self.start
    }
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullHeader {
    pub version: u8,
    pub content_type: u8,
    /// the larger of the two key generation fields
    pub key_generation: u8,
    pub key_area_index: u8,
    pub content_size: u64,
    pub title_id: u64,
    pub rights_id: [u8; 16],
    pub sections: Vec<Section>,
    /// still encrypted
    pub key_area: [[u8; 16]; 4],
}

impl FullHeader {
    /// A non-zero rights ID means the section key is a title key from a ticket, not from the key area.
    pub fn uses_rights_id(&self) -> bool {
        self.rights_id != [0; 16]
    }
}

fn rd<R: Read>(f: &mut R, n: usize) -> io::Result<Option<Vec<u8>>> {
    let mut v = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match f.read(&mut v[got..])? {
            0 => return Ok(None),
            k => got += k,
        }
    }
    Ok(Some(v))
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Reads and decrypts the 0xC00-byte header of the NCA at `at`. `None` for a short file, a bad key or a magic that is not NCA3 / NCA2.
pub fn read_full_header<R: Read + Seek>(f: &mut R, at: u64, header_key: &[u8]) -> io::Result<Option<FullHeader>> {
    let Some(xts) = Xts::new(header_key) else { return Ok(None) };
    f.seek(SeekFrom::Start(at))?;
    let Some(mut h) = rd(f, 0xC00)? else { return Ok(None) };
    for s in 0..2 {
        xts.decrypt(Xts::tweak_be(s as u128), &mut h[s * 0x200..(s + 1) * 0x200]);
    }
    let version = match &h[0x200..0x204] {
        b"NCA3" => 3,
        b"NCA2" => 2,
        _ => return Ok(None),
    };
    for s in 2..6 {
        // NCA3 continues the sector count; NCA2 starts each section header over at 0
        let tweak = if version == 3 { s as u128 } else { 0 };
        xts.decrypt(Xts::tweak_be(tweak), &mut h[s * 0x200..(s + 1) * 0x200]);
    }
    let mut sections = Vec::new();
    for i in 0..4 {
        let e = 0x240 + i * 0x10;
        let (start, end) = (u32le(&h, e) as u64 * UNIT, u32le(&h, e + 4) as u64 * UNIT);
        if end <= start {
            continue;
        }
        let fh = 0x400 + i * 0x200;
        let hash_type = h[fh + 3];
        let (data_offset, data_size) = match hash_type {
            2 if u32le(&h, fh + 0x2c) >= 2 => (u64le(&h, fh + 0x40), u64le(&h, fh + 0x48)), // hash data at +8: master hash 0x20, block size, layer count (+0x2c), regions from +0x30; the data is region 1
            3 => (u64le(&h, fh + 8 + 0x10 + 5 * 0x18), u64le(&h, fh + 8 + 0x10 + 5 * 0x18 + 8)), // IVFC level infos at +0x10 of the hash data, 0x18 each
            _ => (0, end - start),
        };
        let patch = (h[fh + 4] == 4 && &h[fh + 0x110..fh + 0x114] == b"BKTR" && &h[fh + 0x130..fh + 0x134] == b"BKTR").then(|| PatchInfo {
            reloc_offset: u64le(&h, fh + 0x100),
            reloc_size: u64le(&h, fh + 0x108),
            reloc_entries: u32le(&h, fh + 0x118),
            sub_offset: u64le(&h, fh + 0x120),
            sub_size: u64le(&h, fh + 0x128),
            sub_entries: u32le(&h, fh + 0x138),
        });
        sections.push(Section {
            patch,
            data_offset,
            data_size,
            index: i,
            start,
            end,
            fs_type: h[fh + 2],
            hash_type,
            encryption: h[fh + 4],
            counter: h[fh + 0x140..fh + 0x148].try_into().unwrap(),
        });
    }
    let mut key_area = [[0u8; 16]; 4];
    for (i, k) in key_area.iter_mut().enumerate() {
        k.copy_from_slice(&h[0x300 + i * 16..0x310 + i * 16]);
    }
    Ok(Some(FullHeader {
        version,
        content_type: h[0x205],
        key_generation: h[0x206].max(h[0x220]),
        key_area_index: h[0x207],
        content_size: u64le(&h, 0x208),
        title_id: u64le(&h, 0x210),
        rights_id: h[0x230..0x240].try_into().unwrap(),
        sections,
        key_area,
    }))
}

fn key16(keys: &Keys, name: &str) -> Option<[u8; 16]> {
    keys.get(name)?.try_into().ok()
}

fn ecb_dec(key: &[u8; 16], data: [u8; 16]) -> [u8; 16] {
    let mut b = data;
    Aes128::new(key).decrypt_block(&mut b);
    b
}

/// `master_key_XX` for a key generation (generation 0 and 1 both use master key 0).
pub fn master_key(keys: &Keys, key_generation: u8) -> Option<[u8; 16]> {
    key16(keys, &format!("master_key_{:02x}", key_generation.saturating_sub(1)))
}

/// The key area key for `(key_area_index, key_generation)`: `key_area_key_<kind>_XX` when the file has it, else derived from the master key and the three source keys.
pub fn key_area_key(keys: &Keys, key_area_index: u8, key_generation: u8) -> Option<[u8; 16]> {
    let kind = ["application", "ocean", "system"].get(key_area_index as usize)?;
    let gen = key_generation.saturating_sub(1);
    if let Some(k) = key16(keys, &format!("key_area_key_{kind}_{gen:02x}")) {
        return Some(k);
    }
    let master = master_key(keys, key_generation)?;
    let kek = ecb_dec(&master, key16(keys, "aes_kek_generation_source")?);
    let src_kek = ecb_dec(&kek, key16(keys, &format!("key_area_key_{kind}_source"))?);
    Some(ecb_dec(&src_kek, key16(keys, "aes_key_generation_source")?))
}

/// The 16-byte key of an NCA's CTR sections: the third key of the key area, decrypted with the key area key. For an NCA with a rights ID pass the ticket's title key as `title_key`.
pub fn section_key(h: &FullHeader, keys: &Keys, title_key: Option<[u8; 16]>) -> Option<[u8; 16]> {
    if h.uses_rights_id() {
        return title_key;
    }
    Some(ecb_dec(&key_area_key(keys, h.key_area_index, h.key_generation)?, h.key_area[2]))
}

/// A decrypted view of one section of an NCA (encryption none or CTR). A patch's BKTR sections (type 4) and XTS sections are not readable yet.
pub struct SectionReader<R> {
    inner: R,
    at: u64,
    sec: Section,
    aes: Option<Aes128>,
    pos: u64,
}

impl<R: Read + Seek> SectionReader<R> {
    pub fn new(inner: R, nca_at: u64, sec: Section, key: &[u8; 16]) -> io::Result<Self> {
        let aes = match sec.encryption {
            1 => None,
            3 => Some(Aes128::new(key)),
            e => return Err(io::Error::new(io::ErrorKind::Unsupported, format!("section encryption type {e} is not supported yet"))),
        };
        Ok(SectionReader { inner, at: nca_at, sec, aes, pos: 0 })
    }
    pub fn len(&self) -> u64 {
        self.sec.len()
    }
    pub fn is_empty(&self) -> bool {
        self.sec.is_empty()
    }
}

impl<R: Read + Seek> Read for SectionReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.sec.len().saturating_sub(self.pos);
        let want = (buf.len() as u64).min(left) as usize;
        if want == 0 {
            return Ok(0);
        }
        let abs = self.at + self.sec.start + self.pos; // offset in the file; the counter counts 16-byte blocks from the NCA's start
        let nca_off = self.sec.start + self.pos;
        let skew = (nca_off & 15) as usize;
        let mut tmp = vec![0u8; skew + want];
        self.inner.seek(SeekFrom::Start(abs - skew as u64))?;
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
        if let Some(aes) = &self.aes {
            let mut ctr = [0u8; 16];
            for (i, b) in self.sec.counter.iter().enumerate() {
                ctr[7 - i] = *b; // the stored counter is reversed
            }
            ctr[8..].copy_from_slice(&((nca_off - skew as u64) >> 4).to_be_bytes());
            aes.ctr_apply(ctr, &mut tmp);
        }
        let n = got - skew;
        buf[..n].copy_from_slice(&tmp[skew..]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for SectionReader<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.sec.len() as i128 + d as i128,
        };
        if p < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before the start of the section"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const HK: [u8; 32] = [0x3c; 32];
    const SK: [u8; 16] = [0x77; 16];
    const CTR8: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

    fn ctr_for(off: u64) -> [u8; 16] {
        let mut c = [0u8; 16];
        for i in 0..8 {
            c[7 - i] = CTR8[i];
        }
        c[8..].copy_from_slice(&(off >> 4).to_be_bytes());
        c
    }

    /// An NCA3: header, then one CTR section of `plain` at 0xC00, with key SK in key area slot 2 encrypted by `kak`.
    fn nca(plain: &[u8], kak: &[u8; 16], enc: u8, rights: [u8; 16]) -> Vec<u8> {
        let mut h = vec![0u8; 0xC00];
        h[0x200..0x204].copy_from_slice(b"NCA3");
        h[0x205] = 0;
        h[0x206] = 2;
        h[0x207] = 0;
        h[0x208..0x210].copy_from_slice(&(0xC00 + plain.len() as u64).to_le_bytes());
        h[0x210..0x218].copy_from_slice(&0x0100_AAAA_0000_0000u64.to_le_bytes());
        h[0x220] = 2;
        h[0x230..0x240].copy_from_slice(&rights);
        h[0x240..0x244].copy_from_slice(&(0xC00u32 / 0x200).to_le_bytes());
        h[0x244..0x248].copy_from_slice(&(((0xC00 + plain.len()) as u32).div_ceil(0x200)).to_le_bytes());
        let mut k = SK;
        Aes128::new(kak).encrypt_block(&mut k);
        h[0x320..0x330].copy_from_slice(&k);
        h[0x402] = 0;
        h[0x403] = 3;
        h[0x404] = enc;
        h[0x540..0x548].copy_from_slice(&CTR8); // the reader reverses it into the counter's high half
        let xts = Xts::new(&HK).unwrap();
        for s in 0..6 {
            xts.encrypt(Xts::tweak_be(s as u128), &mut h[s * 0x200..(s + 1) * 0x200]);
        }
        let mut body = plain.to_vec();
        body.resize(plain.len().div_ceil(0x200) * 0x200, 0);
        if enc == 3 {
            Aes128::new(&SK).ctr_apply(ctr_for(0xC00), &mut body);
        }
        h.extend_from_slice(&body);
        h
    }

    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + 3) as u8).collect()
    }

    // kak derived as the code derives it, from made-up keys
    fn keys_and_kak() -> (Keys, [u8; 16]) {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let (m, ks, kg, src) = ([0x10u8; 16], [0x20u8; 16], [0x30u8; 16], [0x40u8; 16]);
        let k = Keys::parse(&format!(
            "master_key_01 = {}\naes_kek_generation_source = {}\naes_key_generation_source = {}\nkey_area_key_application_source = {}\n",
            hex(&m), hex(&ks), hex(&kg), hex(&src)
        ));
        // forward direction: key area key -> source key -> kek -> master
        let kek = ecb_dec(&m, ks);
        let src_kek = ecb_dec(&kek, src);
        (k, ecb_dec(&src_kek, kg))
    }

    #[test]
    fn header_fields_and_sections() {
        let (_, kak) = keys_and_kak();
        let img = nca(&pattern(0x1000), &kak, 3, [0; 16]);
        let h = read_full_header(&mut Cursor::new(&img), 0, &HK).unwrap().unwrap();
        assert_eq!((h.version, h.content_type, h.key_generation, h.key_area_index), (3, 0, 2, 0));
        assert_eq!(h.title_id, 0x0100_AAAA_0000_0000);
        assert!(!h.uses_rights_id());
        assert_eq!(h.sections.len(), 1);
        let s = &h.sections[0];
        assert_eq!((s.start, s.end, s.encryption, s.counter), (0xC00, 0x1C00, 3, CTR8));
    }

    #[test]
    fn key_derivation_and_section_key() {
        let (keys, kak) = keys_and_kak();
        assert_eq!(key_area_key(&keys, 0, 2), Some(kak)); // generation 2 uses master_key_01
        assert_eq!(key_area_key(&keys, 1, 2), None);
        let img = nca(&pattern(0x200), &kak, 3, [0; 16]);
        let h = read_full_header(&mut Cursor::new(&img), 0, &HK).unwrap().unwrap();
        assert_eq!(section_key(&h, &keys, None), Some(SK));
        let h2 = FullHeader { rights_id: [9; 16], ..h };
        assert_eq!(section_key(&h2, &keys, None), None);
        assert_eq!(section_key(&h2, &keys, Some([5; 16])), Some([5; 16]));
    }

    #[test]
    fn ctr_section_reads_at_any_offset() {
        let (_, kak) = keys_and_kak();
        let plain = pattern(0x1000);
        let img = nca(&plain, &kak, 3, [0; 16]);
        let h = read_full_header(&mut Cursor::new(&img), 0, &HK).unwrap().unwrap();
        let mut r = SectionReader::new(Cursor::new(&img), 0, h.sections[0].clone(), &SK).unwrap();
        let mut all = Vec::new();
        r.read_to_end(&mut all).unwrap();
        assert_eq!(&all[..0x1000], &plain[..]);
        for (off, n) in [(0u64, 1usize), (1, 15), (15, 17), (17, 300), (0xFF1, 15), (0x333, 0x400)] {
            r.seek(SeekFrom::Start(off)).unwrap();
            let mut b = vec![0u8; n];
            r.read_exact(&mut b).unwrap();
            assert_eq!(b, &plain[off as usize..off as usize + n], "offset {off}");
        }
        assert_eq!(r.seek(SeekFrom::End(0)).unwrap(), 0x1000);
    }

    #[test]
    fn plain_section_and_nonzero_nca_offset() {
        let (_, kak) = keys_and_kak();
        let plain = pattern(0x400);
        let mut img = vec![0xEE; 0x300]; // the NCA sits at 0x300 in a container
        img.extend(nca(&plain, &kak, 1, [0; 16]));
        let h = read_full_header(&mut Cursor::new(&img), 0x300, &HK).unwrap().unwrap();
        let mut r = SectionReader::new(Cursor::new(&img), 0x300, h.sections[0].clone(), &SK).unwrap();
        let mut b = vec![0u8; 0x400];
        r.read_exact(&mut b).unwrap();
        assert_eq!(b, plain);
    }

    #[test]
    fn unsupported_encryption_is_an_error() {
        let (_, kak) = keys_and_kak();
        let img = nca(&pattern(0x200), &kak, 4, [0; 16]);
        let h = read_full_header(&mut Cursor::new(&img), 0, &HK).unwrap().unwrap();
        let e = SectionReader::new(Cursor::new(&img), 0, h.sections[0].clone(), &SK).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::Unsupported);
        assert!(read_full_header(&mut Cursor::new(&img), 0, &[1; 32]).unwrap().is_none());
    }
}
