//! Decodes files made by libzstd (tools/make_zstd_vectors.py) and compares them with the same deterministic inputs rebuilt here.

use nx_zstd::{decompress, decompress_vec};
use std::io::Read;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

const WORDS: [&[u8]; 16] = [b"the", b"quick", b"brown", b"fox", b"jumps", b"over", b"lazy", b"dog", b"switch", b"android", b"vulkan", b"shader", b"nso", b"romfs", b"\n", b"  "];

fn input(kind: &str) -> Vec<u8> {
    match kind {
        "text" => {
            let mut g = Lcg(1);
            let mut out = Vec::new();
            while out.len() < 200000 {
                out.extend_from_slice(WORDS[(g.next() % 16) as usize]);
                out.push(b' ');
            }
            out.truncate(200000);
            out
        }
        "runs" => {
            let mut g = Lcg(2);
            let mut out = Vec::new();
            while out.len() < 150000 {
                let b = (g.next() & 0xFF) as u8;
                let n = 1 + (g.next() % 3000) as usize;
                out.resize(out.len() + n, b);
            }
            out.truncate(150000);
            out
        }
        "noise" => {
            let mut g = Lcg(3);
            (0..100000).map(|_| ((g.next() >> 8) & 0xFF) as u8).collect()
        }
        "records" => {
            let mut g = Lcg(4);
            let mut out = Vec::new();
            for i in 0..300000u32 / 16 {
                out.extend_from_slice(&i.to_le_bytes());
                out.extend_from_slice(&i.wrapping_mul(i).to_le_bytes());
                out.extend_from_slice(&((g.next() & 0x3FF) as u16).to_le_bytes());
                out.extend_from_slice(&[(i % 251) as u8; 6]);
            }
            out
        }
        _ => unreachable!(),
    }
}

fn file(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn libzstd_output_decodes_exactly() {
    let mut n = 0;
    for entry in std::fs::read_dir(format!("{}/tests/data", env!("CARGO_MANIFEST_DIR"))).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let stem = name.strip_suffix(".zst").unwrap();
        let kind = stem.split('_').next().unwrap();
        let got = decompress_vec(&file(stem)).unwrap_or_else(|e| panic!("{stem}: {e}"));
        let want = input(kind);
        assert_eq!(got.len(), want.len(), "{stem}");
        assert!(got == want, "{stem} differs at {:?}", got.iter().zip(&want).position(|(a, b)| a != b));
        n += 1;
    }
    assert!(n >= 12, "only {n} vectors found");
}

#[test]
fn concatenated_frames_and_tiny_reads() {
    let mut two = file("text_3");
    two.extend(file("runs_1"));
    let mut want = input("text");
    want.extend(input("runs"));
    assert!(decompress_vec(&two).unwrap() == want);
    // a reader that hands out one byte at a time
    struct Slow<'a>(&'a [u8]);
    impl Read for Slow<'_> {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() || b.is_empty() {
                return Ok(0);
            }
            b[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }
    let mut out = Vec::new();
    decompress(&mut Slow(&file("records_9")), &mut out).unwrap();
    assert!(out == input("records"));
}

#[test]
fn corrupt_input_is_an_error_not_a_panic() {
    let good = file("text_19");
    // every truncation of the header region, and flipped bytes through the file
    for cut in (0..40).chain([good.len() / 2, good.len() - 1]) {
        let _ = decompress_vec(&good[..cut]);
    }
    let mut bad = 0;
    for i in (0..good.len()).step_by(97) {
        let mut d = good.clone();
        d[i] ^= 0x5A;
        if decompress_vec(&d).is_err() {
            bad += 1;
        }
    }
    assert!(bad > 0);
    assert!(decompress_vec(b"not zstd at all").is_err());
    assert!(decompress_vec(&[0x28, 0xB5, 0x2F, 0xFD, 0x20, 0x08, 0x41, 0x00, 0x00, 1, 2]).is_err()); // a raw block of 8 bytes with 2 present
    assert!(decompress_vec(&[0x28, 0xB5, 0x2F, 0xFD, 0x20, 0x00, 0x01, 0x00, 0x00]).unwrap().is_empty()); // an empty frame is valid
    assert_eq!(decompress_vec(&[]).unwrap(), Vec::<u8>::new());
}

/// `cargo test --release -p nx-zstd -- --ignored --nocapture`: rough speed of the decoder on this machine.
#[test]
#[ignore]
fn speed() {
    for name in ["text_3", "records_3", "text_19"] {
        let z = file(name);
        let t = std::time::Instant::now();
        let mut total = 0usize;
        for _ in 0..200 {
            total += decompress_vec(&z).unwrap().len();
        }
        println!("{name}: {:.0} MB/s", total as f64 / 1048576.0 / t.elapsed().as_secs_f64());
    }
}
