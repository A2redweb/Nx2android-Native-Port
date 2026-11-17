//! Frame step stubs (`FRAME_STEP` of upstream's per-game patch files) as data: `tools/gen_cuts.py` runs each build's stub function against a recording context and writes `framestep.tsv`.
//! Lines: `build ID \t op \t …` with ops `site off original-hex`, `emit word`, `put off words why`, `render_scale 1`. A word is hex, or a pc-relative one kept symbolic
//! (`B:from:to`, `BEQ:from:to`, `ADRP:reg:pc:target`, optionally `|bits` or-ed in), whose addresses may name the patch area (0x40000000 + 4 × words emitted so far) or
//! the state page (0x50000000 + offset in it); they are resolved when the module is rewritten, because only then is the patch area's place known.

use crate::asm;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Word {
    Raw(u32),
    B(i64, i64, u32),
    Beq(i64, i64, u32),
    Adrp(u32, i64, i64, u32),
}

#[derive(Clone, Debug)]
pub enum Op {
    Site(i64, Vec<u8>),
    Emit(Word),
    Put(i64, Vec<Word>, String),
    RenderScale,
}

const STUB: i64 = 0x4000_0000;
const STATE: i64 = 0x5000_0000;

fn num(s: &str) -> Option<i64> {
    i64::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

fn word(s: &str) -> Option<Word> {
    let (body, extra) = match s.split_once('|') {
        Some((b, e)) => (b, num(e)? as u32),
        None => (s, 0),
    };
    if let Some(h) = body.strip_prefix("0x").filter(|_| !body.contains(':')) {
        return Some(Word::Raw(u32::from_str_radix(h, 16).ok()? | extra));
    }
    let f: Vec<&str> = body.split(':').collect();
    match (f[0], f.len()) {
        ("B", 3) => Some(Word::B(num(f[1])?, num(f[2])?, extra)),
        ("BEQ", 3) => Some(Word::Beq(num(f[1])?, num(f[2])?, extra)),
        ("ADRP", 4) => Some(Word::Adrp(num(f[1])? as u32, num(f[2])?, num(f[3])?, extra)),
        _ => None,
    }
}

pub fn parse(text: &str) -> Result<HashMap<String, Vec<Op>>, String> {
    let mut out: HashMap<String, Vec<Op>> = HashMap::new();
    for (n, l) in text.lines().enumerate() {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.splitn(5, '\t').collect();
        let bad = || format!("framestep.tsv line {}: malformed", n + 1);
        if f.len() < 2 {
            return Err(bad());
        }
        let op = match f[1] {
            "site" if f.len() >= 4 => {
                let h = f[3];
                let bytes = (0..h.len()).step_by(2).map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok()).collect::<Option<Vec<u8>>>().ok_or_else(bad)?;
                Op::Site(num(f[2]).ok_or_else(bad)?, bytes)
            }
            "emit" if f.len() >= 3 => Op::Emit(word(f[2]).ok_or_else(bad)?),
            "put" if f.len() >= 4 => Op::Put(num(f[2]).ok_or_else(bad)?, f[3].split(' ').map(word).collect::<Option<Vec<_>>>().ok_or_else(bad)?, f.get(4).unwrap_or(&"").to_string()),
            "render_scale" => Op::RenderScale,
            _ => return Err(bad()),
        };
        out.entry(f[0].to_string()).or_default().push(op);
    }
    Ok(out)
}

/// Where the stubs and the state page are.
pub struct Place {
    pub stub: i64,
    pub state: i64,
}

impl Place {
    fn at(&self, v: i64) -> i64 {
        if v >= STATE {
            self.state + (v - STATE)
        } else if v >= STUB {
            self.stub + (v - STUB)
        } else {
            v
        }
    }

    pub fn resolve(&self, w: &Word) -> Result<u32, String> {
        Ok(match w {
            Word::Raw(x) => *x,
            Word::B(f, t, e) => asm::b(self.at(*f), self.at(*t))? | e,
            Word::Beq(f, t, e) => asm::beq(self.at(*f), self.at(*t))? | e,
            Word::Adrp(r, pc, t, e) => asm::adrp(*r, self.at(*pc), self.at(*t))? | e,
        })
    }
}

/// What a build's stubs come to: the words to append to the patch area, the cuts to apply (offset, original bytes, new bytes, reason), the sites to check, and whether there is a render-scale stub.
pub struct Plan {
    pub emitted: Vec<u32>,
    pub cuts: Vec<(i64, Vec<u32>, String)>,
    pub sites: Vec<(i64, Vec<u8>)>,
    pub render_scale: bool,
}

pub fn plan(ops: &[Op], place: &Place) -> Result<Plan, String> {
    let mut p = Plan { emitted: vec![], cuts: vec![], sites: vec![], render_scale: false };
    for op in ops {
        match op {
            Op::Site(o, b) => p.sites.push((*o, b.clone())),
            Op::Emit(w) => p.emitted.push(place.resolve(w)?),
            Op::Put(o, ws, why) => p.cuts.push((*o, ws.iter().map(|w| place.resolve(w)).collect::<Result<_, _>>()?, why.clone())),
            Op::RenderScale => p.render_scale = true,
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_resolve() {
        let t = "ab\temit\tADRP:0x10:0x40000000:0x50000000\nab\temit\t0xb9400210\nab\tput\t0x8d94b0\tB:0x8d94b0:0x40000000|0x80000000\tmovie\nab\tsite\t0x10\t88ae40f9\nab\trender_scale\t1\n";
        let m = parse(t).unwrap();
        let ops = &m["ab"];
        assert_eq!(ops.len(), 5);
        let place = Place { stub: 0x100_0000, state: 0x20_0000 };
        let p = plan(ops, &place).unwrap();
        assert_eq!(p.emitted[1], 0xb9400210);
        // adrp x16 from the stub page to the state page: -0xe0 pages
        assert_eq!(p.emitted[0], asm::adrp(16, 0x100_0000, 0x20_0000).unwrap());
        assert_eq!(p.cuts[0].1, vec![asm::b(0x8d94b0, 0x100_0000).unwrap() | 0x8000_0000]);
        assert_eq!(p.sites[0], (0x10, vec![0x88, 0xae, 0x40, 0xf9]));
        assert!(p.render_scale);
        assert!(parse("ab\tnope\n").is_err());
    }
}
