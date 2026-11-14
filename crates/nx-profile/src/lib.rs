//! Game profiles, read the way upstream reads them (`nxnative/dm/games/game.py`, `nxnative/dm/android/tools/android_profile.py`), and the list of games built from them
//! (upstream's `porter profiles --all --json`). No dependencies.
//!
//! A game profile is `games/<title id>/game.tsv`: `key <TAB> value <TAB> value ...`, `#` starts a comment line. Its Android keys are `android.tsv` beside it.
//! A game without an `android.tsv` is not ported to Android (listed as "ios only").

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// `game.tsv`: ordered rows of a key and its tab-separated values.
#[derive(Debug, Clone)]
pub struct GameProfile {
    pub id: String,
    pub dir: PathBuf,
    rows: Vec<(String, Vec<String>)>,
}

impl GameProfile {
    pub fn parse(id: &str, dir: &Path, text: &str) -> Self {
        let mut rows = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let mut c = line.split('\t');
            let key = c.next().unwrap_or("").trim().to_string();
            rows.push((key, c.map(|v| v.trim().to_string()).collect()));
        }
        GameProfile { id: id.to_string(), dir: dir.to_path_buf(), rows }
    }

    /// Reads `<dir>/game.tsv`; the profile's id is the folder's name (the base game's title ID).
    pub fn load(dir: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(dir.join("game.tsv"))?;
        let id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Self::parse(&id, dir, &text))
    }

    /// Every row of a key (keys such as `build` repeat).
    pub fn values(&self, key: &str) -> Vec<&[String]> {
        self.rows.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_slice()).collect()
    }

    /// The first row's values joined by tabs, as upstream's `Game.get`.
    pub fn get(&self, key: &str) -> Option<String> {
        self.rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.join("\t"))
    }

    pub fn name(&self) -> String {
        self.get("name").unwrap_or_else(|| self.id.clone())
    }
}

pub const STATUSES: [&str; 4] = ["playable", "wip", "boots", "experimental"];
const SINGLE: [&str; 6] = ["status", "tested", "package", "orientation", "requires", "sysdata"];
const REPEAT: [&str; 3] = ["setting", "env", "overlay"];
const LAYOUTS: [&str; 2] = ["tablet", "phone"];

/// A profile file that upstream's tools would refuse; the text names the file and every problem found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileError(pub String);

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ProfileError {}

/// `android.tsv`: how the game runs on Android.
#[derive(Debug, Clone)]
pub struct AndroidProfile {
    pub id: String,
    rows: Vec<(String, String)>,
}

fn is_package(s: &str) -> bool {
    // [a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|p| {
            let mut ch = p.chars();
            matches!(ch.next(), Some('a'..='z')) && ch.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
        })
}

fn is_fraction(s: &str) -> bool {
    // \d*\.?\d+
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits = |t: &str| t.chars().all(|c| c.is_ascii_digit());
    match frac {
        None => !int.is_empty() && digits(int),
        Some(f) => digits(int) && !f.is_empty() && digits(f),
    }
}

fn is_env_row(s: &str) -> bool {
    // [A-Z_][A-Z0-9_]*=.*
    let Some((k, _)) = s.split_once('=') else { return false };
    let mut ch = k.chars();
    matches!(ch.next(), Some('A'..='Z' | '_')) && ch.all(|c| matches!(c, 'A'..='Z' | '0'..='9' | '_'))
}

fn setting_ok(name: &str, value: &str) -> bool {
    match name {
        "overlay" | "rumble" | "docked" | "fps" => value == "0" || value == "1",
        "opacity" => value.parse::<u32>().map(|v| (20..=80).contains(&v) && v % 5 == 0).unwrap_or(false) && !value.starts_with('+'),
        _ => false,
    }
}

impl AndroidProfile {
    /// `path` is only for messages. `controls`: the control names of `dm/android/overlay_default.tsv`; when given, `overlay` rows are checked against them.
    pub fn parse(id: &str, path: &str, text: &str, controls: Option<&HashSet<String>>) -> Result<Self, ProfileError> {
        let mut rows: Vec<(String, String)> = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let n = n + 1;
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('\t').unwrap_or((line, ""));
            let (k, v) = (k.trim(), v.trim());
            if !SINGLE.contains(&k) && !REPEAT.contains(&k) {
                return Err(ProfileError(format!("{path}:{n}: unknown key {k:?} (keys: {})", [SINGLE.as_slice(), REPEAT.as_slice()].concat().join(", "))));
            }
            if SINGLE.contains(&k) && rows.iter().any(|(rk, _)| rk == k) {
                return Err(ProfileError(format!("{path}:{n}: {k} given twice")));
            }
            rows.push((k.to_string(), v.to_string()));
        }
        let p = AndroidProfile { id: id.to_string(), rows };
        p.check(path, controls)?;
        Ok(p)
    }

    pub fn get(&self, key: &str, default: &str) -> String {
        self.rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_else(|| default.to_string())
    }

    pub fn values(&self, key: &str) -> Vec<&str> {
        self.rows.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str()).collect()
    }

    pub fn status(&self) -> String {
        self.get("status", "experimental")
    }
    pub fn tested(&self) -> String {
        self.get("tested", "")
    }
    pub fn package(&self) -> String {
        let p = self.get("package", "");
        if p.is_empty() { format!("org.nx2a.t{}", self.id.to_lowercase()) } else { p }
    }
    pub fn landscape(&self) -> bool {
        self.get("orientation", "landscape") == "landscape"
    }
    pub fn requires(&self) -> Vec<String> {
        self.get("requires", "").split_whitespace().map(str::to_string).collect()
    }
    /// System data title IDs the game reads from the console's firmware.
    pub fn sysdata(&self) -> Vec<u64> {
        self.get("sysdata", "").split_whitespace().filter_map(|t| u64::from_str_radix(t, 16).ok()).collect()
    }
    /// `KEY=VALUE` rows: the host switches the game needs (the data pack's `env.txt`).
    pub fn env(&self) -> Vec<&str> {
        self.values("env")
    }
    /// The pause menu's defaults, `(name, value)`.
    pub fn settings(&self) -> Vec<(&str, &str)> {
        self.values("setting").into_iter().filter_map(|v| v.split_once('=')).collect()
    }

    fn check(&self, path: &str, controls: Option<&HashSet<String>>) -> Result<(), ProfileError> {
        let mut bad: Vec<String> = Vec::new();
        let status = self.status();
        if !STATUSES.contains(&status.as_str()) {
            bad.push(format!("status {status:?}: one of {}", STATUSES.join(", ")));
        }
        let package = self.package();
        if !is_package(&package) {
            bad.push(format!("package {package:?} is not a package name"));
        }
        if !matches!(self.get("orientation", "landscape").as_str(), "landscape" | "any") {
            bad.push("orientation: landscape or any".into());
        }
        for r in self.requires() {
            if r != "bcn" {
                bad.push(format!("requires {r:?}: one of bcn"));
            }
        }
        for t in self.get("sysdata", "").split_whitespace() {
            if !(t.len() == 16 && t.chars().all(|c| c.is_ascii_hexdigit())) {
                bad.push(format!("sysdata {t:?}: a 16-digit hex title ID"));
            }
        }
        for v in self.values("setting") {
            let ok = v.split_once('=').map(|(k, val)| setting_ok(k, val)).unwrap_or(false);
            if !ok {
                bad.push(format!("setting {v:?}: overlay=0|1, rumble=0|1, docked=0|1, fps=0|1, opacity=20..80 by 5"));
            }
        }
        for v in self.values("env") {
            if !is_env_row(v) {
                bad.push(format!("env {v:?}: KEY=VALUE"));
            }
        }
        for v in self.values("overlay") {
            let c: Vec<&str> = v.split_whitespace().collect();
            let known = controls.map(|s| c.len() == 4 && s.contains(c[1])).unwrap_or(true);
            if c.len() != 4 || !LAYOUTS.contains(&c[0]) || !known || !c[2..].iter().all(|x| is_fraction(x)) {
                bad.push(format!("overlay {v:?}: <tablet|phone> <control> <x> <y>"));
            }
        }
        if bad.is_empty() { Ok(()) } else { Err(ProfileError(format!("{path}: {}", bad.join("; ")))) }
    }
}

/// The control names of `dm/android/overlay_default.tsv` (column 2; rows are `layout name kind anchor x y w h button label`).
pub fn overlay_controls(text: &str) -> HashSet<String> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split('\t').nth(1).map(str::to_string))
        .collect()
}

/// One game of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub android: bool,
    /// playable / wip / boots / experimental, or "ios only" for a game without `android.tsv`
    pub status: String,
    pub tested: String,
    pub package: String,
}

pub fn entry(g: &GameProfile, p: Option<&AndroidProfile>) -> Entry {
    match p {
        Some(p) => Entry { id: g.id.clone(), name: g.name(), android: true, status: p.status(), tested: p.tested(), package: p.package() },
        None => Entry { id: g.id.clone(), name: g.name(), android: false, status: "ios only".into(), tested: String::new(), package: format!("org.nx2a.t{}", g.id.to_lowercase()) },
    }
}

/// Every game under a profiles folder (`nxnative/dm/games`), in folder-name order, with its Android profile when it has one.
pub fn load_all(root: &Path, controls: Option<&HashSet<String>>) -> Result<Vec<(GameProfile, Option<AndroidProfile>)>, String> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .map_err(|e| format!("{}: {e}", root.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("game.tsv").is_file())
        .collect();
    dirs.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    let mut out = Vec::new();
    for d in dirs {
        let g = GameProfile::load(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        let a = d.join("android.tsv");
        let p = if a.is_file() {
            let text = fs::read_to_string(&a).map_err(|e| format!("{}: {e}", a.display()))?;
            Some(AndroidProfile::parse(&g.id, &a.display().to_string(), &text, controls).map_err(|e| e.0)?)
        } else {
            None
        };
        out.push((g, p));
    }
    Ok(out)
}

/// The whole list (every game, `android_only` leaves out those that are not ported to Android).
pub fn catalog(root: &Path, controls: Option<&HashSet<String>>, android_only: bool) -> Result<Vec<Entry>, String> {
    Ok(load_all(root, controls)?.iter().map(|(g, p)| entry(g, p.as_ref())).filter(|e| e.android || !android_only).collect())
}

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The list as JSON, the keys and order of upstream's `porter profiles --all --json`.
pub fn to_json(entries: &[Entry]) -> String {
    let mut out = String::from("[");
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str("{\"id\": ");
        json_str(&mut out, &e.id);
        out.push_str(", \"name\": ");
        json_str(&mut out, &e.name);
        let _ = write!(out, ", \"android\": {}, \"status\": ", e.android);
        json_str(&mut out, &e.status);
        out.push_str(", \"tested\": ");
        json_str(&mut out, &e.tested);
        out.push_str(", \"package\": ");
        json_str(&mut out, &e.package);
        out.push('}');
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_rows_and_get() {
        let g = GameProfile::parse("0100000000010000", Path::new("x"), "# c\n\ntitle_id\t0100000000010000\nname\tSuper\tMario \nbuild\tv1\t1.0\nbuild\tv2\t1.1\n");
        assert_eq!(g.get("title_id").as_deref(), Some("0100000000010000"));
        assert_eq!(g.name(), "Super\tMario"); // values joined by a tab, each trimmed
        assert_eq!(g.values("build").len(), 2);
        assert_eq!(GameProfile::parse("ID", Path::new("x"), "").name(), "ID");
    }

    #[test]
    fn android_defaults_and_package() {
        let p = AndroidProfile::parse("0100ABCD", "a.tsv", "# only comments\n", None).unwrap();
        assert_eq!(p.status(), "experimental");
        assert_eq!(p.package(), "org.nx2a.t0100abcd");
        assert!(p.landscape());
    }

    #[test]
    fn android_errors() {
        let e = |t: &str| AndroidProfile::parse("1", "a.tsv", t, None).unwrap_err().0;
        assert!(e("bogus\tx\n").contains("unknown key \"bogus\""));
        assert!(e("status\tplayable\nstatus\twip\n").contains("a.tsv:2: status given twice"));
        assert!(e("status\tgreat\n").contains("status \"great\""));
        assert!(e("package\tNot.Valid\n").contains("is not a package name"));
        assert!(e("package\tsolo\n").contains("is not a package name"));
        assert!(e("env\tlower=1\n").contains("KEY=VALUE"));
        assert!(e("setting\topacity=22\n").contains("setting"));
        assert!(e("sysdata\t0100\n").contains("16-digit"));
        assert!(e("requires\tastc\n").contains("one of bcn"));
        assert!(e("orientation\tportrait\n").contains("landscape or any"));
    }

    #[test]
    fn android_ok_values() {
        let t = "status\tplayable\ntested\tv393216 Y700\npackage\tcom.x.y\nenv\tDM_VK_ASYNC_PIPE=0\nenv\tDM_VK_PASS_TIMES=1\nsetting\topacity=25\nsetting\tdocked=1\nrequires\tbcn\nsysdata\t0100000000000802\n";
        let p = AndroidProfile::parse("1", "a.tsv", t, None).unwrap();
        assert_eq!(p.status(), "playable");
        assert_eq!(p.env(), vec!["DM_VK_ASYNC_PIPE=0", "DM_VK_PASS_TIMES=1"]);
        assert_eq!(p.settings(), vec![("opacity", "25"), ("docked", "1")]);
        assert_eq!(p.sysdata(), vec![0x0100000000000802]);
    }

    #[test]
    fn overlay_rows() {
        let controls: HashSet<String> = ["menu".to_string(), "a".to_string()].into_iter().collect();
        assert!(AndroidProfile::parse("1", "a", "overlay\ttablet menu 0.1 .5\n", Some(&controls)).is_ok());
        assert!(AndroidProfile::parse("1", "a", "overlay\ttablet nope 0.1 0.5\n", Some(&controls)).is_err());
        assert!(AndroidProfile::parse("1", "a", "overlay\twatch menu 0.1 0.5\n", Some(&controls)).is_err());
        assert!(AndroidProfile::parse("1", "a", "overlay\ttablet menu 0.1\n", Some(&controls)).is_err());
        assert!(AndroidProfile::parse("1", "a", "overlay\ttablet menu 1. 0.5\n", Some(&controls)).is_err());
        assert!(is_fraction("12") && is_fraction("0.25") && is_fraction(".5") && !is_fraction("") && !is_fraction("1.") && !is_fraction("-1"));
    }

    #[test]
    fn json_escapes() {
        let e = Entry { id: "1".into(), name: "A \"q\"\\".into(), android: false, status: "ios only".into(), tested: String::new(), package: "org.nx2a.t1".into() };
        assert_eq!(to_json(&[e]), r#"[{"id": "1", "name": "A \"q\"\\", "android": false, "status": "ios only", "tested": "", "package": "org.nx2a.t1"}]"#);
    }
}
