//! Game Freak's archives whose members the game's own Oodle decoder opens: Trinity packs (Pokémon Legends: Z-A, arc/data.trpfs) and GFPAKs (Pokémon Legends: Arceus).
//! The decoder is the game's own OodleLZ_Decompress, run by the host built as an executable (`DM_OODLE_SERVE=1`, dm/host/hle_oodle.c): a request is u64 compressed length, u64 raw
//! length and the bytes; the answer is u64 bytes decoded (0 = failed) and those bytes. Every member is decoded and scanned like a loose file.

use crate::container::{load_file, u32le, u64le};
use crate::scan::Appender;
use crate::split::{scan_blob, Found};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

// ---- FlatBuffers, just enough for the Trinity pack tables ----
fn fb_root(d: &[u8], at: usize) -> Result<usize, String> {
    Ok(at + u32le(d, at)? as usize)
}
fn fb_field(d: &[u8], t: usize, i: usize) -> Result<Option<usize>, String> {
    let so = u32le(d, t)? as i32 as i64;
    let vt = usize::try_from(t as i64 - so).map_err(|_| "bad vtable".to_string())?;
    let size = d.get(vt..vt + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap())).ok_or("vtable past the end")? as usize;
    if 4 + 2 * i >= size {
        return Ok(None);
    }
    let off = d.get(vt + 4 + 2 * i..vt + 6 + 2 * i).map(|b| u16::from_le_bytes(b.try_into().unwrap())).ok_or("vtable past the end")? as usize;
    Ok((off != 0).then_some(t + off))
}
fn fb_vec(d: &[u8], t: usize, i: usize) -> Result<(usize, usize), String> {
    match fb_field(d, t, i)? {
        None => Ok((0, 0)),
        Some(p) => {
            let p = p + u32le(d, p)? as usize;
            Ok((p + 4, u32le(d, p)? as usize))
        }
    }
}
fn fb_refs(d: &[u8], t: usize, i: usize) -> Result<Vec<usize>, String> {
    let (st, n) = fb_vec(d, t, i)?;
    (0..n).map(|k| Ok(st + 4 * k + u32le(d, st + 4 * k)? as usize)).collect()
}

/// One member of an archive: where its bytes are, whether they are Oodle-compressed, and its decoded length
#[derive(Debug)]
pub struct Member {
    pub label: String,
    pub start: usize,
    pub stored: usize,
    pub raw: usize,
    pub oodle: bool,
}

pub const ONEPACK: &[u8; 8] = b"ONEPACK\0";
pub const GFPAK: &[u8; 8] = b"GFLXPACK";

/// The members of a Trinity file system (`ONEPACK\0`, u64 offset of its root table {1: pack offsets}; a pack is {0: file hashes, 1: files {1: compression i8 (3 = Oodle), 3: length u64, 4: data}})
pub fn trinity_members(d: &[u8], rel: &str) -> Result<Vec<Member>, String> {
    if d.get(..8) != Some(&ONEPACK[..]) {
        return Err("not a Trinity file system".into());
    }
    let root = fb_root(d, u64le(d, 8)? as usize)?;
    let (st, n) = fb_vec(d, root, 1)?;
    let mut out = vec![];
    for p in 0..n {
        let off = u64le(d, st + 8 * p)? as usize;
        let pr = fb_root(d, off)?;
        let (hs, hn) = fb_vec(d, pr, 0)?;
        for (k, t) in fb_refs(d, pr, 1)?.into_iter().enumerate() {
            let comp = match fb_field(d, t, 1)? { Some(c) => *d.get(c).ok_or("read past the end")? as i8, None => 0 };
            let raw = match fb_field(d, t, 3)? { Some(l) => u64le(d, l)? as usize, None => 0 };
            let (dst, sz) = fb_vec(d, t, 4)?;
            if dst + sz > d.len() {
                return Err(format!("{rel}: file {k} of the pack at {off:#x} runs past the end"));
            }
            let h = if k < hn { u64le(d, hs + 8 * k)? } else { 0 };
            if comp != 3 && comp != 0 {
                continue; // a compression this reader does not know
            }
            out.push(Member { label: format!("{rel}#{off:x}#{h:016x}"), start: dst, stored: sz, raw, oodle: comp == 3 });
        }
    }
    Ok(out)
}

/// The members of a GFPAK (`GFLXPACK`, u32 count at +0x10, u64 file table at +0x18; 0x18 bytes each {u16 level, u16 compression, u32 length, u32 stored length, u32 0, u64 offset}); stored and Oodle members
pub fn gfpak_members(d: &[u8], rel: &str) -> Result<Vec<Member>, String> {
    if d.get(..8) != Some(&GFPAK[..]) {
        return Err("not a GFPAK".into());
    }
    let n = u32le(d, 0x10)? as usize;
    let files = u64le(d, 0x18)? as usize;
    let mut out = vec![];
    for i in 0..n {
        let e = files + 0x18 * i;
        let ct = u32le(d, e)? >> 16;
        let (usz, csz, off) = (u32le(d, e + 4)? as usize, u32le(d, e + 8)? as usize, u64le(d, e + 16)? as usize);
        if off + csz > d.len() {
            return Err(format!("{rel}: entry {i} runs past the end"));
        }
        if ct == 0 || ct == 3 {
            out.push(Member { label: format!("{rel}#{i}"), start: off, stored: csz, raw: usz, oodle: ct == 3 });
        }
    }
    Ok(out)
}

/// The game's decoder in a process of its own
pub struct Decoder {
    child: Child,
    to: ChildStdin,
    from: ChildStdout,
}

impl Decoder {
    pub fn start(exe: &Path, mods: &Path, cwd: &Path, game: &str, log: &Path) -> Result<Decoder, String> {
        let err = std::fs::File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
        let mut child = Command::new(exe).arg(mods).current_dir(cwd).env("DM_GAME", game).env("DM_OODLE_SERVE", "1")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(err).spawn().map_err(|e| format!("{}: {e}", exe.display()))?;
        let (to, from) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
        Ok(Decoder { child, to, from })
    }

    pub fn decode(&mut self, comp: &[u8], raw: usize) -> Result<Vec<u8>, String> {
        let mut req = Vec::with_capacity(16);
        req.extend_from_slice(&(comp.len() as u64).to_le_bytes());
        req.extend_from_slice(&(raw as u64).to_le_bytes());
        self.to.write_all(&req).and_then(|_| self.to.write_all(comp)).and_then(|_| self.to.flush()).map_err(|_| "the decoder ended".to_string())?;
        let mut n = [0u8; 8];
        self.from.read_exact(&mut n).map_err(|_| "the decoder ended".to_string())?;
        if u64::from_le_bytes(n) as usize != raw {
            return Err(format!("failed on {} → {raw} bytes", comp.len()));
        }
        let mut out = vec![0u8; raw];
        self.from.read_exact(&mut out).map_err(|_| "the decoder ended".to_string())?;
        Ok(out)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct HostSpec {
    pub exe: PathBuf,
    pub mods: PathBuf,
    pub cwd: PathBuf,
    pub game: String,
    pub log_dir: PathBuf,
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut kids: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
    kids.sort();
    for p in kids {
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// The archives under `romfs` this module reads: (path, label, members)
pub fn archives(romfs: &Path) -> Vec<(PathBuf, String)> {
    let mut all = vec![];
    walk(romfs, &mut all);
    all.into_iter()
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.ends_with(".trpfs") || n.ends_with(".gfpak")
        })
        .map(|p| {
            let rel = p.strip_prefix(romfs).unwrap().to_string_lossy().replace('\\', "/");
            (p, format!("/{rel}"))
        })
        .collect()
}

pub struct Outcome {
    pub members: usize,
    pub failed: usize,
    pub notes: Vec<String>,
}

/// Decodes every Oodle member of the archives and scans all members for shaders; what is found goes to `add`. `threads` decoder processes run at once.
pub fn scan_archives(list: &[(PathBuf, String)], host: &HostSpec, threads: usize, add: &mut Appender, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<Outcome, String> {
    let mut out = Outcome { members: 0, failed: 0, notes: vec![] };
    let _ = std::fs::create_dir_all(&host.log_dir);
    let mut done_total = 0usize;
    for (path, rel) in list {
        let d = load_file(path)?;
        let members = if d.get(..8) == Some(&ONEPACK[..]) {
            trinity_members(&d, rel)?
        } else if d.get(..8) == Some(&GFPAK[..]) {
            gfpak_members(&d, rel)?
        } else {
            continue;
        };
        if members.is_empty() {
            continue;
        }
        let total = members.len();
        let next = AtomicUsize::new(0);
        let (tx, rx) = mpsc::channel::<Result<(usize, Vec<Found>), String>>();
        let nthreads = threads.clamp(1, 4).min(total);
        let mut result: Result<(), String> = Ok(());
        std::thread::scope(|sc| {
            for t in 0..nthreads {
                let (tx, next, members, d) = (tx.clone(), &next, &members, &d);
                sc.spawn(move || {
                    let mut dec: Option<Decoder> = None;
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(m) = members.get(i) else { break };
                        let bytes = &d[m.start..m.start + m.stored];
                        let decoded;
                        let data: &[u8] = if m.oodle {
                            if dec.is_none() {
                                match Decoder::start(&host.exe, &host.mods, &host.cwd, &host.game, &host.log_dir.join(format!("decoder{t}.log"))) {
                                    Ok(x) => dec = Some(x),
                                    Err(e) => {
                                        let _ = tx.send(Err(e));
                                        return;
                                    }
                                }
                            }
                            match dec.as_mut().unwrap().decode(bytes, m.raw) {
                                Ok(v) => {
                                    decoded = v;
                                    &decoded
                                }
                                Err(e) if e == "the decoder ended" => {
                                    let _ = tx.send(Err(format!("{}: {e}", m.label)));
                                    return;
                                }
                                Err(e) => {
                                    let _ = tx.send(Err(format!("\0{}: {e}", m.label))); // a member that did not decode; the others go on
                                    let _ = tx.send(Ok((i, vec![])));
                                    continue;
                                }
                            }
                        } else {
                            bytes
                        };
                        let mut found = vec![];
                        if scan_blob(data, &m.label, &mut found).is_err() {
                            found.clear();
                        }
                        if tx.send(Ok((i, found))).is_err() {
                            return;
                        }
                    }
                });
            }
            drop(tx);
            let mut seen = 0usize;
            while let Ok(r) = rx.recv() {
                match r {
                    Ok((_, found)) => {
                        for f in found {
                            if let Err(e) = add.add(f) {
                                result = Err(e);
                                next.store(usize::MAX / 2, Ordering::Relaxed);
                            }
                        }
                        seen += 1;
                        if seen % 256 == 0 {
                            progress(done_total + seen, done_total + total);
                        }
                    }
                    Err(e) if e.starts_with('\0') => {
                        out.failed += 1;
                        if out.notes.len() < 3 {
                            out.notes.push(e[1..].to_string());
                        }
                    }
                    Err(e) => {
                        result = Err(e);
                        next.store(usize::MAX / 2, Ordering::Relaxed);
                    }
                }
            }
        });
        result?;
        out.members += total;
        done_total += total;
        progress(done_total, done_total);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gfpak_members_are_read() {
        let mut d = vec![0u8; 0x18 + 8];
        d[..8].copy_from_slice(GFPAK);
        d[0x10..0x14].copy_from_slice(&2u32.to_le_bytes());
        let files = d.len();
        d[0x18..0x20].copy_from_slice(&(files as u64).to_le_bytes());
        let data_at = files + 0x30;
        for (i, (ct, usz, csz)) in [(0u16, 4u32, 4u32), (3, 10, 6)].into_iter().enumerate() {
            let mut e = vec![0u8; 0x18];
            e[2..4].copy_from_slice(&ct.to_le_bytes());
            e[4..8].copy_from_slice(&usz.to_le_bytes());
            e[8..12].copy_from_slice(&csz.to_le_bytes());
            e[16..24].copy_from_slice(&((data_at + 8 * i) as u64).to_le_bytes());
            d.extend(e);
        }
        d.resize(data_at + 16, 0);
        let m = gfpak_members(&d, "/a.gfpak").unwrap();
        assert_eq!(m.len(), 2);
        assert!(!m[0].oodle && m[1].oodle && m[1].raw == 10 && m[1].stored == 6);
    }

    fn w32(b: &mut [u8], at: usize, v: u32) { b[at..at + 4].copy_from_slice(&v.to_le_bytes()); }
    fn w16(b: &mut [u8], at: usize, v: u16) { b[at..at + 2].copy_from_slice(&v.to_le_bytes()); }
    fn w64(b: &mut [u8], at: usize, v: u64) { b[at..at + 8].copy_from_slice(&v.to_le_bytes()); }

    #[test]
    fn trinity_members_are_read() {
        let mut b = vec![0u8; 200];
        b[..8].copy_from_slice(ONEPACK);
        w64(&mut b, 8, 16);
        // root offset @16, root table @20 {1: pack offsets}, vtable @28, the vector @36
        w32(&mut b, 16, 4);
        w32(&mut b, 20, (-8i32) as u32); w32(&mut b, 24, 36 - 24);
        for (i, v) in [8u16, 8, 0, 4].into_iter().enumerate() { w16(&mut b, 28 + 2 * i, v); }
        w32(&mut b, 36, 1); w64(&mut b, 40, 48);
        // the pack: root offset @48, table @56 {0 hashes, 1 files}, vtable @68
        w32(&mut b, 48, 8);
        w32(&mut b, 56, (-12i32) as u32); w32(&mut b, 60, 76 - 60); w32(&mut b, 64, 96 - 64);
        for (i, v) in [8u16, 12, 4, 8].into_iter().enumerate() { w16(&mut b, 68 + 2 * i, v); }
        w32(&mut b, 76, 2); w64(&mut b, 80, 0xAA); w64(&mut b, 88, 0xBB);
        w32(&mut b, 96, 2); w32(&mut b, 100, 108 - 100); w32(&mut b, 104, 152 - 104);
        for (t, vt, data, comp, len, bytes) in [(108usize, 128usize, 144usize, 3u8, 9u64, 3u32), (152, 172, 188, 0, 2, 2)] {
            w32(&mut b, t, (t as i32 - vt as i32) as u32);
            b[t + 4] = comp; w64(&mut b, t + 8, len); w32(&mut b, t + 16, (data - (t + 16)) as u32);
            for (i, v) in [14u16, 20, 0, 4, 0, 8, 16].into_iter().enumerate() { w16(&mut b, vt + 2 * i, v); }
            w32(&mut b, data, bytes);
        }
        let m = trinity_members(&b, "/arc/data.trpfs").unwrap();
        assert_eq!(m.len(), 2);
        assert!(m[0].oodle && m[0].raw == 9 && m[0].stored == 3 && m[0].start == 148 && m[0].label == "/arc/data.trpfs#30#00000000000000aa");
        assert!(!m[1].oodle && m[1].raw == 2 && m[1].stored == 2 && m[1].start == 192);
    }
}
