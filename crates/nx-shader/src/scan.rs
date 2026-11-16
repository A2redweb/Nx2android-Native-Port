//! Scans a game's files for shaders and keeps them on disk: a spool file holds each distinct shader's control section and code area; memory holds only the index.

use crate::split::{scan_file, skey, Found, Stage};
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
pub fn scan_dir(roots: &[(PathBuf, String)], spool: &Path, threads: usize, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<(Index, Vec<(String, String)>), String> {
    let mut files: Vec<(PathBuf, String)> = vec![];
    for (root, prefix) in roots {
        let mut v = vec![];
        walk(root, &mut v).map_err(|e| format!("{}: {e}", root.display()))?;
        for p in v {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            files.push((p, format!("{prefix}/{rel}")));
        }
    }
    let total = files.len();
    let next = Arc::new(AtomicUsize::new(0));
    let files = Arc::new(files);
    let (tx, rx) = mpsc::channel::<(usize, Result<Vec<Found>, String>)>();
    let mut merger = Merger { w: BufWriter::new(File::create(spool).map_err(|e| e.to_string())?), pos: 0, ents: BTreeMap::new() };
    let mut skipped = vec![];
    std::thread::scope(|sc| -> Result<(), String> {
        for _ in 0..threads.max(1) {
            let (tx, next, files) = (tx.clone(), next.clone(), files.clone());
            sc.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some((path, label)) = files.get(i) else { break };
                let r = std::fs::read(path).map_err(|e| e.to_string()).and_then(|d| {
                    let mut out = vec![];
                    scan_file(&d, label, &mut out).map(|_| out)
                });
                if tx.send((i, r)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        let mut pending: HashMap<usize, Result<Vec<Found>, String>> = HashMap::new();
        let mut want = 0;
        while let Ok((i, r)) = rx.recv() {
            pending.insert(i, r);
            while let Some(r) = pending.remove(&want) {
                match r {
                    Ok(found) => {
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
        let (idx, skipped) = scan_dir(&[(root.clone(), "".into())], &spool, 3, &|_, _| {}).unwrap();
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(idx.ents.len(), 2);
        let k = skey(&vctl, &vcode);
        assert_eq!(idx.ents[&k].label, "/a.bfsha#BNSH@0x0#var0");
        let (c, d) = idx.load(k).unwrap();
        assert_eq!((c, d), (vctl, vcode));
        let _ = std::fs::remove_dir_all(&root);
    }
}
