//! One shader's translation job (upstream's `shader_vk.py` job_for, vertex_ab, xfb_layout): the code the translator takes, and the JSON of what it asks about.

use crate::container::u32le;
use crate::json::Json;
use crate::split::{Stage, CS_MAGIC, GFX_MAGIC};
use std::collections::BTreeMap;

/// Storage buffers are bound from a 256-byte boundary below their address (gfx_vk_exec.c SB_ALIGN)
pub const SB_ALIGN: i64 = 256;
/// Bits of the host's gather coordinate precision (Adreno rounds gather coordinates on a texel edge the other way from Maxwell)
pub const GATHER_BIAS: i64 = 8;

pub struct Options {
    /// the profile's android_subgroup_size (default 128)
    pub subgroup: i64,
}

pub struct Job {
    pub json: Json,
    /// what goes in the file the job's "code" names
    pub code: Vec<u8>,
    /// the c1 section (the shader's constant buffer 1 defaults)
    pub c1: Vec<u8>,
    /// a note to print: a form not known (the shader is translated without it)
    pub notes: Vec<String>,
}

/// A VertexA + VertexB program merged as Ryujinx does: (merged code, c1), None for anything else
pub fn vertex_ab(code: &[u8], ctl: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if code.len() < 0x80 || ctl.len() < 0x760 || code[..4] != GFX_MAGIC {
        return None;
    }
    let w0 = u32le(code, 0x30).ok()?;
    if w0 & 0x1F != 1 || (w0 >> 10) & 0xF != 0 {
        return None;
    }
    let f: Vec<u32> = (0..6).map(|i| u32le(ctl, 0x748 + 4 * i)).collect::<Result<_, _>>().ok()?;
    let (a_sph, a_len, b_sph, b_len) = (f[0] as usize, f[1] as usize, f[3] as usize, f[4] as usize);
    if f.iter().all(|&x| x == 0) {
        return None;
    }
    let bw0 = if (0x80..=code.len().wrapping_sub(0x50)).contains(&b_sph) && code.len() >= 0x50 { u32le(code, b_sph).ok()? } else { 0 };
    let (al, bl) = (a_len as i64 - 0x50, b_len as i64 - 0x50);
    if a_sph != 0x30 || b_sph != u32le(ctl, 0x700).ok()? as usize + 0x30 || bw0 & 0x1F != 1 || (bw0 >> 10) & 0xF != 1 || al.rem_euclid(32) != 0 || bl.rem_euclid(32) != 0 || b_sph + b_len > code.len() {
        return None;
    }
    if a_len < 0x50 || b_len < 0x50 || a_sph + a_len > code.len() {
        return None;
    }
    let words = |at: usize| -> Option<Vec<u32>> { (0..20).map(|i| u32le(code, at + 4 * i).ok()).collect() };
    let (sa, sb) = (words(a_sph)?, words(b_sph)?);
    let mut sph = vec![sb[0] | (sa[0] & !(0xFu32 << 10))];
    for i in 1..4 {
        sph.push((sa[i] & 0xFF_FFFF).max(sb[i] & 0xFF_FFFF) | (sb[i] & !0xFF_FFFF));
    }
    sph.push(sb[4]);
    for i in 5..20 {
        sph.push(sa[i] | sb[i]);
    }
    let mut av = code[a_sph + 0x50..a_sph + a_len].to_vec();
    let b = &code[b_sph + 0x50..b_sph + b_len];
    let n = av.len();
    for o in (0..n).step_by(8) {
        if o % 32 == 0 {
            continue;
        }
        let ins = u64::from_le_bytes(av[o..o + 8].try_into().unwrap());
        if ins >> 52 == 0xE30 && ins & 0x1F == 0xF {
            let v = 0xE24u64 << 52 | (((n - (o + 8)) as u64) & 0xFF_FFFF) << 20 | ins & 0xF_FFFF;
            av[o..o + 8].copy_from_slice(&v.to_le_bytes());
        }
    }
    let mut out = code[..0x30].to_vec();
    out.extend(sph.iter().flat_map(|w| w.to_le_bytes()));
    out.extend(av);
    out.extend_from_slice(b);
    Some((out, code[b_sph + b_len..].to_vec()))
}

/// A vertex shader's transform feedback as its control section declares it: per buffer None or (stride, output word at each 4-byte position, Maxwell's numbering 0x20 + 4n + component).
/// Ok(None): no feedback. Err: a form not known (the note says which): translated without capture.
pub fn xfb_layout(ctl: &[u8]) -> Result<Option<Vec<Option<(u32, Vec<u32>)>>>, String> {
    let strides: Vec<u32> = (0..4).map(|i| ctl.get(0x6DC + 2 * i..0x6DC + 2 * i + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()) as u32).ok_or("control section too short")).collect::<Result<_, _>>()?;
    if strides.iter().all(|&s| s == 0) {
        return Ok(None);
    }
    let mut out = vec![];
    for b in 0..4 {
        if strides[b] == 0 {
            out.push(None);
            continue;
        }
        let n = *ctl.get(0x6E8 + b).ok_or("control section too short")? as usize;
        let sl = |base: usize| ctl.get(base + 0x80 * b..base + 0x80 * b + n).ok_or("control section too short");
        let (outs, comps) = (sl(0xDC)?, sl(0x4DC)?);
        let words: Vec<u32> = outs.iter().zip(comps).flat_map(|(&o, &k)| (0..k as u32).map(move |c| 0x20 + 4 * (o as u32).wrapping_sub(0x20) + c)).collect();
        if n == 0 || outs.iter().any(|&o| !(0x20..0x40).contains(&o)) || comps.iter().any(|&k| !(1..=4).contains(&k)) || 4 * words.len() as u32 != strides[b] {
            return Err(format!("transform feedback buffer {b} in a form not known (stride {}, outputs {outs:?}, components {comps:?}): translated without capture", strides[b]));
        }
        out.push(Some((strides[b], words)));
    }
    Ok(Some(out))
}

/// The bundle key of a geometry shader translated for input primitive t (0 points, 1 lines, 2 lines with adjacency, 4 triangles with adjacency)
pub fn gs_variant_key(key: u64, t: u64) -> u64 {
    key ^ (t + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// `facts`: the profile's answer table row for this key (tex, img, attr, fout, local, shared); `jkey`: the key the translator names its outputs by (16 hex digits).
pub fn job_for(jkey: &str, stage: Stage, ctl: &[u8], code: &[u8], facts: Option<&Json>, o: &Options) -> Result<Job, String> {
    let (mut code_v, mut c1) = {
        let (c1o, c1t) = (u32le(ctl, 0x700)? as usize, u32le(ctl, 0x704)? as usize);
        let mut rec = code.to_vec();
        rec.extend_from_slice(ctl);
        (code.to_vec(), rec.get(c1o..c1t).ok_or("c1 section out of range")?.to_vec())
    };
    if stage == Stage::Vs {
        if let Some((c, k)) = vertex_ab(&code_v, ctl) {
            code_v = c;
            c1 = k;
        }
    }
    if stage != Stage::Cs {
        code_v = code_v.get(0x30..).ok_or("code area shorter than its header")?.to_vec();
    } else if code_v.get(..4) == Some(&CS_MAGIC[..]) {
        code_v = code_v.get(0x100..).ok_or("compute code shorter than its header")?.to_vec();
    }
    let mut notes = vec![];
    let xfb = if matches!(stage, Stage::Vs | Stage::Gs) {
        match xfb_layout(ctl) {
            Ok(x) => x,
            Err(e) => {
                notes.push(format!("{jkey}: {e}"));
                None
            }
        }
    } else {
        None
    };
    let mut j = Json::obj();
    j.set("key", Json::Str(jkey.into()));
    j.set("stage", Json::Str(stage.name().into()));
    for k in ["tex", "img", "attr", "fout"] {
        j.set(k, Json::obj());
    }
    j.set("c1", Json::Str(c1.iter().map(|b| format!("{b:02x}")).collect()));
    j.set("debug", Json::Bool(false));
    j.set("reduced", Json::Bool(true));
    j.set("sb_align", Json::Int(SB_ALIGN));
    j.set("subgroup", Json::Int(o.subgroup));
    j.set("gather_bias", Json::Int(GATHER_BIAS));
    j.set("gs_passthrough", Json::Bool(false));
    j.set("draw_parameters", Json::Bool(true));
    if let Some(x) = xfb {
        j.set("xfb", Json::Arr(x.into_iter().map(|b| match b {
            None => Json::Null,
            Some((stride, words)) => {
                let mut m = Json::obj();
                m.set("stride", Json::Int(stride as i64));
                m.set("words", Json::Arr(words.into_iter().map(|w| Json::Int(w as i64)).collect()));
                m
            }
        }).collect()));
    }
    if stage == Stage::Cs {
        let v: Vec<u32> = (0..4).map(|i| u32le(ctl, 0x748 + 4 * i)).collect::<Result<_, _>>()?;
        j.set("local", Json::Arr(v[..3].iter().map(|&x| Json::Int(x as i64)).collect()));
        j.set("shared", Json::Int(((v[3] + 15) / 16 * 16) as i64));
    }
    if stage == Stage::Ps && ctl.len() >= 0x71C && u32le(ctl, 0x718)? & 1 != 0 {
        j.set("early_z", Json::Bool(true));
    }
    match facts {
        Some(Json::Obj(m)) => {
            for (k, v) in m {
                j.set(k, v.clone());
            }
        }
        _ => j.set("no_facts", Json::Bool(true)),
    }
    Ok(Job { json: j, code: code_v, c1, notes })
}

/// The profile's answer table (`shader_facts.tsv`): key (16 hex digits) → row. A malformed line is an error.
pub fn parse_facts(text: &str) -> Result<BTreeMap<String, Json>, String> {
    let mut m = BTreeMap::new();
    for (n, l) in text.lines().enumerate() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let (k, v) = l.split_once('\t').ok_or_else(|| format!("shader_facts.tsv line {}: no tab", n + 1))?;
        m.insert(k.to_string(), Json::parse(v).map_err(|e| format!("shader_facts.tsv line {}: {e}", n + 1))?);
    }
    Ok(m)
}

/// The profile's handle remap table (`android_shader_handles.tsv`): key → (source, target, sites)
pub fn parse_handles(text: &str) -> Result<BTreeMap<String, (u32, u32, usize)>, String> {
    let mut m = BTreeMap::new();
    for (n, l) in text.lines().enumerate() {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        let num = |s: &str| -> Result<u64, String> { (if let Some(h) = s.strip_prefix("0x") { u64::from_str_radix(h, 16) } else { s.parse() }).map_err(|_| format!("android_shader_handles.tsv line {}: bad number", n + 1)) };
        if f.len() != 4 {
            return Err(format!("android_shader_handles.tsv line {}: expected 4 columns", n + 1));
        }
        m.insert(f[0].to_string(), (num(f[1])? as u32, num(f[2])? as u32, num(f[3])? as usize));
    }
    Ok(m)
}
