//! What each descriptor of a translated shader is for (upstream's `shader_vk.py` classify) and the binary reflection the host reads (refl_bin; layout in dm/android/gfx_vk.h dmsv_refl / dmsv_bind).

use crate::json::Json;
use crate::spirv::Binding;

const TEX_BASE: i64 = 0x20;
const IMG_BASE: i64 = 0x120;
const IMG_SLOTS: i64 = 8;
const SEPTEX_BASE: i64 = 0x168;
const SEPTEX_SLOT0: i64 = 128;
const SAMP_BASE: i64 = 0x568;

fn ssbo_base(stage: &str) -> Option<i64> {
    match stage {
        "vs" => Some(0x110),
        "cs" => Some(0x310),
        "ps" => Some(0x510),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Classified {
    pub kind: &'static str,
    pub reg: Option<i64>,
    pub bank: Option<i64>,
    pub rw: bool,
    pub off: Option<i64>,
    pub samp_tex: bool,
    pub samp: Option<i64>,
    pub samp_id: Option<i64>,
    pub samp_ubo: Option<(i64, i64)>,
    pub fetch1d: bool,
}

fn digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `[a-z]+_<letter><digits>` as a full match → the digits
fn lower_prefixed_number(name: &str, letter: char) -> Option<i64> {
    let n = name.bytes().take_while(|b| b.is_ascii_lowercase()).count();
    let rest = name.get(n..)?;
    let rest = rest.strip_prefix('_')?;
    // the letters before the underscore are greedy, but a name has exactly one underscore before the letter here
    digits(rest.strip_prefix(letter)?)
}

/// `[a-z]+_([ti])\w*?_(?:cb(\d+)|tcb)_([0-9A-F]+)` matched at the start: (kind letter, slots, handle)
fn texture_name(name: &str) -> Option<(char, Option<i64>, u64)> {
    let b = name.as_bytes();
    let lead = b.iter().take_while(|c| c.is_ascii_lowercase()).count();
    for p in (1..=lead).rev() {
        if b.get(p) != Some(&b'_') {
            continue;
        }
        let Some(&k) = b.get(p + 1) else { continue };
        if k != b't' && k != b'i' {
            continue;
        }
        let rest = &name[p + 2..];
        for cut in 0..=rest.len() {
            if !rest[..cut].bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                break;
            }
            let Some(tail) = rest[cut..].strip_prefix('_') else { continue };
            let (slots, after) = if let Some(t) = tail.strip_prefix("cb") {
                let nd = t.bytes().take_while(|c| c.is_ascii_digit()).count();
                if nd == 0 {
                    continue;
                }
                (t[..nd].parse::<i64>().ok(), &t[nd..])
            } else if let Some(t) = tail.strip_prefix("tcb") {
                (None, t)
            } else {
                continue;
            };
            let Some(h) = after.strip_prefix('_') else { continue };
            let nh = h.bytes().take_while(|c| matches!(c, b'0'..=b'9' | b'A'..=b'F')).count();
            if nh == 0 {
                continue;
            }
            return Some((k as char, slots, u64::from_str_radix(&h[..nh], 16).ok()?));
        }
    }
    None
}

fn slot(word: i64, base: i64, cb: i64) -> Option<i64> {
    if cb == 2 && word * 4 >= base && (word * 4 - base) % 8 == 0 {
        Some((word * 4 - base) / 8)
    } else {
        None
    }
}

/// `res`: the translator's reflection (`sbufs` rows, `fetch1d`). `dynamic_slots`: the profile's android_dynamic_slots = on.
pub fn classify(stage: &str, b: &Binding, res: &Json, dynamic_slots: bool) -> Classified {
    let nm = b.name.as_str();
    let mut e = Classified { kind: "?", ..Default::default() };
    if dynamic_slots && matches!(b.desc, "image" | "sampler") && b.count == 32 {
        let ok = {
            // [a-z]+_[ts]\w*_cb0_40000000
            let n = nm.bytes().take_while(|c| c.is_ascii_lowercase()).count();
            n > 0 && nm[n..].starts_with('_') && nm[n + 1..].starts_with(['t', 's']) && nm.ends_with("_cb0_40000000") && nm.len() >= n + 2 + "_cb0_40000000".len() - 1
        };
        if ok {
            e.kind = if b.desc == "image" { "texd" } else { "sampd" };
            return e;
        }
    }
    if let Some(n) = lower_prefixed_number(nm, 'c') {
        if (3..=16).contains(&n) {
            e.kind = "cbuf";
            e.reg = Some(n - 3);
        } else {
            e.kind = if n == 1 { "c1" } else { "drvcb" };
            e.bank = Some(n);
        }
        return e;
    }
    if nm == "support_buffer" {
        e.kind = "support";
        return e;
    }
    if let Some(idx) = lower_prefixed_number(nm, 's') {
        let mut sb: Vec<&Json> = res.get("sbufs").and_then(|v| v.as_arr()).map(|a| a.iter().collect()).unwrap_or_default();
        sb.sort_by_key(|x| x.get("binding").and_then(|v| v.as_i64()).unwrap_or(0));
        let s = sb.get(idx as usize);
        let int = |j: &Json, k: &str| j.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        let off = s.filter(|s| int(s, "cb_slot") == 0).map(|s| int(s, "cb_offset") * 4);
        let base = ssbo_base(stage);
        e.kind = "sbuf";
        e.rw = s.is_some_and(|s| int(s, "flags") & 1 != 0);
        if stage == "cs" && off.is_some_and(|o| (0x210..0x310).contains(&o) && (o - 0x210) % 16 == 0) {
            e.kind = "ubo_ptr";
            e.reg = Some((off.unwrap() - 0x210) / 16);
        } else if let Some(s) = s.filter(|s| int(s, "cb_slot") >= 3) {
            e.kind = "sbuf_ubo";
            e.reg = Some(int(s, "cb_slot") - 3);
            e.off = Some(int(s, "cb_offset") * 4);
        } else {
            e.reg = match (off, base) {
                (Some(o), Some(bs)) if (0..0x100).contains(&(o - bs)) && (o - bs) % 16 == 0 => Some((o - bs) / 16),
                _ => None,
            };
        }
        return e;
    }
    if let Some((kind, slots, h)) = texture_name(nm) {
        let slots_i = slots.unwrap_or(-1);
        let tb = if slots_i >= 0 { slots_i & 0xFFFF } else { 2 };
        let sb = if slots_i >= 0 && (slots_i >> 16) != 0 { (slots_i >> 16) - 1 } else { tb };
        let (o0, o1, ty) = ((h & 0x3FFF) as i64, ((h >> 14) & 0x3FFF) as i64, (h >> 28) as i64);
        if tb == 2 {
            let words: Vec<i64> = if ty != 0 { vec![o0, o1] } else { vec![o0] };
            if kind == 'i' {
                e.kind = "img";
                e.reg = words.iter().copied().filter(|x| (IMG_BASE..IMG_BASE + 8 * IMG_SLOTS).contains(&(x * 4))).find_map(|x| slot(x, IMG_BASE, tb));
                if b.desc == "storage_tbuf" {
                    e.kind = "img_tbuf";
                }
                return e;
            }
            if b.count > 1 || b.count == 0 {
                e.kind = "tex_c2a";
                e.off = if ty == 0 { Some(o0 * 4) } else { None };
                return e;
            }
            let tw: Vec<i64> = if ty == 3 { vec![o0] } else { words.clone() };
            let mut t = tw.iter().copied().filter(|x| x * 4 < SEPTEX_BASE).find_map(|x| slot(x, TEX_BASE, tb));
            if t.is_none() {
                t = tw.iter().copied().filter(|x| x * 4 < SAMP_BASE).find_map(|x| slot(x, SEPTEX_BASE, tb).map(|s| SEPTEX_SLOT0 + s));
            }
            e.kind = if b.desc == "tbuf" { "tbuf" } else { "tex" };
            e.reg = t;
            if ty == 3 {
                e.samp_id = Some(o1);
            } else if ty != 0 {
                e.samp = words.iter().copied().filter(|x| x * 4 >= SAMP_BASE).find_map(|x| slot(x, SAMP_BASE, sb));
            } else {
                e.samp_tex = true;
            }
            if let Some(t) = t {
                if e.kind == "tex" && res.get("fetch1d").and_then(|v| v.as_arr()).is_some_and(|a| a.iter().any(|x| x.as_i64() == Some(t))) {
                    e.fetch1d = true;
                }
            }
            return e;
        }
        if (3..=16).contains(&tb) && matches!(ty, 0 | 1 | 3) && !(ty == 1 && !(3..=16).contains(&sb)) {
            e.kind = if kind == 'i' { "img_ubo" } else if b.desc == "tbuf" { "tbuf_ubo" } else { "tex_ubo" };
            e.reg = Some(tb - 3);
            e.off = Some(o0 * 4);
            if ty == 3 {
                e.samp_id = Some(o1);
            } else if ty == 1 {
                e.samp_ubo = Some((sb - 3, o1 * 4));
            } else if e.kind == "tex_ubo" {
                e.samp_tex = true;
            }
            return e;
        }
    }
    e
}

fn kind_num(k: &str) -> u8 {
    match k {
        "support" => 0,
        "cbuf" => 1,
        "c1" => 2,
        "drvcb" => 3,
        "sbuf" => 4,
        "ubo_ptr" => 5,
        "sbuf_ubo" => 6,
        "tex" => 7,
        "tbuf" => 8,
        "img" => 9,
        "img_tbuf" => 10,
        "tex_c2a" => 11,
        "tex_ubo" => 12,
        "tbuf_ubo" => 13,
        "img_ubo" => 14,
        "texd" => 15,
        "sampd" => 16,
        _ => 255,
    }
}

fn desc_num(d: &str) -> u8 {
    match d {
        "ubo" => 0,
        "ssbo" => 1,
        "sampled" => 2,
        "storage" => 3,
        "tbuf" => 4,
        "storage_tbuf" => 5,
        "image" => 6,
        "sampler" => 7,
        _ => 255,
    }
}

/// header {u32 nbind, u32 c1_len, u32 w[3]}, nbind × 20-byte bindings {u8 kind, u8 desc, u8 dim (0xff none), u8 flags (1 arrayed, 2 multisampled, 4 written, 8 fetch1d), u16 binding,
/// u16 count, i16 reg, u8 samp source, u8 fmt, i32 off, i32 samp_arg}, then c1. `hdr3`: a compute shader's local size, a vertex shader's input masks, a pixel shader's output masks.
pub fn refl_bin(binds: &[(Binding, Classified)], c1: &[u8], hdr3: [u32; 3]) -> Vec<u8> {
    let mut out = vec![];
    for v in [binds.len() as u32, c1.len() as u32, hdr3[0], hdr3[1], hdr3[2]] {
        out.extend(v.to_le_bytes());
    }
    for (b, e) in binds {
        let mut reg = e.reg.unwrap_or(-1);
        if e.kind == "drvcb" {
            reg = e.bank.unwrap_or(0);
        }
        let off = e.off.unwrap_or(-1);
        let (samp, sarg): (u8, i64) = if e.samp_tex {
            (1, 0)
        } else if let Some(s) = e.samp {
            (2, s)
        } else if let Some(s) = e.samp_id {
            (3, s)
        } else if let Some((a, o)) = e.samp_ubo {
            (4, a | o << 8)
        } else {
            (0, 0)
        };
        let flags = (b.arrayed != 0) as u8 | ((b.ms != 0) as u8) << 1 | (e.rw as u8) << 2 | (e.fetch1d as u8) << 3;
        out.push(kind_num(e.kind));
        out.push(desc_num(b.desc));
        out.push(b.dim.map_or(0xFF, |d| d as u8));
        out.push(flags);
        out.extend((b.binding as u16).to_le_bytes());
        out.extend((b.count as u16).to_le_bytes());
        out.extend((reg as i16).to_le_bytes());
        out.push(samp);
        out.push(b.fmt as u8);
        out.extend((off as i32).to_le_bytes());
        out.extend((sarg as i32).to_le_bytes());
    }
    out.extend_from_slice(c1);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(name: &str, desc: &'static str, count: u32) -> Binding {
        Binding { name: name.into(), set: 0, binding: 7, desc, count, dim: Some(1), arrayed: 0, ms: 0, fmt: 0 }
    }
    fn res() -> Json {
        Json::parse(r#"{"sbufs":[{"binding":9,"slot":0,"cb_slot":0,"cb_offset":132,"flags":1},{"binding":4,"slot":1,"cb_slot":3,"cb_offset":2,"flags":0}],"fetch1d":[1]}"#).unwrap()
    }

    #[test]
    fn constant_buffers_and_storage() {
        let r = res();
        assert_eq!((classify("vs", &b("vp_c4", "ubo", 1), &r, false).kind, classify("vs", &b("vp_c4", "ubo", 1), &r, false).reg), ("cbuf", Some(1)));
        let c = classify("vs", &b("vp_c1", "ubo", 1), &r, false);
        assert_eq!(c.kind, "c1");
        let c = classify("vs", &b("vp_c0", "ubo", 1), &r, false);
        assert_eq!((c.kind, c.bank), ("drvcb", Some(0)));
        assert_eq!(classify("vs", &b("support_buffer", "ubo", 1), &r, false).kind, "support");
        // sbufs sorted by binding: s0 is the one at binding 4 (cb_slot 3 → sbuf_ubo), s1 the one at binding 9 (offset 528 = 0x210, written)
        let c = classify("ps", &b("fp_s0", "ssbo", 1), &r, false);
        assert_eq!((c.kind, c.reg, c.off), ("sbuf_ubo", Some(0), Some(8)));
        let c = classify("cs", &b("cp_s1", "ssbo", 1), &r, false);
        assert_eq!((c.kind, c.reg, c.rw), ("ubo_ptr", Some(0), true));
        let c = classify("vs", &b("vp_s1", "ssbo", 1), &r, false);
        assert_eq!((c.kind, c.reg, c.rw), ("sbuf", None, true));
    }

    #[test]
    fn textures_through_the_driver_table() {
        let r = res();
        // c2 handle table: texture slot 1 is at byte 0x28 → word 10, one combined handle (type 0): sampler is in the texture handle
        let c = classify("ps", &b("fp_t2d_cb2_0000000A", "sampled", 1), &r, false);
        assert_eq!((c.kind, c.reg, c.samp_tex), ("tex", Some(1), true));
        // type 3: texture word 10, constant sampler id in o1 = 5 → handle = 3 << 28 | 5 << 14 | 10
        let c = classify("ps", &b("fp_t2d_tcb_3001400A", "sampled", 1), &r, false);
        assert_eq!((c.kind, c.reg, c.samp_id), ("tex", Some(1), Some(5)));
        // fetch1d: slot 1 is listed
        let c = classify("ps", &b("fp_t2d_tcb_0000000A", "sampled", 1), &r, false);
        assert!(c.fetch1d);
        // an image at c2 word 0x120/4 = 72 → image slot 0
        let c = classify("cs", &b("cp_i2d_tcb_00000048", "storage", 1), &r, false);
        assert_eq!((c.kind, c.reg), ("img", Some(0)));
        // bindless: handle in the game's uniform buffer 5 at word 3: cb5, type 0
        let c = classify("ps", &b("fp_t2d_cb5_00000003", "sampled", 1), &r, false);
        assert_eq!((c.kind, c.reg, c.off, c.samp_tex), ("tex_ubo", Some(2), Some(12), true));
        assert_eq!(classify("ps", &b("something_else", "ubo", 1), &r, false).kind, "?");
    }

    #[test]
    fn binary_layout() {
        let e = Classified { kind: "tex", reg: Some(3), samp: Some(2), ..Default::default() };
        let mut bd = b("x", "sampled", 1);
        bd.arrayed = 1;
        let v = refl_bin(&[(bd, e)], &[0xAA, 0xBB], [1, 2, 3]);
        assert_eq!(v.len(), 20 + 20 + 2);
        assert_eq!(&v[..4], &1u32.to_le_bytes());
        assert_eq!(&v[20..28], &[7, 2, 1, 1, 7, 0, 1, 0]); // kind tex, desc sampled, dim 1, flags arrayed, binding 7, count 1
        assert_eq!(&v[28..32], &[3, 0, 2, 0]); // reg 3, samp source SLOT, fmt 0
        assert_eq!(&v[32..36], &(-1i32).to_le_bytes());
        assert_eq!(&v[36..40], &2i32.to_le_bytes());
        assert_eq!(&v[40..], &[0xAA, 0xBB]);
    }
}
