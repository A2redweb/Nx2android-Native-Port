//! What the Android app calls: plain functions with string and number arguments, and background jobs whose progress is polled as JSON.
//! Kept free of JNI so it can be tested on a PC; `nx-jni` is the thin layer over it.

use nx_extract::Opened;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A string as a JSON string literal.
pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Writes the host's switch files into a game's data folder, as upstream's porter does: `env.txt` (`DM_LANG`, then the profile's `env` rows) and `settings.txt` (the profile's
/// `setting` rows; an existing one is the player's and stays). `profiles` is the folder of profile folders; the game's folder is found by ID, ignoring case.
pub fn write_switch_files(profiles: &Path, id: &str, game_dir: &Path, language: &str) -> Result<(), String> {
    let dir = std::fs::read_dir(profiles)
        .map_err(|e| format!("{}: {e}", profiles.display()))?
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(id))
        .map(|e| e.path())
        .ok_or_else(|| format!("no profile for {id}"))?;
    // a game with no Android profile (an experimental port: it has the iOS profile's values only) gets the defaults upstream's porter writes for it
    let text = std::fs::read_to_string(dir.join("android.tsv")).unwrap_or_else(|_| format!("status\texperimental\npackage\torg.nx2a.t{}\norientation\tlandscape\n", id.to_lowercase()));
    let p = nx_profile::AndroidProfile::parse(id, &dir.join("android.tsv").to_string_lossy(), &text, None).map_err(|e| e.0)?;
    let mut env = format!("# Written by Nx2Android Native for {id}; KEY=VALUE per line, read by the host at launch\nDM_LANG={language}\n");
    for r in p.env() {
        env.push_str(r);
        env.push('\n');
    }
    std::fs::create_dir_all(game_dir).map_err(|e| e.to_string())?;
    std::fs::write(game_dir.join("env.txt"), env).map_err(|e| e.to_string())?;
    let settings: String = p.settings().iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    if !settings.is_empty() && !game_dir.join("settings.txt").exists() {
        std::fs::write(game_dir.join("settings.txt"), settings).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Rewrites the game's code modules into `mods/` of its data folder (what the host loads: `<module>.patched.bin` with its `.fixups.tsv` and `.gates.tsv`).
/// The modules are the profile's `modules` row; `cuts_tsv` and `framestep_tsv` are `cuts.tsv` and `framestep.tsv` of `tools/gen_cuts.py`. `mods/` appears only when every module is done. Returns a one-line summary.
pub fn rewrite_modules(profiles: &Path, cuts_tsv: &str, framestep_tsv: &str, id: &str, game_dir: &Path) -> Result<String, String> {
    let dir = std::fs::read_dir(profiles)
        .map_err(|e| format!("{}: {e}", profiles.display()))?
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(id))
        .map(|e| e.path())
        .ok_or_else(|| format!("no profile for {id}"))?;
    let g = nx_profile::GameProfile::load(&dir).map_err(|e| format!("{id}: {e}"))?;
    let mut modules: Vec<String> = g.values("modules").into_iter().flat_map(|r| r.iter().flat_map(|w| w.split_whitespace().map(String::from)).collect::<Vec<_>>()).collect();
    if modules.is_empty() {
        modules = ["main", "sdk"].map(String::from).to_vec();
    }
    // cuts the port added for this game (60 fps: the MASTER_WRITE bytes of an FPSLocker config) follow the shared table
    let extra = std::fs::read_to_string(game_dir.join("extra_cuts.tsv")).unwrap_or_default();
    let all_cuts = format!("{cuts_tsv}\n{extra}");
    let cuts = nx_rewrite::cuts::parse(&all_cuts)?;
    let steps = nx_rewrite::framestep::parse(framestep_tsv)?;
    let tmp = game_dir.join("mods.tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let mut summary = vec![];
    // the builds this profile registers: `build <version> <display> module=id …`
    let registered: Vec<(String, Vec<(String, String)>)> = g
        .values("build")
        .into_iter()
        .filter_map(|r| {
            let ver = r.first()?.clone();
            Some((ver, r.iter().skip(2).filter_map(|w| w.split_once('=')).map(|(a, b)| (a.to_string(), b.to_lowercase())).collect()))
        })
        .collect();
    for m in &modules {
        let src = game_dir.join("ExeFS").join(m);
        if !src.is_file() {
            continue; // a profile lists the modules some builds have
        }
        let bytes = std::fs::read(&src).map_err(|e| format!("{m}: {e}"))?;
        let nso = nx_nso::Nso::parse(&bytes).map_err(|e| format!("{m}: {e}"))?;
        if !registered.is_empty() {
            let id_hex = nso.build_id_hex().to_lowercase();
            let known = |b: &str| registered.iter().any(|(_, mods)| mods.iter().any(|(n, v)| n == m && b.starts_with(&v[..v.len().min(b.len())]) ));
            if !known(&id_hex) {
                let vers: Vec<&str> = registered.iter().map(|(v, _)| v.as_str()).collect();
                summary.push(format!("[RED] {m}: build {} is not one this game's profile registers (versions {}); the game's patches do not apply to it and it will most likely not run: use the base game and update the profile was tested on", &id_hex[..16.min(id_hex.len())], vers.join(", ")));
            }
        }
        let list = cuts.get(&nso.build_id_hex()).map(Vec::as_slice).unwrap_or(&[]);
        let o = nx_rewrite::rewrite_with(&bytes, list, steps.get(&nso.build_id_hex()).map(Vec::as_slice).unwrap_or(&[])).map_err(|e| format!("{m}: {e}"))?;
        for (ext, data) in [("patched.bin", &o.patched[..]), ("fixups.tsv", o.fixups.as_bytes()), ("gates.tsv", o.gates.as_bytes()), ("sites.tsv", o.sites.as_bytes()), ("cuts.tsv", o.cuts.as_bytes()), ("obs.tsv", o.obs.as_bytes())] {
            std::fs::write(tmp.join(format!("{m}.{ext}")), data).map_err(|e| format!("{m}.{ext}: {e}"))?;
        }
        summary.push(format!("{m}: {} sites, {} cuts", o.stats.values().sum::<usize>(), list.len()));
    }
    if summary.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("{id}: none of the modules {} is in ExeFS", modules.join(" ")));
    }
    let mods = game_dir.join("mods");
    let _ = std::fs::remove_dir_all(&mods);
    std::fs::rename(&tmp, &mods).map_err(|e| e.to_string())?;
    Ok(summary.join("; "))
}

fn find_profile(profiles: &Path, id: &str) -> Result<std::path::PathBuf, String> {
    std::fs::read_dir(profiles)
        .map_err(|e| format!("{}: {e}", profiles.display()))?
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(id))
        .map(|e| e.path())
        .ok_or_else(|| format!("no profile for {id}"))
}

/// The SPIR-V and reflection of shaders of the game's `shaders.dmsv`, as text (base64), for `keys` (hex keys separated by spaces): what a bug report about a shader needs
pub fn shader_dump(game_dir: &Path, keys: &str) -> String {
    fn b64(d: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut o = String::with_capacity(d.len() * 4 / 3 + 4);
        for c in d.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() { o.push(A[(n >> (18 - 6 * i) & 63) as usize] as char) } else { o.push('=') }
            }
        }
        o
    }
    let bytes = match std::fs::read(game_dir.join("shaders.dmsv")) { Ok(b) => b, Err(e) => return format!("no shader bundle: {e}") };
    let bundle = match nx_dmsv::Bundle::parse(&bytes) { Ok(b) => b, Err(e) => return e };
    let mut out = String::new();
    for k in keys.split_whitespace() {
        let Ok(key) = u64::from_str_radix(k, 16) else { continue };
        if key == 0 { continue }
        match bundle.find(key) {
            None => out.push_str(&format!("{k}: not in the bundle\n")),
            Some(e) => match bundle.spv(e) {
                Ok(spv) => out.push_str(&format!("{k} stage {} spv {} bytes\nspv {}\nrefl {}\n", e.stage, spv.len(), b64(&spv), b64(bundle.refl(e)))),
                Err(x) => out.push_str(&format!("{k}: {x}\n")),
            },
        }
    }
    out
}

/// Builds `shaders.dmsv` of the game's data folder from the shaders in its RomFS: the profile's facts and handle remaps, the translator `xlate` (its command words) and a scratch folder.
/// Returns a one-line summary; fails when any shader that was meant to be in the bundle is missing from it.
pub fn build_shaders(profiles: &Path, id: &str, game_dir: &Path, xlate: Vec<String>, threads: usize, progress: &(dyn Fn(&str, usize, usize) + Sync)) -> Result<String, String> {
    use nx_shader::json::Json;
    let dir = find_profile(profiles, id)?;
    let g = nx_profile::GameProfile::load(&dir).map_err(|e| format!("{id}: {e}"))?;
    let mut facts = std::collections::BTreeMap::new();
    if let Ok(t) = std::fs::read_to_string(dir.join("shader_facts.tsv")) {
        for l in t.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            let (k, j) = l.split_once('\t').ok_or("shader_facts.tsv: bad row")?;
            facts.insert(k.to_string(), Json::parse(j).map_err(|e| format!("shader_facts.tsv {k}: {e}"))?);
        }
    }
    let mut handles = std::collections::BTreeMap::new();
    if let Ok(t) = std::fs::read_to_string(dir.join("android_shader_handles.tsv")) {
        let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|_| format!("android_shader_handles.tsv: bad number {s}"));
        for l in t.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 4 {
                return Err("android_shader_handles.tsv: bad row".into());
            }
            handles.insert(f[0].to_string(), (hex(f[1])?, hex(f[2])?, f[3].parse::<usize>().map_err(|_| "android_shader_handles.tsv: bad site count")?));
        }
    }
    let work = game_dir.join("shaders.work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    // main's memory image holds shaders too (embedded BNSH, GLSLC outputs); scanned like a file
    let img_dir = work.join("images");
    std::fs::create_dir_all(&img_dir).map_err(|e| e.to_string())?;
    let mut common = None;
    if let Ok(bytes) = std::fs::read(game_dir.join("ExeFS").join("main")) {
        let nso = nx_nso::Nso::parse(&bytes).map_err(|e| format!("main: {e}"))?;
        let img = nso.image(&bytes).map_err(|e| format!("main: {e}"))?;
        common = nx_shader::unity::common_strings(&img); // None: not a Unity game
        std::fs::write(img_dir.join("main"), img).map_err(|e| e.to_string())?;
    }
    let roots = [(img_dir, "".to_string()), (game_dir.join("RomFS"), "RomFS".to_string())];
    let (mut index, skipped) = nx_shader::scan::scan_dir(&roots, common, std::sync::Arc::new(nx_shader::container::zs_dicts(&game_dir.join("RomFS"))?), &work.join("spool.bin"), threads, &|d, t| progress("scanning", d, t))?;
    // shaders the game set while it ran, whose code no game file holds: saved by an earlier play (captured_shaders/), translated in now
    let (captured, cap_notes) = index.add_captures(&game_dir.join("captured_shaders"))?;
    // Game Freak's packs (Z-A's arc/data.trpfs, Arceus's .gfpak files): their members are Oodle-compressed and only the game's own decoder opens them; the host built as an
    // executable runs it (libdmhost_exe.so beside the translator), and what the members hold is added to the index
    let mut pack_note = String::new();
    let arcs = nx_shader::pack::archives(&game_dir.join("RomFS"));
    if !arcs.is_empty() {
        let exe = std::path::Path::new(xlate.first().map(String::as_str).unwrap_or("")).parent().map(|p| p.join("libdmhost_exe.so"));
        let host = exe.filter(|e| e.is_file()).map(|exe| nx_shader::pack::HostSpec { exe, mods: game_dir.join("mods"), cwd: game_dir.to_path_buf(), game: id.to_string(), log_dir: game_dir.join("decoder-logs") });
        match host {
            Some(h) if h.mods.is_dir() => {
                let mut ap = index.appender()?;
                let r = nx_shader::pack::scan_archives(&arcs, &h, threads.min(3), &mut ap, &|d, t| progress("decoding the game's archives", d, t));
                let added = ap.finish()?;
                match r {
                    Ok(o) => {
                        pack_note = format!(", {added} from the game's archives ({} files read{})", o.members, if o.failed > 0 { format!(", {} did not decode", o.failed) } else { String::new() });
                        if !o.notes.is_empty() {
                            pack_note.push_str(&format!("; [RED] {}", o.notes.join("; ")));
                        }
                    }
                    Err(e) => pack_note = format!(", {added} from the game's archives; [RED] the decoder stopped: {e} (see decoder-logs)"),
                }
            }
            Some(_) => pack_note = "; [RED] the game's archives are Oodle-compressed and the game's code is not rewritten yet (the decoder runs it)".into(),
            None => pack_note = "; [RED] the game's archives are Oodle-compressed and this build has no decoder (libdmhost_exe.so)".into(),
        }
    }
    let params = nx_shader::pipeline::Params {
        xlate,
        work: work.join("xlate"),
        threads,
        chunk: 64,
        subgroup: g.get("android_subgroup_size").and_then(|v| v.trim().parse().ok()).unwrap_or(128),
        dynamic_slots: matches!(g.get("android_dynamic_slots").as_deref().map(str::trim), Some("on" | "1" | "true" | "yes")),
        facts,
        handles,
    };
    let out = game_dir.join("shaders.dmsv");
    let rep = nx_shader::pipeline::build(&index, &params, &out, progress)?;
    let _ = std::fs::remove_dir_all(&work);
    if !rep.failed.is_empty() {
        let first: Vec<String> = rep.failed.iter().take(3).map(|(k, e)| format!("{k:016x}: {e}")).collect();
        let mut te: Vec<(&String, &usize)> = rep.translator_errors.iter().collect();
        te.sort_by(|a, b| b.1.cmp(a.1));
        let why: Vec<String> = te.iter().take(4).map(|(l, n)| format!("{n}x {l}")).collect();
        return Err(format!("{} of {} shaders failed; {}{}", rep.failed.len(), rep.expected.len(), first.join("; "), if why.is_empty() { String::new() } else { format!("; the translator said: {}", why.join(" | ")) }));
    }
    let cap = if captured > 0 { format!(", {captured} captured while playing") } else { String::new() };
    let bad = if cap_notes.is_empty() { String::new() } else { format!("; [RED] {}", cap_notes.iter().take(3).cloned().collect::<Vec<_>>().join("; ")) };
    Ok(format!("{} shaders ({} unreadable files{cap}{pack_note}){bad}", rep.emitted, skipped.len()))
}

/// Builds `shaders_live.dmsv` of the game's data folder from the shaders the game captured (captured_shaders/) alone, while the game runs: the host opens it beside
/// `shaders.dmsv` and looks a shader up in it when the bundle lacks one (gfx_vk_shader.c, live bundle), so a shader no game file holds is drawn within seconds of the
/// first time the game sets it instead of after the next port. Returns how many shaders it holds.
pub fn build_live(profiles: &Path, id: &str, game_dir: &Path, xlate: Vec<String>, threads: usize) -> Result<usize, String> {
    use nx_shader::json::Json;
    let dir = find_profile(profiles, id)?;
    let g = nx_profile::GameProfile::load(&dir).map_err(|e| format!("{id}: {e}"))?;
    let mut facts = std::collections::BTreeMap::new();
    if let Ok(t) = std::fs::read_to_string(dir.join("shader_facts.tsv")) {
        for l in t.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            let (k, j) = l.split_once('\t').ok_or("shader_facts.tsv: bad row")?;
            facts.insert(k.to_string(), Json::parse(j).map_err(|e| format!("shader_facts.tsv {k}: {e}"))?);
        }
    }
    let mut handles = std::collections::BTreeMap::new();
    if let Ok(t) = std::fs::read_to_string(dir.join("android_shader_handles.tsv")) {
        let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|_| format!("android_shader_handles.tsv: bad number {s}"));
        for l in t.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 4 {
                return Err("android_shader_handles.tsv: bad row".into());
            }
            handles.insert(f[0].to_string(), (hex(f[1])?, hex(f[2])?, f[3].parse::<usize>().map_err(|_| "android_shader_handles.tsv: bad site count")?));
        }
    }
    let work = game_dir.join("shaders.live.work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let spool = work.join("spool.bin");
    std::fs::write(&spool, b"").map_err(|e| e.to_string())?;
    let mut index = nx_shader::scan::Index { spool, ents: Default::default() };
    let (n, _notes) = index.add_captures(&game_dir.join("captured_shaders"))?;
    if n == 0 {
        let _ = std::fs::remove_dir_all(&work);
        return Ok(0);
    }
    let params = nx_shader::pipeline::Params {
        xlate,
        work: work.join("xlate"),
        threads,
        chunk: 64,
        subgroup: g.get("android_subgroup_size").and_then(|v| v.trim().parse().ok()).unwrap_or(128),
        dynamic_slots: matches!(g.get("android_dynamic_slots").as_deref().map(str::trim), Some("on" | "1" | "true" | "yes")),
        facts,
        handles,
    };
    let tmp = game_dir.join("shaders_live.dmsv.part");
    let rep = nx_shader::pipeline::build(&index, &params, &tmp, &|_, _, _| {});
    let _ = std::fs::remove_dir_all(&work);
    let rep = rep?;
    if rep.emitted == 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("none of {n} captured shaders translated ({} failed)", rep.failed.len()));
    }
    std::fs::rename(&tmp, game_dir.join("shaders_live.dmsv")).map_err(|e| e.to_string())?;
    Ok(rep.emitted)
}

/// The games list as JSON (`{"games":[...]}`) or `{"error":"..."}`. `overlay` is the text of upstream's `overlay_default.tsv`, when the controls should be validated.
pub fn games_json(profiles_dir: &str, overlay: &str, all: bool) -> String {
    let controls = (!overlay.is_empty()).then(|| nx_profile::overlay_controls(overlay));
    match nx_profile::catalog(Path::new(profiles_dir), controls.as_ref(), !all) {
        Ok(e) => format!("{{\"games\":{}}}", nx_profile::to_json(&e)),
        Err(m) => format!("{{\"error\":{}}}", json_str(&m)),
    }
}

/// Which keys a `prod.keys` holds: `{"keys":N,"master_keys":N,"header_key":bool}` or an error.
pub fn keys_json(path: &str) -> String {
    match nx_keys::Keys::load(Path::new(path)) {
        Ok(k) => format!("{{\"keys\":{},\"master_keys\":{},\"header_key\":{}}}", k.len(), k.master_keys(), k.header_key().is_some()),
        Err(e) => format!("{{\"error\":{}}}", json_str(&format!("{path}: {e}"))),
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExtractRequest {
    pub keys_path: String,
    /// where to read the dump (may be `/proc/self/fd/N`) and its real file name
    pub base_path: String,
    pub base_name: String,
    pub update_path: String,
    pub update_name: String,
    pub out_dir: String,
    pub tmp_dir: String,
    /// add-on content dumps (path, file name): each one's RomFS goes to `<out_dir>/aoc/<index>/`, index = its title ID − the game's − 0x1000
    pub dlc: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone)]
struct Status {
    state: State,
    stage: String,
    message: String,
    title_id: u64,
    name: String,
    display_version: String,
    exefs_files: usize,
    romfs_files: usize,
    romfs_bytes: u64,
}

struct Job {
    status: Mutex<Status>,
    bytes: AtomicU64,
    /// the size of the dumps being read, what `bytes` counts against
    total: AtomicU64,
    cancel: std::sync::atomic::AtomicBool,
}

/// The payload of the panic that stops an extraction a player cancelled
struct Cancelled;

fn jobs() -> &'static Mutex<HashMap<u32, Arc<Job>>> {
    static J: OnceLock<Mutex<HashMap<u32, Arc<Job>>>> = OnceLock::new();
    J.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set(job: &Job, f: impl FnOnce(&mut Status)) {
    if let Ok(mut s) = job.status.lock() {
        f(&mut s);
    }
}

fn run(job: &Arc<Job>, r: &ExtractRequest) -> Result<nx_extract::Report, String> {
    set(job, |s| s.stage = "Reading keys".into());
    let keys = nx_keys::Keys::load(Path::new(&r.keys_path)).map_err(|e| format!("prod.keys: {e}"))?;
    if keys.header_key().is_none() {
        return Err("prod.keys has no header_key".into());
    }
    let j = job.clone();
    let progress: nx_extract::Progress = Arc::new(move |n: u64| {
        if j.cancel.load(Ordering::Relaxed) {
            std::panic::panic_any(Cancelled);
        }
        j.bytes.fetch_add(n, Ordering::Relaxed);
    });
    set(job, |s| s.stage = "Opening the game".into());
    let base: Opened = nx_extract::open_counted(Path::new(&r.base_path), &r.base_name, &keys, Some(progress.clone())).map_err(|e| e.to_string())?;
    job.total.fetch_add(base.total, Ordering::Relaxed);
    let upd = if r.update_path.is_empty() {
        None
    } else {
        set(job, |s| s.stage = "Opening the update".into());
        {
            let u = nx_extract::open_counted(Path::new(&r.update_path), &r.update_name, &keys, Some(progress.clone())).map_err(|e| e.to_string())?;
            job.total.fetch_add(u.total, Ordering::Relaxed);
            Some(u)
        }
    };
    set(job, |s| s.stage = "Extracting".into());
    let res = nx_extract::extract_game(base.package, upd.map(|u| u.package), &keys, Path::new(&r.out_dir)).map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&r.tmp_dir); // decompressed copies, if any
    let rep = res?;
    for (i, (path, name)) in r.dlc.iter().enumerate() {
        set(job, |s| s.stage = format!("Add-on content {} of {}", i + 1, r.dlc.len()));
        add_dlc(&keys, Path::new(&r.out_dir), rep.title_id, path, name, i, job, &progress)?;
    }
    Ok(rep)
}

/// One add-on content dump into `<out>/aoc/<index>/`. A DLC that belongs to another game is an error: the host would never mount it.
fn add_dlc(keys: &nx_keys::Keys, out: &Path, app: u64, path: &str, name: &str, n: usize, job: &Job, progress: &nx_extract::Progress) -> Result<(), String> {
    let pkg: Opened = nx_extract::open_counted(Path::new(path), name, keys, Some(progress.clone())).map_err(|e| e.to_string())?;
    job.total.fetch_add(pkg.total, Ordering::Relaxed);
    let work = out.join("aoc").join(format!(".part{n}"));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let (tid, _) = nx_extract::extract_aoc(pkg.package, keys, &work).map_err(|e| format!("{name}: {e}"))?;
    let Some(idx) = tid.checked_sub(app).and_then(|d| d.checked_sub(0x1000)).filter(|i| *i < 0x1000) else {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("{name} is add-on content of {tid:016x}, not of {app:016x}"));
    };
    let dst = out.join("aoc").join(idx.to_string());
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::rename(&work, &dst).map_err(|e| format!("{name}: {e}"))
}

/// Starts extracting a game on a background thread; returns the job number.
pub fn start_extract(r: ExtractRequest) -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let job = Arc::new(Job {
        status: Mutex::new(Status { state: State::Running, stage: "Starting".into(), message: String::new(), title_id: 0, name: String::new(), display_version: String::new(), exefs_files: 0, romfs_files: 0, romfs_bytes: 0 }),
        bytes: AtomicU64::new(0),
        total: AtomicU64::new(0),
        cancel: std::sync::atomic::AtomicBool::new(false),
    });
    if let Ok(mut m) = jobs().lock() {
        m.insert(id, job.clone());
    }
    std::thread::spawn(move || match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&job, &r))).unwrap_or_else(|p| Err(if p.is::<Cancelled>() { "cancelled".to_string() } else { "the extraction stopped unexpectedly".to_string() })) {
        Ok(rep) => set(&job, |s| {
            s.state = State::Done;
            s.stage = "Done".into();
            s.title_id = rep.title_id;
            s.name = rep.name;
            s.display_version = rep.display_version;
            s.exefs_files = rep.exefs_files;
            s.romfs_files = rep.romfs_files;
            s.romfs_bytes = rep.romfs_bytes;
        }),
        Err(m) => set(&job, |s| {
            s.state = State::Failed;
            s.message = m;
        }),
    });
    id
}

/// Stops a job at its next read of the dump; its state becomes failed with the message `cancelled`.
pub fn cancel(id: u32) {
    if let Some(job) = jobs().lock().ok().and_then(|m| m.get(&id).cloned()) {
        job.cancel.store(true, Ordering::Relaxed);
    }
}

/// The state of a job as JSON.
pub fn status_json(id: u32) -> String {
    let job = jobs().lock().ok().and_then(|m| m.get(&id).cloned());
    let Some(job) = job else { return format!("{{\"error\":{}}}", json_str("no such job")) };
    let s = job.status.lock().map(|s| s.clone());
    let Ok(s) = s else { return "{\"error\":\"status unavailable\"}".into() };
    format!(
        "{{\"id\":{id},\"state\":\"{}\",\"stage\":{},\"message\":{},\"bytes\":{},\"total\":{},\"title_id\":\"{:016x}\",\"name\":{},\"display_version\":{},\"exefs_files\":{},\"romfs_files\":{},\"romfs_bytes\":{}}}",
        match s.state {
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
        },
        json_str(&s.stage),
        json_str(&s.message),
        job.bytes.load(Ordering::Relaxed),
        job.total.load(Ordering::Relaxed),
        s.title_id,
        json_str(&s.name),
        json_str(&s.display_version),
        s.exefs_files,
        s.romfs_files,
        s.romfs_bytes
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn switch_files_follow_the_profile() {
        let root = std::env::temp_dir().join(format!("nxc-sw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("0100ABCD00000000")).unwrap();
        std::fs::write(root.join("0100ABCD00000000/android.tsv"), "status\tplayable\nenv\tDM_NVN_ASYNC=1\nsetting\tdocked=0\n").unwrap();
        let out = root.join("data");
        super::write_switch_files(&root, "0100abcd00000000", &out, "en-US").unwrap();
        let env = std::fs::read_to_string(out.join("env.txt")).unwrap();
        assert!(env.contains("DM_LANG=en-US\nDM_NVN_ASYNC=1\n"));
        assert_eq!(std::fs::read_to_string(out.join("settings.txt")).unwrap(), "docked=0\n");
        std::fs::write(out.join("settings.txt"), "docked=1\n").unwrap(); // the player's choice stays
        super::write_switch_files(&root, "0100ABCD00000000", &out, "ja").unwrap();
        assert_eq!(std::fs::read_to_string(out.join("settings.txt")).unwrap(), "docked=1\n");
        assert!(std::fs::read_to_string(out.join("env.txt")).unwrap().contains("DM_LANG=ja"));
        assert!(super::write_switch_files(&root, "0100FFFF00000000", &out, "x").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
    use super::*;

    #[test]
    fn modules_are_rewritten_into_mods() {
        let root = std::env::temp_dir().join(format!("nxcore-rw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let prof = root.join("games/0100abcd00000000");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(prof.join("game.tsv"), "id\t0100ABCD00000000\nmodules\tmain sdk\n").unwrap();
        let game = root.join("data");
        std::fs::create_dir_all(game.join("ExeFS")).unwrap();
        std::fs::write(game.join("ExeFS/main"), nx_rewrite::testing::fake_nso(&[0xAA00_03F2, 0xD65F_03C0])).unwrap();
        let msg = super::rewrite_modules(&root.join("games"), "", "", "0100ABCD00000000", &game).unwrap();
        assert!(msg.starts_with("main: 1 sites"), "{msg}");
        for e in ["patched.bin", "fixups.tsv", "gates.tsv"] {
            assert!(game.join("mods").join(format!("main.{e}")).is_file(), "{e}");
        }
        assert!(!game.join("mods.tmp").exists());
        std::fs::remove_dir_all(game.join("ExeFS/main")).ok();
        std::fs::remove_file(game.join("ExeFS/main")).unwrap();
        assert!(super::rewrite_modules(&root.join("games"), "", "", "0100ABCD00000000", &game).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn json_escaping() {
        assert_eq!(json_str("a\"b\\c\n\t\u{1}é"), "\"a\\\"b\\\\c\\n\\t\\u0001é\"");
    }

    #[test]
    fn errors_come_back_as_json() {
        assert!(games_json("/nonexistent/dir", "", false).starts_with("{\"error\":"));
        assert!(keys_json("/nonexistent/prod.keys").starts_with("{\"error\":"));
        assert!(status_json(987654).contains("no such job"));
    }

    fn wait(id: u32) -> String {
        for _ in 0..500 {
            let s = status_json(id);
            if !s.contains("\"state\":\"running\"") {
                return s;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("job did not finish");
    }

    #[test]
    fn a_job_with_missing_files_fails_with_a_message() {
        let id = start_extract(ExtractRequest { keys_path: "/nonexistent/prod.keys".into(), base_path: "/x".into(), base_name: "x.nsp".into(), out_dir: "/tmp/nx-core-x".into(), tmp_dir: "/tmp/nx-core-t".into(), ..Default::default() });
        let s = wait(id);
        assert!(s.contains("\"state\":\"failed\"") && s.contains("prod.keys"), "{s}");
    }

    #[test]
    fn a_job_on_a_junk_dump_fails_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("nx-core-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let keys = dir.join("prod.keys");
        std::fs::write(&keys, format!("header_key = {}\n", "ab".repeat(32))).unwrap();
        let junk = dir.join("junk.nsp");
        std::fs::write(&junk, vec![0u8; 100]).unwrap();
        let id = start_extract(ExtractRequest {
            keys_path: keys.display().to_string(),
            base_path: junk.display().to_string(),
            base_name: "junk.nsp".into(),
            out_dir: dir.join("out").display().to_string(),
            tmp_dir: dir.join("tmp").display().to_string(),
            ..Default::default()
        });
        let s = wait(id);
        assert!(s.contains("\"state\":\"failed\""), "{s}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The build ID (32 hex digits) of the game's main module, which names its FPSLocker config.
pub fn fps_build_id(game_dir: &Path) -> Result<String, String> {
    let bytes = std::fs::read(game_dir.join("ExeFS/main")).map_err(|e| format!("ExeFS/main: {e}"))?;
    Ok(nx_nso::Nso::parse(&bytes).map_err(|e| format!("main: {e}"))?.build_id_hex())
}

/// Converts the FPSLocker config text of the game's main build into fpslock.tsv (the host's rows) and extra_cuts.tsv (the rewriter's cuts) in the game's data folder.
/// Err carries what the host cannot apply, which the port tells the player.
pub fn fps_convert(game_dir: &Path, title_id: &str, text: &str, source: &str) -> Result<String, String> {
    let bytes = std::fs::read(game_dir.join("ExeFS/main")).map_err(|e| format!("ExeFS/main: {e}"))?;
    let nso = nx_nso::Nso::parse(&bytes).map_err(|e| format!("main: {e}"))?;
    let image = nso.image(&bytes).map_err(|e| format!("main: {e}"))?;
    let bid = nso.build_id_hex();
    let _ = title_id;
    let (tsv, cuts) = nx_fps::convert(text, &image, &bid, source).map_err(|e| e.0)?;
    std::fs::write(game_dir.join("fpslock.tsv"), tsv).map_err(|e| e.to_string())?;
    std::fs::write(game_dir.join("extra_cuts.tsv"), nx_fps::cuts_tsv(&bid, &cuts)).map_err(|e| e.to_string())?;
    Ok(format!("{} memory write(s) at run time, {} code byte change(s)", std::fs::read_to_string(game_dir.join("fpslock.tsv")).map(|t| t.lines().filter(|l| l.starts_with("ALL_FPS")).count()).unwrap_or(0), cuts.len()))
}

pub fn fps_url(title_id: &str, build_id: &str) -> String {
    nx_fps::url(title_id, build_id)
}

/// The title ID (16 hex digits) of a dump, from its tickets or content meta headers, else a `[title ID]` in its name; "" when there is none. `path` is where the bytes can be read, `name` the file's own name.
pub fn dump_title(keys_path: &str, path: &str, name: &str) -> String {
    let keys = nx_keys::Keys::load(Path::new(keys_path)).ok();
    let Ok(f) = nx_nsz::PosFile::open(path) else { return String::new() };
    match nx_nca::title_of(&mut std::io::BufReader::new(f), Path::new(name), keys.as_ref()) {
        Ok(Some(t)) => format!("{:016X}", t.id),
        _ => String::new(),
    }
}

#[cfg(test)]
mod title_tests {
    use super::dump_title;

    fn pfs0(files: &[(&str, &[u8])]) -> Vec<u8> {
        let strs: Vec<u8> = files.iter().flat_map(|(n, _)| n.bytes().chain([0])).collect();
        let mut out = b"PFS0".to_vec();
        out.extend((files.len() as u32).to_le_bytes());
        out.extend((strs.len() as u32).to_le_bytes());
        out.extend(0u32.to_le_bytes());
        let (mut off, mut so) = (0u64, 0u32);
        for (n, d) in files {
            out.extend(off.to_le_bytes());
            out.extend((d.len() as u64).to_le_bytes());
            out.extend(so.to_le_bytes());
            out.extend(0u32.to_le_bytes());
            off += d.len() as u64;
            so += n.len() as u32 + 1;
        }
        out.extend(strs);
        for (_, d) in files {
            out.extend(*d);
        }
        out
    }

    #[test]
    fn a_dump_without_a_title_in_its_name_is_found_by_its_ticket() {
        let dir = std::env::temp_dir().join(format!("nx2a-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("Some Game.nsp");
        // a ticket is named by the rights ID: the title ID, then the key generation
        std::fs::write(&f, pfs0(&[("0100f43008c44800000000000000000a.tik", b"x"), ("a.nca", b"y")])).unwrap();
        assert_eq!(dump_title("/nonexistent/prod.keys", f.to_str().unwrap(), "Some Game.nsp"), "0100F43008C44800");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
