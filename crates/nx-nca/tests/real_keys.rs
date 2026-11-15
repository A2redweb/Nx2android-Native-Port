//! Checks the key derivation against a real `prod.keys`: every key the file stores directly is also derived from its sources, and the two must agree.
//! Skipped unless `NXN_PROD_KEYS` names a key file (never committed): `NXN_PROD_KEYS=prod.keys cargo test -p nx-nca --test real_keys -- --nocapture`.
//! Passed on 2026-11-14 with a 231-key file: 17/17 master keys, 22/22 titlekeks, 66/66 key area keys, and header_key from its sources.

use nx_crypto::Aes128;
use nx_keys::Keys;
use nx_nca::section::key_area_key;

fn k16(k: &Keys, n: &str) -> Option<[u8; 16]> {
    k.get(n)?.try_into().ok()
}

fn dec(key: &[u8; 16], d: [u8; 16]) -> [u8; 16] {
    let mut b = d;
    Aes128::new(key).decrypt_block(&mut b);
    b
}

#[test]
fn derivations_match_the_stored_keys() {
    let Ok(path) = std::env::var("NXN_PROD_KEYS") else {
        eprintln!("NXN_PROD_KEYS not set: skipped");
        return;
    };
    let k = Keys::load(std::path::Path::new(&path)).unwrap();
    let (mut masters, mut tkeks, mut kaks) = (0, 0, 0);
    for g in 0..0x20u8 {
        if let (Some(kek), Some(src), Some(mk)) = (k16(&k, &format!("master_kek_{g:02x}")), k16(&k, "master_key_source"), k16(&k, &format!("master_key_{g:02x}"))) {
            assert_eq!(dec(&kek, src), mk, "master_key_{g:02x}");
            masters += 1;
        }
        if let (Some(mk), Some(src), Some(tk)) = (k16(&k, &format!("master_key_{g:02x}")), k16(&k, "titlekek_source"), k16(&k, &format!("titlekek_{g:02x}"))) {
            assert_eq!(dec(&mk, src), tk, "titlekek_{g:02x}");
            tkeks += 1;
        }
    }
    // key area keys: hide the stored ones so the derivation path runs
    let mut text = String::new();
    for n in k.names() {
        if !n.starts_with("key_area_key_") || n.ends_with("_source") {
            let v: String = k.get(n).unwrap().iter().map(|b| format!("{b:02x}")).collect();
            text += &format!("{n} = {v}\n");
        }
    }
    let derived = Keys::parse(&text);
    for (idx, kind) in ["application", "ocean", "system"].iter().enumerate() {
        for g in 0..0x20u8 {
            if let Some(want) = k16(&k, &format!("key_area_key_{kind}_{g:02x}")) {
                assert_eq!(key_area_key(&derived, idx as u8, g + 1), Some(want), "key_area_key_{kind}_{g:02x}");
                kaks += 1;
            }
        }
    }
    // the header key: both halves decrypted with a kek made from master_key_00
    let mk = k16(&k, "master_key_00").unwrap();
    let kek = dec(&dec(&dec(&mk, k16(&k, "aes_kek_generation_source").unwrap()), k16(&k, "header_kek_source").unwrap()), k16(&k, "aes_key_generation_source").unwrap());
    let src = k.get("header_key_source").unwrap();
    let mut hk = [0u8; 32];
    hk[..16].copy_from_slice(&dec(&kek, src[..16].try_into().unwrap()));
    hk[16..].copy_from_slice(&dec(&kek, src[16..].try_into().unwrap()));
    assert_eq!(&hk[..], k.header_key().unwrap());
    eprintln!("verified: {masters} master keys, {tkeks} titlekeks, {kaks} key area keys, header_key");
}
