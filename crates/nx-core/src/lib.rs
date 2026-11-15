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
    let text = std::fs::read_to_string(dir.join("android.tsv")).map_err(|e| format!("{id}: no android.tsv ({e})"))?;
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
}

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
    let mut progress = |n: u64| {
        job.bytes.fetch_add(n, Ordering::Relaxed);
    };
    set(job, |s| s.stage = "Opening the game".into());
    let base: Opened = nx_extract::open_path(Path::new(&r.base_path), &r.base_name, &keys, Path::new(&r.tmp_dir), &mut progress).map_err(|e| e.to_string())?;
    let upd = if r.update_path.is_empty() {
        None
    } else {
        set(job, |s| s.stage = "Opening the update".into());
        Some(nx_extract::open_path(Path::new(&r.update_path), &r.update_name, &keys, Path::new(&r.tmp_dir), &mut progress).map_err(|e| e.to_string())?)
    };
    set(job, |s| s.stage = "Extracting".into());
    let res = nx_extract::extract_game(base.package, upd.map(|u| u.package), &keys, Path::new(&r.out_dir)).map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&r.tmp_dir); // decompressed copies, if any
    res
}

/// Starts extracting a game on a background thread; returns the job number.
pub fn start_extract(r: ExtractRequest) -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let job = Arc::new(Job {
        status: Mutex::new(Status { state: State::Running, stage: "Starting".into(), message: String::new(), title_id: 0, name: String::new(), display_version: String::new(), exefs_files: 0, romfs_files: 0, romfs_bytes: 0 }),
        bytes: AtomicU64::new(0),
    });
    if let Ok(mut m) = jobs().lock() {
        m.insert(id, job.clone());
    }
    std::thread::spawn(move || match run(&job, &r) {
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

/// The state of a job as JSON.
pub fn status_json(id: u32) -> String {
    let job = jobs().lock().ok().and_then(|m| m.get(&id).cloned());
    let Some(job) = job else { return format!("{{\"error\":{}}}", json_str("no such job")) };
    let s = job.status.lock().map(|s| s.clone());
    let Ok(s) = s else { return "{\"error\":\"status unavailable\"}".into() };
    format!(
        "{{\"id\":{id},\"state\":\"{}\",\"stage\":{},\"message\":{},\"bytes\":{},\"title_id\":\"{:016x}\",\"name\":{},\"display_version\":{},\"exefs_files\":{},\"romfs_files\":{},\"romfs_bytes\":{}}}",
        match s.state {
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
        },
        json_str(&s.stage),
        json_str(&s.message),
        job.bytes.load(Ordering::Relaxed),
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
