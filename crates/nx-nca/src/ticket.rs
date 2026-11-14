//! Tickets (`.tik`) and the title keys they carry.
//!
//! A ticket starts with a signature (type u32, signature, padding), then the body: issuer 0x40, a 0x100-byte title key block, then (from the body's +0x141) the key type
//! (0 common, 1 personalised) and (+0x160) the rights ID. Upstream's extractor reads the same fields at fixed offsets for the usual RSA-2048 signature (0x180, 0x281, 0x2A0).
//! A common ticket's first 16 bytes are the title key encrypted with the title KEK of the NCA's key generation. A personalised ticket is encrypted to a console and is refused.

use crate::section::master_key;
use nx_crypto::Aes128;
use nx_keys::Keys;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub rights_id: [u8; 16],
    /// still encrypted
    pub title_key: [u8; 16],
    pub common: bool,
}

/// Signature block size (signature plus padding) for a ticket signature type.
fn sig_block(kind: u32) -> Option<usize> {
    Some(match kind {
        0x10000 | 0x10003 => 0x200 + 0x3c,
        0x10001 | 0x10004 => 0x100 + 0x3c,
        0x10002 | 0x10005 => 0x3c + 0x40,
        _ => return None,
    })
}

impl Ticket {
    pub fn parse(b: &[u8]) -> Option<Ticket> {
        let kind = u32::from_le_bytes(b.get(..4)?.try_into().ok()?);
        let body = 4 + sig_block(kind)?;
        let key = b.get(body + 0x40..body + 0x50)?;
        Some(Ticket {
            title_key: key.try_into().ok()?,
            common: *b.get(body + 0x141)? == 0,
            rights_id: b.get(body + 0x160..body + 0x170)?.try_into().ok()?,
        })
    }

    /// The usable title key for an NCA of `key_generation`; `None` when a key is missing or the ticket is personalised.
    pub fn title_key(&self, keys: &Keys, key_generation: u8) -> Option<[u8; 16]> {
        if !self.common {
            return None;
        }
        let master = master_key(keys, key_generation)?;
        let mut kek = <[u8; 16]>::try_from(keys.get("titlekek_source")?).ok()?;
        Aes128::new(&master).decrypt_block(&mut kek);
        let mut k = self.title_key;
        Aes128::new(&kek).decrypt_block(&mut k);
        Some(k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tik(kind: u32, common: bool, rights: [u8; 16], key: [u8; 16]) -> Vec<u8> {
        let body = 4 + sig_block(kind).unwrap();
        let mut v = vec![0u8; body + 0x170];
        v[..4].copy_from_slice(&kind.to_le_bytes());
        v[body + 0x40..body + 0x50].copy_from_slice(&key);
        v[body + 0x141] = if common { 0 } else { 1 };
        v[body + 0x160..body + 0x170].copy_from_slice(&rights);
        v
    }

    #[test]
    fn fixed_offsets_match_upstream_for_rsa2048() {
        let t = tik(0x10004, true, [7; 16], [9; 16]);
        assert_eq!(t[0x180], 9); // upstream reads the key at 0x180, the key type at 0x281 and the rights ID at 0x2A0
        assert_eq!(t[0x2a0], 7);
        assert_eq!(t[0x281], 0);
        let p = Ticket::parse(&t).unwrap();
        assert_eq!((p.rights_id, p.title_key, p.common), ([7; 16], [9; 16], true));
    }

    #[test]
    fn other_signature_types_and_bad_input() {
        for k in [0x10000, 0x10001, 0x10002, 0x10003, 0x10005] {
            assert_eq!(Ticket::parse(&tik(k, true, [1; 16], [2; 16])).unwrap().rights_id, [1; 16], "{k:#x}");
        }
        assert!(Ticket::parse(&tik(0x10004, false, [1; 16], [2; 16])).unwrap().title_key(&Keys::parse(""), 2).is_none());
        assert!(Ticket::parse(&[0; 3]).is_none());
        assert!(Ticket::parse(&tik(0x10004, true, [1; 16], [2; 16])[..0x200]).is_none());
        let mut bad = tik(0x10004, true, [1; 16], [2; 16]);
        bad[0] = 0x99;
        assert!(Ticket::parse(&bad).is_none());
    }

    #[test]
    fn title_key_decryption_round_trip() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let (master, src, plain) = ([0x11u8; 16], [0x22u8; 16], [0x33u8; 16]);
        let keys = Keys::parse(&format!("master_key_01 = {}\ntitlekek_source = {}\n", hex(&master), hex(&src)));
        // encrypt the way a console would: kek = AES-dec(master, src); stored = AES-enc(kek, plain)
        let mut kek = src;
        Aes128::new(&master).decrypt_block(&mut kek);
        let mut stored = plain;
        Aes128::new(&kek).encrypt_block(&mut stored);
        let t = Ticket { rights_id: [0; 16], title_key: stored, common: true };
        assert_eq!(t.title_key(&keys, 2), Some(plain));
        assert_eq!(t.title_key(&keys, 5), None);
    }
}
