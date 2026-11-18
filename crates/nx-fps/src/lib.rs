//! 60 fps from the community's FPSLocker configs (masagrator's FPSLocker-Warehouse, FORMAT.md there): per game build, the memory writes that lift a game's own frame
//! limit. The app fetches the config of the build it runs and this crate converts it for the host:
//!   ALL_FPS write / evaluate_write / compare / evaluate_compare at MAIN addresses → fpslock.tsv (read by hle_fpslock.c, DM_FPS_LOCKS); the expressions depend only on the
//!     frame-rate target, so they are evaluated here for 30 and 60
//!   MASTER_WRITE bytes → cuts for the rewriter, with the original bytes taken from main's image
//!   block timing: dropped (it switches off FPSLocker's own frame delay, which the host does not have)
//! A config that uses anything else (asm_a64, DECLARATIONS, VARIABLE / HEAP / ALIAS addresses, refresh_rate) is refused whole: its entries work together, and half of
//! them can leave the game worse off than at 30 fps. Same rules as upstream's porter/fpslocker.py. The warehouse has no license: configs are fetched for the player's own
//! build and never kept in the repository. Std only; the YAML reader handles the subset the configs use.

mod expr;
mod yaml;

use expr::Num;
use yaml::Value;

#[derive(Debug, PartialEq)]
pub struct Refused(pub String);
type R<T> = Result<T, Refused>;
fn no<T>(s: impl Into<String>) -> R<T> {
    Err(Refused(s.into()))
}

pub const RAW: &str = "https://raw.githubusercontent.com/masagrator/FPSLocker-Warehouse/v4/SaltySD/plugins/FPSLocker/patches/";

/// Where the warehouse keeps the config of a build.
pub fn url(title_id: &str, build_id: &str) -> String {
    format!("{RAW}{}/{}.yaml", title_id.to_uppercase(), build_id.chars().take(16).collect::<String>().to_uppercase())
}

/// A cut of the rewriter: offset in main's image, the bytes there, the bytes to put, why.
#[derive(Debug, PartialEq)]
pub struct CutRow {
    pub off: usize,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
    pub why: String,
}

const TARGETS: [i64; 2] = [30, 60];

fn kind_size(k: &str) -> Option<usize> {
    Some(match k {
        "int8" | "uint8" => 1,
        "int16" | "uint16" => 2,
        "int32" | "uint32" | "float" => 4,
        "int64" | "uint64" | "double" => 8,
        _ => return None,
    })
}

fn is_float(k: &str) -> bool {
    k == "float" || k == "double"
}

fn fits(k: &str, n: &Num) -> bool {
    if is_float(k) {
        return true;
    }
    let v = match n {
        Num::I(i) => *i,
        Num::F(f) => f.trunc() as i128,
    };
    let bits = kind_size(k).unwrap() * 8;
    if k.starts_with('u') {
        v >= 0 && v < (1i128 << bits)
    } else {
        v >= -(1i128 << (bits - 1)) && v < (1i128 << (bits - 1))
    }
}

/// The value list of an entry at a target, as strings the host reads back ("2", "0.03333333333333333").
fn values(v: &Value, kind: &str, t: i64, evaluated: bool) -> R<Vec<(Num, String)>> {
    let items: Vec<&Value> = match v {
        Value::List(l) => l.iter().collect(),
        x => vec![x],
    };
    let mut out = vec![];
    for x in items {
        let n = match x {
            Value::Int(i) => Num::I(*i as i128),
            Value::Float(f) => Num::F(*f),
            Value::Str(s) if evaluated => expr::eval(s, t)?,
            Value::Str(s) => return no(format!("value {s:?} is not a number")),
            _ => return no(format!("value {x:?} is not a number")),
        };
        let n = if is_float(kind) { Num::F(n.f()) } else { Num::I(n.trunc()) };
        if !fits(kind, &n) {
            return no(format!("value {x:?} does not fit {kind}"));
        }
        let s = match &n {
            Num::I(i) => i.to_string(),
            Num::F(f) => format!("{f:?}"),
        };
        out.push((n, s));
    }
    Ok(out)
}

fn join(v: &[(Num, String)]) -> String {
    v.iter().map(|x| x.1.as_str()).collect::<Vec<_>>().join(",")
}

/// [MAIN, off, …]: every offset but the last names a pointer.
fn chain(a: Option<&Value>, what: &str) -> R<String> {
    let l = match a {
        Some(Value::List(l)) if l.len() >= 2 => l,
        other => return no(format!("{what} {other:?}: not [MAIN, offset, …]")),
    };
    if !matches!(&l[0], Value::Str(s) if s == "MAIN") {
        return no(format!("{what} in region {:?}: only MAIN addresses are supported", l[0]));
    }
    let mut out = vec![];
    for (i, o) in l[1..].iter().enumerate() {
        match o {
            Value::Int(n) if !(i == 0 && *n < 0) => out.push(if *n >= 0 { format!("{n:#x}") } else { format!("-{:#x}", -n) }),
            _ => return no(format!("{what} {l:?}: offsets must be numbers, the first not negative")),
        }
    }
    Ok(out.join(","))
}

/// The config → (fpslock.tsv text, cut rows). Refused names what the host cannot do.
pub fn convert(text: &str, image: &[u8], build_id: &str, source: &str) -> R<(String, Vec<CutRow>)> {
    let y = yaml::parse(text).map_err(Refused)?;
    let map = match &y {
        Value::Map(m) => m,
        _ => return no("the config is not a YAML mapping"),
    };
    for (k, _) in map {
        if !["ALL_FPS", "MASTER_WRITE", "DECLARATIONS", "unsafeCheck", "Addons"].contains(&k.as_str()) {
            return no(format!("unknown section {k}"));
        }
    }
    let get = |k: &str| map.iter().find(|(n, _)| n == k).map(|(_, v)| v);
    let nonempty = |v: Option<&Value>| match v {
        None | Some(Value::Null) => false,
        Some(Value::List(l)) => !l.is_empty(),
        Some(Value::Map(m)) => !m.is_empty(),
        _ => true,
    };
    if nonempty(get("DECLARATIONS")) {
        return no("it declares variables or code (DECLARATIONS), which the host has no buffer for");
    }
    if nonempty(get("Addons")) {
        return no("it needs extra files (Addons)");
    }
    fn list(v: Option<&Value>) -> R<Vec<&Value>> {
        match v {
            None | Some(Value::Null) => Ok(vec![]),
            Some(Value::List(l)) => Ok(l.iter().collect()),
            _ => no("a section is not a list"),
        }
    }
    let mut rows = vec![];
    let mut cuts = vec![];
    for (i, e) in list(get("ALL_FPS"))?.into_iter().enumerate() {
        let what = format!("ALL_FPS entry {}", i + 1);
        let m = match e {
            Value::Map(m) => m,
            _ => return no(format!("{what} is not a mapping")),
        };
        let f = |k: &str| m.iter().find(|(n, _)| n == k).map(|(_, v)| v);
        let s = |k: &str| match f(k) {
            Some(Value::Str(s)) => Some(s.as_str()),
            _ => None,
        };
        let t = s("type").unwrap_or("");
        if t == "block" {
            if s("what") != Some("timing") {
                return no(format!("{what}: block {:?}", f("what")));
            }
            continue;
        }
        if !["write", "evaluate_write", "compare", "evaluate_compare"].contains(&t) {
            return no(format!("{what}: type {t:?}"));
        }
        let kind = s("value_type").unwrap_or("");
        if kind == "refresh_rate" {
            return no(format!("{what}: it sets the display rate (refresh_rate), which the host keeps at 60 Hz"));
        }
        if kind_size(kind).is_none() {
            return no(format!("{what}: value type {kind:?}"));
        }
        let ev = t.starts_with("evaluate");
        let val = f("value").ok_or_else(|| Refused(format!("{what}: no value")))?;
        let v30 = join(&values(val, kind, TARGETS[0], ev)?);
        let v60 = join(&values(val, kind, TARGETS[1], ev)?);
        let mut cmp = "-".to_string();
        if t.ends_with("compare") {
            let (op, ck) = (s("compare_type").unwrap_or(""), s("compare_value_type").unwrap_or(""));
            if !["<", "<=", ">", ">=", "==", "!="].contains(&op) || kind_size(ck).is_none() {
                return no(format!("{what}: compare {op:?} {ck:?}"));
            }
            let cv = values(f("compare_value").ok_or_else(|| Refused(format!("{what}: no compare_value")))?, ck, 60, false)?;
            if cv.len() != 1 {
                return no(format!("{what}: compare value"));
            }
            cmp = format!("{ck} {} {op} {}", chain(f("compare_address"), &what)?, join(&cv));
        }
        rows.push(format!("{what}\t{kind}\t{}\t{v30}\t{v60}\t{cmp}", chain(f("address"), &what)?));
    }
    for (i, e) in list(get("MASTER_WRITE"))?.into_iter().enumerate() {
        let what = format!("MASTER_WRITE entry {}", i + 1);
        let m = match e {
            Value::Map(m) => m,
            _ => return no(format!("{what} is not a mapping")),
        };
        let f = |k: &str| m.iter().find(|(n, _)| n == k).map(|(_, v)| v);
        match f("type") {
            Some(Value::Str(s)) if s == "bytes" => {}
            other => return no(format!("{what}: type {other:?} (only bytes)")),
        }
        let (off, kind) = match (f("main_offset"), f("value_type")) {
            (Some(Value::Int(o)), Some(Value::Str(k))) if *o >= 0 && kind_size(k).is_some() => (*o as usize, k.as_str()),
            (o, k) => return no(format!("{what}: main_offset {o:?}, value type {k:?}")),
        };
        let mut new = vec![];
        for (n, _) in values(f("value").ok_or_else(|| Refused(format!("{what}: no value")))?, kind, 60, false)? {
            let sz = kind_size(kind).unwrap();
            let bytes: Vec<u8> = match (&n, kind) {
                (Num::F(x), "float") => (*x as f32).to_le_bytes().to_vec(),
                (Num::F(x), "double") => x.to_le_bytes().to_vec(),
                (Num::I(i), _) => (*i as u128).to_le_bytes()[..sz].to_vec(),
                (Num::F(x), _) => ((x.trunc() as i128) as u128).to_le_bytes()[..sz].to_vec(),
            };
            new.extend(bytes);
        }
        if off + new.len() > image.len() {
            return no(format!("{what}: main+{off:#x} is past main's image"));
        }
        let old = image[off..off + new.len()].to_vec();
        if old != new {
            cuts.push(CutRow { off, old, new, why: format!("FPSLocker-Warehouse {what}") });
        }
    }
    if rows.is_empty() && cuts.is_empty() {
        return no("it writes nothing the host would apply");
    }
    let mut out = format!(
        "# Written by Nx2Android Native from the FPSLocker-Warehouse config {source} (masagrator and contributors); read by hle_fpslock.c (DM_FPS_LOCKS)\n# entry <TAB> value type <TAB> main offsets (all but the last name a pointer) <TAB> values at 30 fps <TAB> at 60 fps <TAB> compare (type, offsets, operator, value; - = always)\nbuild\t{build_id}\n"
    );
    for r in rows {
        out.push_str(&r);
        out.push('\n');
    }
    Ok((out, cuts))
}

/// The cuts as rows of the app's cuts.tsv: build ID, offset, old bytes, new bytes, why.
pub fn cuts_tsv(build_id: &str, cuts: &[CutRow]) -> String {
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    cuts.iter().map(|c| format!("{build_id}\t{:#x}\t{}\t{}\t{}\n", c.off, hex(&c.old), hex(&c.new), c.why)).collect()
}

#[cfg(test)]
mod tests;
