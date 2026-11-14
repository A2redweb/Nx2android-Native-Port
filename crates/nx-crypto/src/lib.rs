//! AES-128 for Switch dumps: ECB, CTR and XTS (with Nintendo's big-endian sector tweak). No dependencies.
//!
//! This is a portable, table-based implementation, checked against the FIPS-197, NIST SP 800-38A and IEEE 1619 test vectors (see the tests).
//! It is an interim: it does not use the ARMv8 AES instructions, so it is slower than an audited crate with hardware support. Bulk work (a whole game's RomFS) should move
//! to such a crate once the build has crates access; the interface here is small enough to swap.

const fn xtime(a: u8) -> u8 {
    (a << 1) ^ if a & 0x80 != 0 { 0x1b } else { 0 }
}

const fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    p
}

/// x^254 is the multiplicative inverse in GF(2^8) (0 maps to 0).
const fn inverse(x: u8) -> u8 {
    let x2 = gmul(x, x);
    let x4 = gmul(x2, x2);
    let x8 = gmul(x4, x4);
    let x16 = gmul(x8, x8);
    let x32 = gmul(x16, x16);
    let x64 = gmul(x32, x32);
    let x128 = gmul(x64, x64);
    // 254 = 128 + 64 + 32 + 16 + 8 + 4 + 2
    gmul(gmul(gmul(gmul(gmul(gmul(x128, x64), x32), x16), x8), x4), x2)
}

const fn build_sbox() -> [u8; 256] {
    let mut s = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let b = inverse(i as u8);
        s[i] = b ^ b.rotate_left(1) ^ b.rotate_left(2) ^ b.rotate_left(3) ^ b.rotate_left(4) ^ 0x63;
        i += 1;
    }
    s
}

const fn build_inv_sbox(s: &[u8; 256]) -> [u8; 256] {
    let mut r = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        r[s[i] as usize] = i as u8;
        i += 1;
    }
    r
}

const fn build_mul(k: u8) -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = gmul(i as u8, k);
        i += 1;
    }
    t
}

const SBOX: [u8; 256] = build_sbox();
const INV_SBOX: [u8; 256] = build_inv_sbox(&SBOX);
const M2: [u8; 256] = build_mul(2);
const M3: [u8; 256] = build_mul(3);
const M9: [u8; 256] = build_mul(9);
const M11: [u8; 256] = build_mul(11);
const M13: [u8; 256] = build_mul(13);
const M14: [u8; 256] = build_mul(14);

/// An AES-128 key, expanded.
#[derive(Clone)]
pub struct Aes128 {
    rk: [[u8; 16]; 11],
}

impl Aes128 {
    pub fn new(key: &[u8; 16]) -> Self {
        let mut w = [[0u8; 4]; 44];
        for i in 0..4 {
            w[i].copy_from_slice(&key[4 * i..4 * i + 4]);
        }
        let mut rcon = 1u8;
        for i in 4..44 {
            let mut t = w[i - 1];
            if i % 4 == 0 {
                t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
                rcon = xtime(rcon);
            }
            for j in 0..4 {
                w[i][j] = w[i - 4][j] ^ t[j];
            }
        }
        let mut rk = [[0u8; 16]; 11];
        for (r, k) in rk.iter_mut().enumerate() {
            for c in 0..4 {
                k[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }
        Aes128 { rk }
    }

    /// From a slice; `None` unless it is 16 bytes.
    pub fn from_slice(key: &[u8]) -> Option<Self> {
        Some(Self::new(key.try_into().ok()?))
    }

    fn add_round_key(s: &mut [u8; 16], k: &[u8; 16]) {
        for i in 0..16 {
            s[i] ^= k[i];
        }
    }

    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        Self::add_round_key(block, &self.rk[0]);
        for r in 1..=10 {
            for b in block.iter_mut() {
                *b = SBOX[*b as usize];
            }
            // ShiftRows: the state is column-major, byte index = 4*col + row
            let s = *block;
            for c in 0..4 {
                for row in 0..4 {
                    block[4 * c + row] = s[4 * ((c + row) % 4) + row];
                }
            }
            if r != 10 {
                for c in 0..4 {
                    let a = [block[4 * c], block[4 * c + 1], block[4 * c + 2], block[4 * c + 3]];
                    block[4 * c] = M2[a[0] as usize] ^ M3[a[1] as usize] ^ a[2] ^ a[3];
                    block[4 * c + 1] = a[0] ^ M2[a[1] as usize] ^ M3[a[2] as usize] ^ a[3];
                    block[4 * c + 2] = a[0] ^ a[1] ^ M2[a[2] as usize] ^ M3[a[3] as usize];
                    block[4 * c + 3] = M3[a[0] as usize] ^ a[1] ^ a[2] ^ M2[a[3] as usize];
                }
            }
            Self::add_round_key(block, &self.rk[r]);
        }
    }

    pub fn decrypt_block(&self, block: &mut [u8; 16]) {
        Self::add_round_key(block, &self.rk[10]);
        for r in (0..10).rev() {
            // InvShiftRows
            let s = *block;
            for c in 0..4 {
                for row in 0..4 {
                    block[4 * ((c + row) % 4) + row] = s[4 * c + row];
                }
            }
            for b in block.iter_mut() {
                *b = INV_SBOX[*b as usize];
            }
            Self::add_round_key(block, &self.rk[r]);
            if r != 0 {
                for c in 0..4 {
                    let a = [block[4 * c], block[4 * c + 1], block[4 * c + 2], block[4 * c + 3]];
                    block[4 * c] = M14[a[0] as usize] ^ M11[a[1] as usize] ^ M13[a[2] as usize] ^ M9[a[3] as usize];
                    block[4 * c + 1] = M9[a[0] as usize] ^ M14[a[1] as usize] ^ M11[a[2] as usize] ^ M13[a[3] as usize];
                    block[4 * c + 2] = M13[a[0] as usize] ^ M9[a[1] as usize] ^ M14[a[2] as usize] ^ M11[a[3] as usize];
                    block[4 * c + 3] = M11[a[0] as usize] ^ M13[a[1] as usize] ^ M9[a[2] as usize] ^ M14[a[3] as usize];
                }
            }
        }
    }

    /// ECB over whole blocks, in place. `data.len()` must be a multiple of 16.
    pub fn ecb_decrypt(&self, data: &mut [u8]) {
        assert!(data.len() % 16 == 0, "ECB needs whole 16-byte blocks");
        for c in data.chunks_exact_mut(16) {
            self.decrypt_block(c.try_into().unwrap());
        }
    }

    pub fn ecb_encrypt(&self, data: &mut [u8]) {
        assert!(data.len() % 16 == 0, "ECB needs whole 16-byte blocks");
        for c in data.chunks_exact_mut(16) {
            self.encrypt_block(c.try_into().unwrap());
        }
    }

    /// CTR: `counter` is the first block's counter, incremented as a 128-bit big-endian number per block. Encryption and decryption are the same.
    /// A final partial block is handled.
    pub fn ctr_apply(&self, counter: [u8; 16], data: &mut [u8]) {
        let mut ctr = u128::from_be_bytes(counter);
        for chunk in data.chunks_mut(16) {
            let mut ks = ctr.to_be_bytes();
            self.encrypt_block(&mut ks);
            for (d, k) in chunk.iter_mut().zip(ks) {
                *d ^= k;
            }
            ctr = ctr.wrapping_add(1);
        }
    }
}

/// XTS, which needs two keys. `tweak_in` is the 16 bytes that are encrypted with the tweak key to start a sector: the standard (IEEE 1619) uses the sector number
/// little-endian, Nintendo's NCA code uses it big-endian ([`Xts::tweak_be`], [`Xts::tweak_le`]).
#[derive(Clone)]
pub struct Xts {
    data: Aes128,
    tweak: Aes128,
}

impl Xts {
    /// `key` is the 32-byte pair: data key first, tweak key second.
    pub fn new(key: &[u8]) -> Option<Self> {
        if key.len() != 32 {
            return None;
        }
        Some(Xts { data: Aes128::from_slice(&key[..16])?, tweak: Aes128::from_slice(&key[16..])? })
    }

    pub fn tweak_be(sector: u128) -> [u8; 16] {
        sector.to_be_bytes()
    }
    pub fn tweak_le(sector: u128) -> [u8; 16] {
        sector.to_le_bytes()
    }

    fn mul_alpha(t: &mut [u8; 16]) {
        let carry = t[15] >> 7;
        for i in (1..16).rev() {
            t[i] = (t[i] << 1) | (t[i - 1] >> 7);
        }
        t[0] = (t[0] << 1) ^ if carry != 0 { 0x87 } else { 0 };
    }

    fn run(&self, tweak_in: [u8; 16], data: &mut [u8], decrypt: bool) {
        assert!(data.len() % 16 == 0 && !data.is_empty(), "XTS here needs whole blocks (no ciphertext stealing)");
        let mut t = tweak_in;
        self.tweak.encrypt_block(&mut t);
        for c in data.chunks_exact_mut(16) {
            let mut b: [u8; 16] = c.try_into().unwrap();
            for i in 0..16 {
                b[i] ^= t[i];
            }
            if decrypt {
                self.data.decrypt_block(&mut b);
            } else {
                self.data.encrypt_block(&mut b);
            }
            for i in 0..16 {
                c[i] = b[i] ^ t[i];
            }
            Self::mul_alpha(&mut t);
        }
    }

    pub fn decrypt(&self, tweak_in: [u8; 16], data: &mut [u8]) {
        self.run(tweak_in, data, true)
    }
    pub fn encrypt(&self, tweak_in: [u8; 16], data: &mut [u8]) {
        self.run(tweak_in, data, false)
    }
}

pub fn from_hex(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    s.chunks(2).map(|p| (char::from(p[0]).to_digit(16).unwrap() * 16 + char::from(p[1]).to_digit(16).unwrap()) as u8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b16(s: &str) -> [u8; 16] {
        from_hex(s).try_into().unwrap()
    }

    #[test]
    fn sbox_spot_values() {
        // FIPS-197 figure 7
        assert_eq!((SBOX[0x00], SBOX[0x01], SBOX[0x53], SBOX[0xff]), (0x63, 0x7c, 0xed, 0x16));
        assert_eq!((INV_SBOX[0x63], INV_SBOX[0x16]), (0x00, 0xff));
    }

    #[test]
    fn fips197_c1() {
        let aes = Aes128::new(&b16("000102030405060708090a0b0c0d0e0f"));
        let mut b = b16("00112233445566778899aabbccddeeff");
        aes.encrypt_block(&mut b);
        assert_eq!(b, b16("69c4e0d86a7b0430d8cdb78070b4c55a"));
        aes.decrypt_block(&mut b);
        assert_eq!(b, b16("00112233445566778899aabbccddeeff"));
    }

    #[test]
    fn sp800_38a_ecb() {
        let aes = Aes128::new(&b16("2b7e151628aed2a6abf7158809cf4f3c"));
        let mut d = from_hex("6bc1bee22e409f96e93d7e117393172a ae2d8a571e03ac9c9eb76fac45af8e51");
        aes.ecb_encrypt(&mut d);
        assert_eq!(d, from_hex("3ad77bb40d7a3660a89ecaf32466ef97 f5d3d58503b9699de785895a96fdbaaf"));
        aes.ecb_decrypt(&mut d);
        assert_eq!(d, from_hex("6bc1bee22e409f96e93d7e117393172a ae2d8a571e03ac9c9eb76fac45af8e51"));
    }

    #[test]
    fn sp800_38a_ctr() {
        let aes = Aes128::new(&b16("2b7e151628aed2a6abf7158809cf4f3c"));
        let mut d = from_hex("6bc1bee22e409f96e93d7e117393172a ae2d8a571e03ac9c9eb76fac45af8e51");
        aes.ctr_apply(b16("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"), &mut d);
        assert_eq!(d, from_hex("874d6191b620e3261bef6864990db6ce 9806f66b7970fdff8617187bb9fffdff"));
        aes.ctr_apply(b16("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"), &mut d); // CTR is its own inverse
        assert_eq!(d, from_hex("6bc1bee22e409f96e93d7e117393172a ae2d8a571e03ac9c9eb76fac45af8e51"));
        // a partial last block, and a counter that carries across bytes
        let mut p = vec![0u8; 20];
        aes.ctr_apply(b16("000000000000000000000000000000ff"), &mut p);
        let mut q = vec![0u8; 32];
        aes.ctr_apply(b16("000000000000000000000000000000ff"), &mut q);
        assert_eq!(p[..], q[..20]);
    }

    #[test]
    fn ieee1619_xts_vectors() {
        // vector 1: zero keys, data unit 0
        let x = Xts::new(&[0u8; 32]).unwrap();
        let mut d = vec![0u8; 32];
        x.encrypt(Xts::tweak_le(0), &mut d);
        assert_eq!(d, from_hex("917cf69ebd68b2ec9b9fe9a3eadda692 cd43d2f59598ed858c02c2652fbf922e"[..].replace(' ', "").as_str()));
        x.decrypt(Xts::tweak_le(0), &mut d);
        assert_eq!(d, vec![0u8; 32]);
        // vector 2: keys 11.. and 22.., data unit 0x3333333333
        let mut key = vec![0x11u8; 16];
        key.extend(vec![0x22u8; 16]);
        let x = Xts::new(&key).unwrap();
        let mut d = vec![0x44u8; 32];
        x.encrypt(Xts::tweak_le(0x3333333333), &mut d);
        assert_eq!(d, from_hex("c454185e6a16936e39334038acef838b fb186fff7480adc4289382ecd6d394f0"));
    }

    #[test]
    fn nintendo_tweak_differs_from_standard() {
        let x = Xts::new(&[7u8; 32]).unwrap();
        let (mut a, mut b) = (vec![1u8; 32], vec![1u8; 32]);
        x.encrypt(Xts::tweak_be(1), &mut a);
        x.encrypt(Xts::tweak_le(1), &mut b);
        assert_ne!(a, b);
        x.decrypt(Xts::tweak_be(1), &mut a);
        assert_eq!(a, vec![1u8; 32]);
    }

    #[test]
    fn rejects_bad_key_length() {
        assert!(Xts::new(&[0u8; 16]).is_none());
        assert!(Aes128::from_slice(&[0u8; 15]).is_none());
    }
}
