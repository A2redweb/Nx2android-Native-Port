//! `prod.keys`: the console keys the player dumped from their own Switch. Lines are `name = hex`, `;` or `#` start a comment.
//! The key values are never printed: `Debug` and `Display` show names only, and nothing here logs a value.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Default)]
pub struct Keys {
    map: BTreeMap<String, Vec<u8>>,
    /// names whose value is not valid hex (kept so the caller can say what is wrong, without the value)
    pub rejected: Vec<String>,
}

fn hex(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() || s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|p| Some((char::from(p[0]).to_digit(16)? * 16 + char::from(p[1]).to_digit(16)?) as u8))
        .collect()
}

impl Keys {
    pub fn parse(text: &str) -> Keys {
        let mut k = Keys::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else { continue };
            let name = name.trim().to_ascii_lowercase();
            match hex(value.trim()) {
                Some(v) => {
                    k.map.insert(name, v);
                }
                None => k.rejected.push(name),
            }
        }
        k
    }

    pub fn load(path: &Path) -> io::Result<Keys> {
        Ok(Keys::parse(&fs::read_to_string(path)?))
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.map.get(name).map(Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    /// The NCA headers' AES-XTS key pair (32 bytes).
    pub fn header_key(&self) -> Option<&[u8]> {
        self.get("header_key").filter(|k| k.len() == 32)
    }

    /// `master_key_00`, `master_key_01`, ...: how many master keys the file holds, counted from 00 without a gap.
    pub fn master_keys(&self) -> usize {
        (0..=0x20).take_while(|i| self.map.contains_key(&format!("master_key_{i:02x}"))).count()
    }
}

impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keys").field("names", &self.map.keys().collect::<Vec<_>>()).field("rejected", &self.rejected).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_and_hex() {
        let k = Keys::parse("; comment\n# another\n\nHeader_Key = 00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\nmaster_key_00=AB\nmaster_key_01 = cd\nbad = xyz\nodd = abc\nnoequals\n");
        assert_eq!(k.header_key().unwrap().len(), 32);
        assert_eq!(k.get("master_key_01"), Some(&[0xcdu8][..]));
        assert_eq!(k.master_keys(), 2);
        assert_eq!(k.rejected, ["bad", "odd"]);
        assert_eq!(k.len(), 3);
    }

    #[test]
    fn header_key_needs_32_bytes() {
        assert!(Keys::parse("header_key = 0011").header_key().is_none());
    }

    #[test]
    fn debug_never_shows_values() {
        let k = Keys::parse("header_key = 00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff");
        let s = format!("{k:?}");
        assert!(s.contains("header_key") && !s.contains("00112233") && !s.contains("17") && !s.contains("[0, 17"));
    }

    #[test]
    fn master_keys_stop_at_a_gap() {
        assert_eq!(Keys::parse("master_key_00 = 01\nmaster_key_02 = 02").master_keys(), 1);
    }
}
