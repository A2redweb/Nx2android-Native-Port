//! Scans a game's files for shaders and keeps them on disk: a spool file holds each distinct shader's control section and code area; memory holds only the index.

use crate::split::{scan_file_with, skey, Found, Stage, UnityState};
use crate::unity::Trees;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

#[derive(Clone, Debug)]
pub struct Ent {
    pub stage: Stage,
    /// where it was first found
    pub label: String,
    pub off: u64,
    pub ctl_len: u32,
    pub code_len: u32,
}

pub struct Index {
    pub spool: PathBuf,
    pub ents: BTreeMap<u64, Ent>,
}

impl Index {
    /// Adds the shaders the game captured while it ran (`<dir>/<key>.bin`: "DMSC", u32 control length, u32 code length, control, code; gfx_vk_shader.c vk_shader_missing) that the index
    /// does not hold. Returns how many were added and a line for each file that was left out and why.
    pub fn add_captures(&mut self, dir: &Path) -> Result<(usize, Vec<String>), String> {
        use crate::split::{check, glslc_stage, skey};
        let mut names: Vec<_> = match std::fs::read_dir(dir) { Ok(r) => r.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect(), Err(_) => return Ok((0, vec![])) };
        names.sort();
        let (mut added, mut notes) = (0, vec![]);
        let mut w = std::fs::OpenOptions::new().append(true).open(&self.spool).map_err(|e| e.to_string())?;
        let mut pos = w.metadata().map_err(|e| e.to_string())?.len();
        for nm in names {
            let stem = match nm.strip_suffix(".bin") { Some(s) if s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit()) => s.to_ascii_lowercase(), _ => continue };
            let Ok(want) = u64::from_str_radix(&stem, 16) else { continue };
            let b = std::fs::read(dir.join(&nm)).map_err(|e| format!("{nm}: {e}"))?;
            if b.len() < 12 || &b[..4] != b"DMSC" { notes.push(format!("capture {nm}: not a captured shader")); continue; }
            let (nctl, ncode) = (u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize, u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize);
            if b.len() != 12 + nctl + ncode { notes.push(format!("capture {nm}: truncated")); continue; }
            let (ctl, code) = (&b[12..12 + nctl], &b[12 + nctl..]);
            if skey(ctl, code) != want { notes.push(format!("capture {nm}: its content does not hash to its name")); continue; }
            if self.ents.contains_key(&want) { continue; }
            let Some(st) = glslc_stage(ctl, code) else { notes.push(format!("capture {nm}: cannot determine its shader stage")); continue };
            if let Some(why) = check(st, ctl, code) { notes.push(format!("capture {nm}: {why}")); continue; }
            w.write_all(ctl).and_then(|_| w.write_all(code)).map_err(|e| e.to_string())?;
            self.ents.insert(want, Ent { stage: st, label: format!("captured/{nm}"), off: pos, ctl_len: nctl as u32, code_len: ncode as u32 });
            pos += (nctl + ncode) as u64;
            added += 1;
        }
        Ok((added, notes))
    }

    /// Opens the spool for appending shaders found after the scan (the packs' members, decoded by the game's own decoder)
    pub fn appender(&mut self) -> Result<Appender<'_>, String> {
        let w = std::fs::OpenOptions::new().append(true).open(&self.spool).map_err(|e| e.to_string())?;
        let pos = w.metadata().map_err(|e| e.to_string())?.len();
        Ok(Appender { idx: self, w: BufWriter::new(w), pos, added: 0 })
    }

    pub fn load(&self, key: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
        let e = self.ents.get(&key).ok_or_else(|| format!("no shader {key:016x}"))?;
        let mut f = File::open(&self.spool).map_err(|x| x.to_string())?;
        f.seek(SeekFrom::Start(e.off)).map_err(|x| x.to_string())?;
        let (mut ctl, mut code) = (vec![0u8; e.ctl_len as usize], vec![0u8; e.code_len as usize]);
        f.read_exact(&mut ctl).map_err(|x| x.to_string())?;
        f.read_exact(&mut code).map_err(|x| x.to_string())?;
        Ok((ctl, code))
    }
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut kids: Vec<_> = std::fs::read_dir(root)?.filter_map(Result::ok).map(|e| e.path()).collect();
    kids.sort();
    for p in kids {
        if p.is_dir() {
            walk(&p, out)?;
        } else {
            out.push(p);
        }
    }
    Ok(())
}

struct Merger {
    w: BufWriter<File>,
    pos: u64,
    ents: BTreeMap<u64, Ent>,
}

impl Merger {
    fn add(&mut self, f: Found) -> Result<(), String> {
        let key = skey(&f.ctl, &f.code);
        if let Some(old) = self.ents.get(&key) {
            // the same key must be the same shader: compare with what is on disk only when the lengths differ (the content hash already covers code and the key's control bytes)
            if old.code_len as usize != f.code.len() {
                return Err(format!("key {key:016x} is the same but the code area differs ({} / {})", f.label, old.label));
            }
            return Ok(());
        }
        self.w.write_all(&f.ctl).and_then(|_| self.w.write_all(&f.code)).map_err(|e| e.to_string())?;
        self.ents.insert(key, Ent { stage: f.stage, label: f.label, off: self.pos, ctl_len: f.ctl.len() as u32, code_len: f.code.len() as u32 });
        self.pos += (f.ctl.len() + f.code.len()) as u64;
        Ok(())
    }
}

/// Scans every file under `root` (labels are paths relative to it, `/`-separated, prefixed by `prefix`) on `threads` threads. Results are merged in file order, so the same input gives the same index.
/// A file that fails to read as a container is reported in `skipped` and left out; a shader that contradicts an earlier one with its key stops the scan.
pub fn scan_dir(roots: &[(PathBuf, String)], common: Option<HashMap<u32, String>>, dicts: crate::container::Dicts, spool: &Path, threads: usize, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<(Index, Vec<(String, String)>), String> {
    let common = common.map(Arc::new);
    let mut files: Vec<(PathBuf, String)> = vec![];
    for (root, prefix) in roots {
        let mut v = vec![];
        walk(root, &mut v).map_err(|e| format!("{}: {e}", root.display()))?;
        for p in v {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if rel.ends_with(".trpfs") {
                continue; // Trinity packs are read member by member (pack.rs), not as one file
            }
            files.push((p, format!("{prefix}/{rel}")));
        }
    }
    let total = files.len();
    let next = Arc::new(AtomicUsize::new(0));
    let files = Arc::new(files);
    type Scanned = Result<(Vec<Found>, Trees, bool), String>;
    let (tx, rx) = mpsc::channel::<(usize, Scanned)>();
    let mut merger = Merger { w: BufWriter::new(File::create(spool).map_err(|e| e.to_string())?), pos: 0, ents: BTreeMap::new() };
    let mut skipped = vec![];
    std::thread::scope(|sc| -> Result<(), String> {
        for _ in 0..threads.max(1) {
            let (tx, next, files, common, dicts) = (tx.clone(), next.clone(), files.clone(), common.clone(), dicts.clone());
            sc.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some((path, label)) = files.get(i) else { break };
                let r = crate::container::load_file(path).and_then(|d| {
                    let (mut out, mut u) = (vec![], UnityState { common: common.clone(), trees: Trees::new(), borrowed: false, dicts: dicts.clone() });
                    scan_file_with(&d, label, &mut out, &mut u).map(|defer| (out, u.trees, defer))
                });
                if tx.send((i, r)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        let mut pending: HashMap<usize, Scanned> = HashMap::new();
        let (mut trees, mut deferred): (Trees, Vec<usize>) = (Trees::new(), vec![]);
        let mut want = 0;
        while let Ok((i, r)) = rx.recv() {
            pending.insert(i, r);
            while let Some(r) = pending.remove(&want) {
                match r {
                    Ok((found, t, defer)) => {
                        trees.extend(t);
                        if defer {
                            deferred.push(want);
                        }
                        for f in found {
                            merger.add(f)?;
                        }
                    }
                    Err(e) => skipped.push((files[want].1.clone(), e)),
                }
                want += 1;
                progress(want, total);
            }
        }
        // second pass: Unity files that had no type tree of their own, with the trees the other files recorded and the layouts upstream keeps
        if !deferred.is_empty() {
            for (k, v) in crate::unity::known_trees()? {
                trees.entry(k).or_insert(v);
            }
            let next = AtomicUsize::new(0);
            let results: Vec<std::sync::Mutex<Option<Scanned>>> = deferred.iter().map(|_| std::sync::Mutex::new(None)).collect();
            std::thread::scope(|sc2| {
                for _ in 0..threads.max(1).min(deferred.len()) {
                    sc2.spawn(|| loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&fi) = deferred.get(i) else { break };
                        let (path, label) = &files[fi];
                        let r = crate::container::load_file(path).and_then(|d| {
                            let (mut out, mut u) = (vec![], UnityState { common: common.clone(), trees: trees.clone(), borrowed: true, dicts: dicts.clone() });
                            scan_file_with(&d, label, &mut out, &mut u).map(|defer| (out, u.trees, defer))
                        });
                        *results[i].lock().unwrap() = Some(r);
                    });
                }
            });
            for (i, r) in results.into_iter().enumerate() {
                match r.into_inner().unwrap().unwrap() {
                    Ok((found, _, _)) => {
                        for f in found {
                            merger.add(f)?;
                        }
                    }
                    Err(e) => skipped.push((files[deferred[i]].1.clone(), e)),
                }
            }
        }
        Ok(())
    })?;
    merger.w.flush().map_err(|e| e.to_string())?;
    Ok((Index { spool: spool.to_path_buf(), ents: merger.ents }, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_a_folder_in_order_and_keeps_shaders_on_disk() {
        let root = std::env::temp_dir().join(format!("nxscan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let (bnsh, vctl, vcode, _) = crate::split::tests_fake_bnsh();
        std::fs::write(root.join("a.bfsha"), &bnsh).unwrap();
        std::fs::write(root.join("sub/copy.bfsha"), &bnsh).unwrap(); // the same shaders again: one entry each
        std::fs::write(root.join("sub/other.bin"), b"nothing here").unwrap();
        let spool = root.join("spool");
        let (idx, skipped) = scan_dir(&[(root.clone(), "".into())], None, Default::default(), &spool, 3, &|_, _| {}).unwrap();
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(idx.ents.len(), 2);
        let k = skey(&vctl, &vcode);
        assert_eq!(idx.ents[&k].label, "/a.bfsha#BNSH@0x0#var0");
        let (c, d) = idx.load(k).unwrap();
        assert_eq!((c, d), (vctl, vcode));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_unity_file_without_trees_borrows_them_in_a_second_pass() {
        let root = std::env::temp_dir().join(format!("nxscan-u-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // 'a' sorts first and has no tree of its own, 'b' brings the tree
        std::fs::write(root.join("a.assets"), crate::unity::tests_serialized(false)).unwrap();
        std::fs::write(root.join("b.assets"), crate::unity::tests_serialized(true)).unwrap();
        let (idx, skipped) = scan_dir(&[(root.clone(), "".into())], Some(HashMap::new()), Default::default(), &root.join("spool"), 2, &|_, _| {}).unwrap();
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(idx.ents.len(), 2);
        assert!(idx.ents.values().all(|e| e.label.starts_with("/b.assets#")), "the keys come from the file read first, which is b: a was deferred");
        // without b nothing lends a tree: a is reported, not guessed at
        std::fs::remove_file(root.join("b.assets")).unwrap();
        let (idx, skipped) = scan_dir(&[(root.clone(), "".into())], Some(HashMap::new()), Default::default(), &root.join("spool"), 2, &|_, _| {}).unwrap();
        assert_eq!((idx.ents.len(), skipped.len()), (0, 1));
        let _ = std::fs::remove_dir_all(&root);
    }
}

pub struct Appender<'a> {
    idx: &'a mut Index,
    w: BufWriter<File>,
    pos: u64,
    pub added: usize,
}

impl Appender<'_> {
    pub fn add(&mut self, f: Found) -> Result<(), String> {
        let key = skey(&f.ctl, &f.code);
        if let Some(old) = self.idx.ents.get(&key) {
            if old.code_len as usize != f.code.len() {
                return Err(format!("key {key:016x} is the same but the code area differs ({} / {})", f.label, old.label));
            }
            return Ok(());
        }
        self.w.write_all(&f.ctl).and_then(|_| self.w.write_all(&f.code)).map_err(|e| e.to_string())?;
        self.idx.ents.insert(key, Ent { stage: f.stage, label: f.label, off: self.pos, ctl_len: f.ctl.len() as u32, code_len: f.code.len() as u32 });
        self.pos += (f.ctl.len() + f.code.len()) as u64;
        self.added += 1;
        Ok(())
    }

    pub fn finish(mut self) -> Result<usize, String> {
        self.w.flush().map_err(|e| e.to_string())?;
        Ok(self.added)
    }
}
