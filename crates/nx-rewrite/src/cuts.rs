//! The registered cuts of upstream's patch files as data (`tools/gen_cuts.py` writes `cuts.tsv`): build ID, offset, original bytes, new bytes, reason.

use crate::Cut;
use std::collections::HashMap;

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// {build ID: its cuts}. A malformed line is an error: a half-read table would silently drop a game's fix.
pub fn parse(text: &str) -> Result<HashMap<String, Vec<Cut>>, String> {
    let mut out: HashMap<String, Vec<Cut>> = HashMap::new();
    for (n, l) in text.lines().enumerate() {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.splitn(5, '\t').collect();
        let bad = || format!("cuts.tsv line {}: malformed", n + 1);
        if f.len() < 4 {
            return Err(bad());
        }
        let off = i64::from_str_radix(f[1].trim_start_matches("0x"), 16).map_err(|_| bad())?;
        let (orig, new) = (unhex(f[2]).ok_or_else(bad)?, unhex(f[3]).ok_or_else(bad)?);
        out.entry(f[0].to_string()).or_default().push(Cut { off, orig, new, why: f.get(4).unwrap_or(&"").to_string() });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses() {
        let m = super::parse("ab\t0x10\t0102\t0304\twhy it\nab\t0x20\t00\t01\t\n# c\n").unwrap();
        assert_eq!(m["ab"].len(), 2);
        assert_eq!(m["ab"][0].off, 0x10);
        assert_eq!(m["ab"][0].new, vec![3, 4]);
        assert!(super::parse("ab\t0x10\t01\n").is_err());
        assert!(super::parse("ab\tzz\t01\t02\tx\n").is_err());
    }
}
