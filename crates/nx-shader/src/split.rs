//! Finds a game's Switch shaders (upstream's `shader_split.py`): a control section and a code area each, keyed by the hash of both.
//! Readers so far: BNSH (the Nintendo shader archive carried by bfsha, effects, UI layouts) and BAHS (agl's sharcb), inside Yaz0, SARC and zstd containers and loose in files.
//! The other engines' readers (Unity, Unreal, KTGL, RE Engine, Trinity, ...) are not ported yet; `scan_blob` is where each one is added.

use crate::container::*;
use nx_crypto::Sha256;

pub const CTL_MAGIC: [u8; 4] = [0x34, 0x12, 0x76, 0x98];
pub const GFX_MAGIC: [u8; 4] = [0x78, 0x56, 0x34, 0x12];
pub const CS_MAGIC: [u8; 4] = [0x79, 0x56, 0x34, 0x12];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    Vs,
    Hs,
    Ds,
    Gs,
    Ps,
    Cs,
}

impl Stage {
    /// NVN's stage numbers (the bundle's `stage` field)
    pub fn nvn(self) -> u32 {
        match self {
            Stage::Vs => 0,
            Stage::Ps => 1,
            Stage::Gs => 2,
            Stage::Hs => 3,
            Stage::Ds => 4,
            Stage::Cs => 5,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Stage::Vs => "vs",
            Stage::Hs => "hs",
            Stage::Ds => "ds",
            Stage::Gs => "gs",
            Stage::Ps => "ps",
            Stage::Cs => "cs",
        }
    }
}

/// BNSH's stage slots, in order
const BNSH_STAGES: [Stage; 6] = [Stage::Vs, Stage::Hs, Stage::Ds, Stage::Gs, Stage::Ps, Stage::Cs];

#[derive(Clone, Debug)]
pub struct Found {
    /// where it was found: `<file>#BNSH@<offset>#var<n>`
    pub label: String,
    pub stage: Stage,
    pub ctl: Vec<u8>,
    pub code: Vec<u8>,
}

/// A graphics code area's stage from SPH word 0 (shader type in bits 10..13; 0 with SPH type 1 is a vertex shader as well: Catherine: Full Body's GLSLC)
pub fn sph_stage(code: &[u8]) -> Option<Stage> {
    let w = u32le(code, 0x30).ok()?;
    let t = (w >> 10) & 0xF;
    if t == 0 && w & 0x1F == 1 {
        return Some(Stage::Vs);
    }
    Some(match t {
        1 => Stage::Vs,
        2 => Stage::Hs,
        3 => Stage::Ds,
        4 => Stage::Gs,
        5 => Stage::Ps,
        _ => return None,
    })
}

/// The number of control bytes in the key: the section's own length (+0x20) when it is 0x760..0x880 (SDK 10's Super Mario 3D World), else 0x880.
pub fn ctl_len(ctl: &[u8]) -> usize {
    match u32le(ctl, 0x20) {
        Ok(n) if (0x760..0x880).contains(&n) => n as usize,
        _ => 0x880,
    }
}

/// sha256(control[..ctl_len] + code)[..8] as the bundle's u64 key (upstream prints it as 16 hex digits: the same bytes, big endian)
pub fn skey(ctl: &[u8], code: &[u8]) -> u64 {
    let mut b = ctl[..ctl_len(ctl).min(ctl.len())].to_vec();
    b.extend_from_slice(code);
    let h = Sha256::digest(&b);
    u64::from_be_bytes(h[..8].try_into().unwrap())
}

/// Two copies of one key hold the same shader: equal code areas, and control sections equal up to their own length
pub fn same_shader(a: (&[u8], &[u8]), b: (&[u8], &[u8])) -> bool {
    let (ca, cb) = (a.0, b.0);
    let n = if ca.len() >= 0x24 { u32le(ca, 0x20).unwrap_or(ca.len() as u32) as usize } else { ca.len() };
    let n = if n >= 0x760 && n <= ca.len().min(cb.len()) { n } else { ca.len().max(cb.len()) };
    a.1 == b.1 && ca.get(..n) == cb.get(..n)
}

/// Control section and code area consistent; the reason if not
pub fn check(st: Stage, ctl: &[u8], code: &[u8]) -> Option<&'static str> {
    if ctl.len() < 0x24 || ctl[..4] != CTL_MAGIC || ctl.len() < ctl_len(ctl) {
        return Some("bad control section");
    }
    let m = code.get(..4);
    // a compute code area may have no header; a graphics one must start with the graphics magic
    if st == Stage::Cs && m == Some(&GFX_MAGIC[..]) || st != Stage::Cs && m != Some(&GFX_MAGIC[..]) {
        return Some("code area header magic does not match the stage");
    }
    if u32le(ctl, 0x704).ok()? as usize != code.len() {
        return Some("control +0x704 does not match the code area length");
    }
    None
}

/// A GLSLC output's stage, from its code area's header (graphics: the SPH's stage bits), or compute when the code has no header and the control's thread group (+0x748) is filled
fn glslc_stage(ctl: &[u8], code: &[u8]) -> Option<Stage> {
    let m = code.get(..4);
    let headerless_cs = m != Some(&CS_MAGIC[..]) && m != Some(&GFX_MAGIC[..]) && ctl.len() >= 0x754 && (0..3).all(|i| u32le(ctl, 0x748 + 4 * i).is_ok_and(|v| v != 0));
    if m == Some(&CS_MAGIC[..]) || headerless_cs {
        Some(Stage::Cs)
    } else if m == Some(&GFX_MAGIC[..]) && code.len() >= 0x34 {
        sph_stage(code)
    } else {
        None
    }
}

/// The GPU code sections of the GLSLC output at `h` (section count at +0x4c, headers of 0x90 bytes from +0x90: {size, data offset, type (0 = GPU code)}, a GPU code header adds +0x30 control offset, +0x34 code offset, +0x38 code length, +0x3c control length)
fn glslc_section(d: &[u8], h: usize, size: usize) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    let n = u32le(d, h + 0x4c).ok()? as usize;
    if n == 0 || n > 16 || h + 0x90 * (n + 1) > d.len() {
        return None;
    }
    let mut secs = vec![];
    for i in 0..n {
        let s = h + 0x90 + 0x90 * i;
        let (ssz, soff, typ) = (u32le(d, s).ok()? as usize, u32le(d, s + 4).ok()? as usize, u32le(d, s + 8).ok()?);
        if typ != 0 {
            continue;
        }
        let (co, ko, kn, cn) = (u32le(d, s + 0x30).ok()? as usize, u32le(d, s + 0x34).ok()? as usize, u32le(d, s + 0x38).ok()? as usize, u32le(d, s + 0x3c).ok()? as usize);
        if soff + ssz > size || co + cn > ssz || ko + kn > ssz {
            return None;
        }
        let b = h + soff;
        secs.push((d.get(b + co..b + co + cn)?.to_vec(), d.get(b + ko..b + ko + kn)?.to_vec()));
    }
    Some(secs)
}

/// GLSLC compiler outputs (magic 0x19866891, +0x44 the whole output's length): one stage each (a control section with the code area right after it) or several compiled as one program, read by section headers.
/// Only outputs that pass the control / code consistency check are kept: the magic is just 4 bytes.
pub fn glslc_shaders(d: &[u8], label: &str) -> Vec<Found> {
    let mut out = vec![];
    for h in find_all(d, &[0x91, 0x68, 0x86, 0x19]) {
        if h + 0x48 > d.len() {
            continue;
        }
        let Ok(size) = u32le(d, h + 0x44).map(|v| v as usize) else { continue };
        if !(size > 0x760 && size <= d.len() - h) {
            continue;
        }
        let Some(c) = find_all(&d[h..h + size], &CTL_MAGIC).next().map(|x| h + x) else { continue };
        if find_all(&d[c + 4..h + size], &CTL_MAGIC).next().is_some() {
            // several stages: every GPU code section, each stage once
            let secs = glslc_section(d, h, size).unwrap_or_default();
            let sts: Vec<Option<Stage>> = secs.iter().map(|(c, k)| glslc_stage(c, k)).collect();
            let distinct: std::collections::BTreeSet<_> = sts.iter().collect();
            if secs.len() < 2 || distinct.len() < sts.len() || sts.iter().zip(&secs).any(|(st, (c, k))| st.is_none_or(|st| check(st, c, k).is_some())) {
                continue;
            }
            for (st, (c, k)) in sts.into_iter().zip(secs) {
                out.push(Found { label: format!("{label}#GLSLC@{h:#x}#var"), stage: st.unwrap(), ctl: c, code: k });
            }
            continue;
        }
        if c + 0x708 > d.len() {
            continue;
        }
        let (Ok(nctl), Ok(ncode)) = (u32le(d, c + 0x20), u32le(d, c + 0x704)) else { continue };
        let (nctl, ncode) = (nctl as usize, ncode as usize);
        let direct = (|| {
            if nctl < 0x760 || nctl & 3 != 0 || c + nctl + ncode > h + size {
                return None;
            }
            let (ctl, code) = (d.get(c..c + nctl)?.to_vec(), d.get(c + nctl..c + nctl + ncode)?.to_vec());
            let st = glslc_stage(&ctl, &code)?;
            check(st, &ctl, &code).is_none().then_some((st, ctl, code))
        })();
        let found = direct.or_else(|| {
            // SDK 4's GLSLC writes control sections whose +0x20 is short of their length: the section header says where both are
            let sec = glslc_section(d, h, size)?;
            if sec.len() != 1 {
                return None;
            }
            let (ctl, code) = sec.into_iter().next()?;
            let st = glslc_stage(&ctl, &code)?;
            check(st, &ctl, &code).is_none().then_some((st, ctl, code))
        });
        if let Some((st, ctl, code)) = found {
            out.push(Found { label: format!("{label}#GLSLC@{h:#x}"), stage: st, ctl, code });
        }
    }
    out
}

pub fn bnsh_shaders(d: &[u8], base: usize, label: &str) -> Result<Vec<Found>, String> {
    if d.get(base + 0x60..base + 0x64) != Some(b"grsc") {
        return Err(format!("{label} BNSH @{base:#x} has no grsc block"));
    }
    let nvar = u32le(d, base + 0x7C)? as usize;
    let pvar = u64le(d, base + 0x80)? as usize;
    let mut out = vec![];
    for v in 0..nvar {
        let prog = u64le(d, base + pvar + 0x40 * v + 0x10)? as usize;
        if prog == 0 {
            continue;
        }
        for (si, st) in BNSH_STAGES.iter().enumerate() {
            let sc = u64le(d, base + prog + 8 + 8 * si)? as usize;
            if sc == 0 {
                continue;
            }
            let (pctl, pcode) = (u64le(d, base + sc + 8)? as usize, u64le(d, base + sc + 16)? as usize);
            let (ncode, nctl) = (u32le(d, base + sc + 24)? as usize, u32le(d, base + sc + 28)? as usize);
            let ctl = d.get(base + pctl..base + pctl + nctl).ok_or("control section out of range")?;
            let code = d.get(base + pcode..base + pcode + ncode).ok_or("code area out of range")?;
            if let Some(why) = check(*st, ctl, code) {
                return Err(format!("{label} BNSH @{base:#x} variation {v} {}: {why}", st.name()));
            }
            out.push(Found { label: format!("{label}#BNSH@{base:#x}#var{v}"), stage: *st, ctl: ctl.to_vec(), code: code.to_vec() });
        }
    }
    Ok(out)
}

pub fn bahs_shaders(d: &[u8], label: &str) -> Result<Vec<Found>, String> {
    let (region, rsz) = (u32le(d, 0x1C)? as usize, u32le(d, 0x18)? as usize);
    if region + rsz > d.len() {
        return Err(format!("{label} BAHS code region out of bounds"));
    }
    let mut out = vec![];
    for a in (0..region.saturating_sub(16)).step_by(4) {
        let (off, z, csz, rel) = (u32le(d, a)? as usize, u32le(d, a + 4)?, u32le(d, a + 8)? as usize, u32le(d, a + 12)? as i32 as i64);
        if z != 0 || !(csz == 0x880 || csz == 0x88C || (0x760..0x880).contains(&csz)) || off % 0x100 != 0 {
            continue;
        }
        let c = a as i64 + rel;
        if c < 0 || c as usize + csz > region || d.get(c as usize..c as usize + 4) != Some(&CTL_MAGIC[..]) {
            continue;
        }
        let c = c as usize;
        let n = u32le(d, c + 0x704)? as usize;
        if off + n > rsz {
            continue;
        }
        let (Some(code), Some(ctl)) = (d.get(region + off..region + off + n), d.get(c..c + csz)) else { continue };
        let st = if code.get(..4) == Some(&CS_MAGIC[..]) {
            Some(Stage::Cs)
        } else if code.get(..4) == Some(&GFX_MAGIC[..]) {
            sph_stage(code)
        } else {
            None
        };
        let Some(st) = st else { continue };
        if check(st, ctl, code).is_some() {
            continue;
        }
        out.push(Found { label: format!("{label}#BAHS@{a:#x}"), stage: st, ctl: ctl.to_vec(), code: code.to_vec() });
    }
    Ok(out)
}

fn find_all<'a>(d: &'a [u8], pat: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    let mut from = 0;
    std::iter::from_fn(move || {
        let p = d[from.min(d.len())..].windows(pat.len()).position(|w| w == pat)? + from;
        from = p + 1;
        Some(p)
    })
}

/// Shaders in one blob: Yaz0 is inflated, SARC is read member by member, BAHS and BNSH are read. A blob none of those recognise gives nothing.
pub fn scan_blob(d: &[u8], label: &str, out: &mut Vec<Found>) -> Result<(), String> {
    if d.get(..4) == Some(b"Yaz0") {
        return scan_blob(&yaz0(d)?, label, out);
    }
    if d.get(..4) == Some(b"SARC") {
        for (nm, m) in sarc_members(d)? {
            scan_blob(m, &format!("{label}/{nm}"), out)?;
        }
        return Ok(());
    }
    if d.get(..4) == Some(b"BAHS") {
        out.extend(bahs_shaders(d, label)?);
        return Ok(());
    }
    for p in find_all(d, b"BNSH\0\0\0\0") {
        let Ok(size) = u32le(d, p + 0x1C) else { continue };
        if p + size as usize <= d.len() && d.get(p + 0x60..p + 0x64) == Some(b"grsc") {
            out.extend(bnsh_shaders(d, p, label)?);
        }
    }
    out.extend(glslc_shaders(d, label));
    Ok(())
}

/// What the Unity reader needs beyond the file: the common string table of main's image and the type trees seen so far
pub struct UnityState {
    pub common: Option<std::sync::Arc<std::collections::HashMap<u32, String>>>,
    pub trees: crate::unity::Trees,
    /// the second pass: trees are borrowed, and a ComputeShader without one is left out
    pub borrowed: bool,
}

impl UnityState {
    pub fn none() -> Self {
        UnityState { common: None, trees: Default::default(), borrowed: false }
    }
}

/// A file's shaders: a zstd file is decompressed first, whatever its name. Ok(true): a Unity file whose type trees are not known yet, to be scanned again with borrowed ones (nothing is added to `out`)
pub fn scan_file_with(bytes: &[u8], label: &str, out: &mut Vec<Found>, u: &mut UnityState) -> Result<bool, String> {
    let owned;
    let mut bytes = bytes;
    if bytes.get(..4) == Some(&ZSTD_MAGIC[..]) {
        owned = unzs(bytes)?;
        bytes = &owned;
    }
    let yaz;
    if bytes.get(..4) == Some(b"Yaz0") {
        yaz = yaz0(bytes)?;
        bytes = &yaz;
    }
    if bytes.get(..8) == Some(b"UnityFS\0") || (bytes.get(8..11) == Some(&[0, 0, 0][..]) && crate::unity::is_serialized(bytes)) {
        let common = u.common.clone().ok_or("a Unity asset, but main has no Unity common string table")?;
        let mut mine = vec![];
        let before = u.trees.clone();
        return match crate::unity::unity_shaders(bytes, label, &common, &mut u.trees, u.borrowed, &mut mine) {
            Ok(()) => {
                out.extend(mine);
                Ok(false)
            }
            Err(crate::unity::UErr::NoTree(_)) if !u.borrowed => {
                u.trees = before; // a deferred file lends no trees
                Ok(true)
            }
            Err(e) => Err(e.to_string()),
        };
    }
    let interesting = bytes.get(..4).is_some_and(|m| matches!(m, b"Yaz0" | b"SARC" | b"BAHS")) || find_all(bytes, b"BNSH\0\0\0\0").next().is_some() || find_all(bytes, &[0x91, 0x68, 0x86, 0x19]).next().is_some();
    if interesting {
        scan_blob(bytes, label, out)?;
    }
    Ok(false)
}

/// `scan_file_with` for a file that cannot be Unity
pub fn scan_file(bytes: &[u8], label: &str, out: &mut Vec<Found>) -> Result<(), String> {
    scan_file_with(bytes, label, out, &mut UnityState::none()).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A BNSH holding one variation with a vertex and a pixel stage
    pub fn fake_bnsh() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let mk_ctl = |n: usize| {
            let mut c = vec![0u8; 0x880];
            c[..4].copy_from_slice(&CTL_MAGIC);
            c[0x20..0x24].copy_from_slice(&0x880u32.to_le_bytes());
            c[0x704..0x708].copy_from_slice(&(n as u32).to_le_bytes());
            c
        };
        let mk_code = |t: u32| {
            let mut c = vec![0u8; 0x80];
            c[..4].copy_from_slice(&GFX_MAGIC);
            c[0x30..0x34].copy_from_slice(&((t << 10) | 1).to_le_bytes());
            c
        };
        let (vctl, vcode, pctl, pcode) = (mk_ctl(0x80), mk_code(1), mk_ctl(0x80), mk_code(5));
        let mut d = vec![0u8; 0x1000];
        d[..8].copy_from_slice(b"BNSH\0\0\0\0");
        d[0x1C..0x20].copy_from_slice(&0x1000u32.to_le_bytes());
        d[0x60..0x64].copy_from_slice(b"grsc");
        d[0x7C..0x80].copy_from_slice(&1u32.to_le_bytes());
        d[0x80..0x88].copy_from_slice(&0x100u64.to_le_bytes()); // variation table
        d[0x100 + 0x10..0x100 + 0x18].copy_from_slice(&0x200u64.to_le_bytes()); // program
        d[0x200 + 8..0x200 + 16].copy_from_slice(&0x300u64.to_le_bytes()); // vertex stage descriptor (slot 0)
        d[0x200 + 8 + 32..0x200 + 8 + 40].copy_from_slice(&0x340u64.to_le_bytes()); // pixel (slot 4)
        let put = |d: &mut Vec<u8>, at: usize, ctl_at: usize, code_at: usize, ctl: &[u8], code: &[u8]| {
            d[at + 8..at + 16].copy_from_slice(&(ctl_at as u64).to_le_bytes());
            d[at + 16..at + 24].copy_from_slice(&(code_at as u64).to_le_bytes());
            d[at + 24..at + 28].copy_from_slice(&(code.len() as u32).to_le_bytes());
            d[at + 28..at + 32].copy_from_slice(&(ctl.len() as u32).to_le_bytes());
            d[ctl_at..ctl_at + ctl.len()].copy_from_slice(ctl);
            d[code_at..code_at + code.len()].copy_from_slice(code);
        };
        d.resize(0x3000, 0);
        put(&mut d, 0x300, 0x400, 0x1000, &vctl, &vcode);
        put(&mut d, 0x340, 0x1400 + 0x400, 0x2000, &pctl, &pcode);
        (d, vctl, vcode, pcode)
    }

    #[test]
    fn bnsh_in_a_sarc_in_yaz0_literals() {
        let (d, vctl, vcode, _) = fake_bnsh();
        let mut out = vec![];
        scan_file(&d, "/x.bfsha", &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].stage, out[1].stage), (Stage::Vs, Stage::Ps));
        assert_eq!(out[0].label, "/x.bfsha#BNSH@0x0#var0");
        assert_eq!(skey(&vctl, &vcode), skey(&out[0].ctl, &out[0].code));
        assert!(same_shader((&out[0].ctl, &out[0].code), (&vctl, &vcode)));
    }

    #[test]
    fn checks_catch_inconsistent_sections() {
        let (_, vctl, vcode, _) = fake_bnsh();
        assert_eq!(check(Stage::Vs, &vctl, &vcode), None);
        assert!(check(Stage::Cs, &vctl, &vcode).is_some()); // graphics magic in a compute shader
        assert!(check(Stage::Vs, &vctl, &vcode[..0x40]).is_some()); // length mismatch
        assert!(check(Stage::Vs, &vctl[..0x30], &vcode).is_some());
    }

    #[test]
    fn stage_from_sph() {
        let (_, _, vcode, pcode) = fake_bnsh();
        assert_eq!(sph_stage(&vcode), Some(Stage::Vs));
        assert_eq!(sph_stage(&pcode), Some(Stage::Ps));
        let mut z = vcode.clone();
        z[0x30..0x34].copy_from_slice(&1u32.to_le_bytes()); // type 0 with SPH type 1: vertex
        assert_eq!(sph_stage(&z), Some(Stage::Vs));
        z[0x30..0x34].copy_from_slice(&((9u32 << 10) | 1).to_le_bytes());
        assert_eq!(sph_stage(&z), None);
    }

    #[test]
    fn keys_match_python() {
        // python: hashlib.sha256(bytes(0x880) + b'x').hexdigest()[:16]
        let ctl = vec![0u8; 0x880];
        let k = skey(&ctl, b"x");
        assert_eq!(format!("{k:016x}"), "99c3ed278ecd8e49");
    }

    #[test]
    fn glslc_output_of_one_stage_is_found_inside_other_bytes() {
        let nctl = 0x880usize;
        let mut ctl = vec![0u8; nctl];
        ctl[..4].copy_from_slice(&CTL_MAGIC);
        ctl[0x20..0x24].copy_from_slice(&(nctl as u32).to_le_bytes());
        let mut code = vec![0u8; 0x100];
        code[..4].copy_from_slice(&GFX_MAGIC);
        code[0x30..0x34].copy_from_slice(&(5u32 << 10).to_le_bytes()); // pixel
        ctl[0x704..0x708].copy_from_slice(&(code.len() as u32).to_le_bytes());
        let mut blob = vec![0xAAu8; 0x33];
        let h = blob.len();
        let mut out = vec![0u8; 0x48];
        out[..4].copy_from_slice(&[0x91, 0x68, 0x86, 0x19]);
        out[0x44..0x48].copy_from_slice(&((0x48 + nctl + code.len()) as u32).to_le_bytes());
        out.extend(&ctl);
        out.extend(&code);
        blob.extend(&out);
        blob.extend([0x55u8; 9]);
        let mut found = vec![];
        scan_file(&blob, "main", &mut found).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].stage, &found[0].ctl, &found[0].code), (Stage::Ps, &ctl, &code));
        assert_eq!(found[0].label, format!("main#GLSLC@{h:#x}"));
        // a code area whose length disagrees with the control is not a shader
        let mut bad = blob.clone();
        bad[h + 0x48 + 0x704] ^= 1;
        let mut none = vec![];
        scan_file(&bad, "main", &mut none).unwrap();
        assert!(none.is_empty());
    }
}

/// A synthetic BNSH for the tests of this crate: (the file, vertex control, vertex code, pixel code)
#[cfg(test)]
pub fn tests_fake_bnsh() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    tests::fake_bnsh()

}
