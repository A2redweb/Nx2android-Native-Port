//! Reads `stage|ctl hex|code hex` lines, prints `json|code hex` for each (for checks against shader_vk.py job_for).
use nx_shader::{job::*, split::Stage};
use std::io::BufRead;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn main() {
    for l in std::io::stdin().lock().lines() {
        let l = l.unwrap();
        let f: Vec<&str> = l.split('|').collect();
        let st = match f[0] { "vs" => Stage::Vs, "hs" => Stage::Hs, "ds" => Stage::Ds, "gs" => Stage::Gs, "ps" => Stage::Ps, _ => Stage::Cs };
        match job_for("k", st, &unhex(f[1]), &unhex(f[2]), None, &Options { subgroup: 128 }) {
            Ok(j) => println!("{}|{}", j.json.to_string(), j.code.iter().map(|b| format!("{b:02x}")).collect::<String>()),
            Err(e) => println!("ERR {e}"),
        }
    }
}
