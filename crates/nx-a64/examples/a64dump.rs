//! Reads 32-bit hex words (one per line) and prints `word used reads writes dest_sp known` in hex, for tools/a64_oracle.py.
use std::io::BufRead;
fn main() {
    for l in std::io::stdin().lock().lines() {
        let l = l.unwrap();
        let w = u32::from_str_radix(l.trim(), 16).unwrap();
        let i = nx_a64::decode(w);
        println!("{w:08x} {:08x} {:08x} {:08x} {} {}", i.used(), i.reads(), i.writes(), i.dest_sp as u8, i.known as u8);
    }
}
