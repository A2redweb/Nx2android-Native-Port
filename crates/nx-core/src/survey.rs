//! What a game's own files say about it, for a game the build has no profile for: written to `survey.txt` of its data folder and sent with the shared log, so a profile can be made
//! from facts rather than guesses. Reads ExeFS (module build IDs and sizes, engine and SDK strings, main.npdm) and RomFS (the biggest files, extensions, Unreal paks and containers).
//! No key is read or written here.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

fn find(d: &[u8], pat: &[u8]) -> Option<usize> {
    d.windows(pat.len()).position(|w| w == pat)
}

/// printable ASCII run starting at `at`, up to `max` bytes
fn run(d: &[u8], at: usize, max: usize) -> String {
    d[at..].iter().take(max).take_while(|b| (0x20..0x7f).contains(*b)).map(|&b| b as char).collect()
}

fn engine_strings(img: &[u8]) -> Vec<String> {
    let mut out = vec![];
    for pat in [&b"++UE4+Release-"[..], b"++UE5+Release-", b"NintendoSdk_nnSdk-", b"nnSdk-", b"Unity Player version", b"UnityPlayer", b"Havok", b"CRIWARE", b"Wwise", b"FMOD", b"Oodle", b"nvnBootstrapLoader", b"vkCreateInstance", b"eglGetProcAddress", b"global-metadata.dat"] {
        if let Some(p) = find(img, pat) {
            let s = run(img, p, 60);
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

pub fn survey(game_dir: &Path) -> String {
    let mut s = String::new();
    let exefs = game_dir.join("ExeFS");
    let _ = writeln!(s, "# survey of {}", game_dir.file_name().and_then(|n| n.to_str()).unwrap_or("?"));
    let mut names: Vec<_> = std::fs::read_dir(&exefs).map(|r| r.filter_map(Result::ok).map(|e| e.path()).collect()).unwrap_or_default();
    names.sort();
    let _ = writeln!(s, "\n## ExeFS");
    for p in &names {
        let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let Ok(bytes) = std::fs::read(p) else { continue };
        if n == "main.npdm" {
            let a = find(&bytes, b"ACI0");
            let sysres = if bytes.get(..4) == Some(b"META") && bytes.len() > 0x18 { u32::from_le_bytes(bytes[0x14..0x18].try_into().unwrap()) } else { 0 };
            let pid = a.and_then(|a| bytes.get(a + 0x10..a + 0x18)).map(|b| u64::from_le_bytes(b.try_into().unwrap()));
            let _ = writeln!(s, "main.npdm\t{} bytes\t64-bit {}\tsystem resource {sysres:#x}\tprogram id {}", bytes.len(), bytes.get(0xC).map(|b| b & 1 != 0).unwrap_or(false), pid.map(|p| format!("{p:016x}")).unwrap_or("?".into()));
            continue;
        }
        match nx_nso::Nso::parse(&bytes) {
            Ok(nso) => {
                let _ = writeln!(s, "{n}\tbuild {}\tsegments {:#x} {:#x} {:#x}\tbss {:#x}", nso.build_id_hex().to_lowercase(), nso.segments[0].size, nso.segments[1].size, nso.segments[2].size, nso.bss);
                if let Ok(img) = nso.image(&bytes) {
                    for e in engine_strings(&img) {
                        let _ = writeln!(s, "  string\t{e}");
                    }
                }
            }
            Err(_) => {
                let _ = writeln!(s, "{n}\t{} bytes (not an NSO)", bytes.len());
            }
        }
    }
    let romfs = game_dir.join("RomFS");
    let mut files: Vec<(String, u64)> = vec![];
    fn walk(root: &Path, base: &Path, out: &mut Vec<(String, u64)>) {
        let Ok(rd) = std::fs::read_dir(root) else { return };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if let Ok(m) = p.metadata() {
                out.push((format!("/{}", p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/")), m.len()));
            }
        }
    }
    walk(&romfs, &romfs, &mut files);
    files.sort();
    let total: u64 = files.iter().map(|f| f.1).sum();
    let _ = writeln!(s, "\n## RomFS\n{} files, {} bytes", files.len(), total);
    let mut ext: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for (p, n) in &files {
        let e = p.rsplit_once('.').map(|x| x.1.to_lowercase()).unwrap_or_default();
        let t = ext.entry(e).or_default();
        t.0 += 1;
        t.1 += n;
    }
    for (e, (n, b)) in &ext {
        let _ = writeln!(s, "ext .{e}\t{n} files\t{b} bytes");
    }
    let mut big = files.clone();
    big.sort_by(|a, b| b.1.cmp(&a.1));
    let _ = writeln!(s, "largest files:");
    for (p, n) in big.iter().take(40) {
        let head = std::fs::File::open(romfs.join(p.trim_start_matches('/'))).ok().and_then(|mut f| {
            use std::io::Read;
            let mut b = [0u8; 16];
            let k = f.read(&mut b).ok()?;
            Some(b[..k].iter().map(|x| format!("{x:02x}")).collect::<String>())
        });
        let _ = writeln!(s, "  {p}\t{n}\t{}", head.unwrap_or_default());
    }
    let _ = writeln!(s, "small files (first 60 by path):");
    for (p, n) in files.iter().take(60) {
        let _ = writeln!(s, "  {p}\t{n}");
    }
    // Unreal: paks, containers, shader archives
    let _ = writeln!(s, "\n## Unreal Engine files");
    let (paks, tocs) = nx_shader::unreal::unreal_files(&romfs);
    let _ = writeln!(s, "{} version 11 pak(s), {} IoStore container(s) with shader libraries", paks.len(), tocs.len());
    for (p, n) in &files {
        if p.ends_with(".pak") || p.ends_with(".utoc") || p.ends_with(".ucas") || p.ends_with(".ushaderbytecode") {
            let _ = writeln!(s, "  {p}\t{n}");
        }
    }
    for p in &paks {
        let line = match nx_shader::unreal::pak_index_block(p) {
            Ok(None) => match nx_shader::unreal::Pak::open(p, None) {
                Ok(k) => {
                    let sh: Vec<&String> = k.files.keys().filter(|n| n.ends_with(".ushaderbytecode")).collect();
                    format!("index not encrypted; {} files; methods {:?}; shader archives: {}", k.files.len(), k.methods, sh.iter().take(12).map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
                }
                Err(e) => format!("not readable: {e}"),
            },
            Ok(Some(_)) => "index encrypted".to_string(),
            Err(e) => format!("not readable: {e}"),
        };
        let _ = writeln!(s, "  {}: {line}", p.file_name().and_then(|n| n.to_str()).unwrap_or("?"));
    }
    if let Ok(rd) = std::fs::read_dir(game_dir) {
        let aoc = game_dir.join("aoc");
        if aoc.is_dir() {
            let _ = writeln!(s, "\n## add-on content\n{} folder(s)", std::fs::read_dir(&aoc).map(|r| r.count()).unwrap_or(0));
        }
        drop(rd);
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_survey_of_a_small_game_folder() {
        let d = std::env::temp_dir().join(format!("nx_survey_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("ExeFS")).unwrap();
        std::fs::create_dir_all(d.join("RomFS/Game/Content")).unwrap();
        std::fs::write(d.join("RomFS/Game/Content/a.bin"), [1u8, 2, 3]).unwrap();
        std::fs::write(d.join("ExeFS/main"), b"junk").unwrap();
        let s = super::survey(&d);
        assert!(s.contains("1 files, 3 bytes") && s.contains("main\t4 bytes (not an NSO)") && s.contains("0 version 11 pak"), "{s}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
