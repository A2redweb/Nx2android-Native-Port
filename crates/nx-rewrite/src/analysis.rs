//! What the rewriter works out about a function before it emits anything: exclusive-access windows, retry blocks, and where a `br x18` goes (upstream's `find_windows`, `retry_block`,
//! `jump_offsets`, `jump_adr_offsets`, `table_base`, `copied_base`, `table_cands`, `tail_call`, `jump_bytes_far`, `half_count`, `check_before_copy`).

use crate::pat::*;

/// a recognised `br x18` jump table
pub enum Jump {
    /// `br x18` as an indirect tail call
    TailCall,
    /// base + 4 * index, with this many landing pads
    Pads { base: i64, count: usize },
    /// an offset table read at rewrite time: every target
    Table(Vec<i64>),
}

fn rd32(img: &[u8], at: i64) -> Option<i32> {
    let at = usize::try_from(at).ok()?;
    img.get(at..at + 4).map(|b| i32::from_le_bytes(b.try_into().unwrap()))
}

fn bit(r: u32) -> u32 {
    1 << r
}

pub fn find_windows(insl: &[I]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for i in 0..insl.len() {
        if insl[i].m() != M::ExclLd {
            continue;
        }
        let mut j = i;
        while j < insl.len() && insl[j].m() != M::ExclSt {
            j += 1;
        }
        if j >= insl.len() || j - i >= 16 {
            continue;
        }
        // another exclusive load before the store: this one has no store of its own (a retry block)
        if insl[i + 1..j].iter().any(|x| x.m() == M::ExclLd) {
            continue;
        }
        if j + 1 < insl.len() && matches!(insl[j + 1].m(), M::Cbz | M::Cbnz) {
            j += 1;
        }
        if insl[i..=j].iter().any(I::has18) {
            out.push((i, j + 1));
        }
    }
    out
}

/// The branch insl[k] lands in the middle of window `w` (a range of insl). Accepted only when it ends an out-of-line retry block that re-enters the window at its store-exclusive.
/// Returns (the retry block's range, the window from that store on).
pub fn retry_block(insl: &[I], k: usize, w: (usize, usize)) -> Option<((usize, usize), (usize, usize))> {
    let t = insl[k].target()?;
    let first = (w.0..w.1).find(|&y| insl[y].a >= t)?;
    if insl[first].m() != M::ExclSt {
        return None;
    }
    let mut p = k as i64 - 1;
    while p > k as i64 - 8 && p > -1 {
        let q = &insl[p as usize];
        if q.m() == M::ExclLd {
            return Some(((p as usize, k + 1), (first, w.1)));
        }
        if matches!(q.m(), M::B | M::Bl | M::Br | M::Blr | M::Ret) || is_branch(q.m()) || q.m() == M::ExclSt {
            return None;
        }
        p -= 1;
    }
    None
}

/// the nearest writer of register `r` going back from insl[from] (exclusive), as an index
fn last_writer(insl: &[I], from: usize, r: u32, limit: usize) -> Option<usize> {
    (limit..from).rev().find(|&i| insl[i].d.writes() & bit(r) != 0)
}

pub fn table_base(insl: &[I], k: usize, rb: u32, far: bool) -> Option<i64> {
    let mut lo = None;
    let start = if far { 0 } else { k.saturating_sub(256) };
    for p in insl[start..k].iter().rev() {
        if far && matches!(p.m(), M::Bl | M::Blr) && rb <= 18 {
            return None;
        }
        if p.d.writes() & bit(rb) == 0 {
            continue;
        }
        if lo.is_none() {
            if let Some((d, imm)) = add_imm_same(p.w) {
                if d == rb {
                    lo = Some(imm as i64);
                    continue;
                }
            }
        }
        if let Some(lo) = lo {
            if p.m() == M::Adrp && p.w & 31 == rb {
                return Some(pcrel_target(p.w, p.a)? + lo);
            }
        }
        return None;
    }
    None
}

pub fn copied_base(insl: &[I], k: usize, rb: u32) -> Option<i64> {
    let (mut lo, mut r) = (None::<i64>, rb);
    for p in insl[..k].iter().rev() {
        if matches!(p.m(), M::Bl | M::Blr) && r <= 18 {
            return None;
        }
        // a write to wN zeroes the upper half of xN too: a clobber, never a copy of the 64-bit base (mine counts every write of the register)
        if p.d.writes() & bit(r) == 0 {
            continue;
        }
        if lo.is_none() {
            if let Some((d, s)) = mov_x(p.w) {
                if d == r {
                    r = s;
                    continue;
                }
            }
            if let Some((d, imm)) = add_imm_same(p.w) {
                if d == r {
                    lo = Some(imm as i64);
                    continue;
                }
            }
        }
        if let Some(lo) = lo {
            if p.m() == M::Adrp && p.w & 31 == r {
                return Some(pcrel_target(p.w, p.a)? + lo);
            }
        }
        return None;
    }
    None
}

pub fn table_cands(insl: &[I], rb: u32) -> Vec<i64> {
    let mut c = std::collections::BTreeSet::new();
    for (k, p) in insl.iter().enumerate() {
        if p.m() != M::Adrp || p.w & 31 != rb {
            continue;
        }
        let q = insl[k + 1..(k + 9).min(insl.len())].iter().find(|x| x.d.writes() & bit(rb) != 0);
        if let Some(q) = q {
            if let Some((d, imm)) = add_imm_same(q.w) {
                if d == rb {
                    if let Some(t) = pcrel_target(p.w, p.a) {
                        c.insert(t + imm as i64);
                    }
                }
            }
        }
    }
    c.into_iter().collect()
}

fn check_before_copy(insl: &[I], ctx_start: usize, ld: usize, idx: &[u32]) -> Option<usize> {
    let k = ld - ctx_start;
    if k < 2 {
        return None;
    }
    let (c, b) = (&insl[ld - 2], &insl[ld - 1]);
    if b.m() != M::BCond(8) {
        return None;
    }
    let (src, _) = cmp_imm(c.w, false)?;
    let kl = ld;
    let mut q = kl as i64 - 3;
    let lim = (kl as i64 - 64).max(0);
    let mut found = false;
    while q >= lim {
        let p = &insl[q as usize];
        if let Some((d, s)) = mov_w(p.w) {
            if idx.contains(&d) && s == src {
                found = true;
                break;
            }
        }
        // a compare only sets flags
        let writes = if is_cmp_like(p.w) { 0 } else { p.d.writes() };
        let mut mask = bit(src);
        for &i in idx {
            mask |= bit(i);
        }
        if writes & mask != 0 {
            return None;
        }
        q -= 1;
    }
    found.then_some(ld - 2)
}

fn is_cmp_like(w: u32) -> bool {
    // cmp / cmn / tst: subs / adds / ands with Rd = 31
    let sub_add = w & 0x1F00_001F == 0x0B00_001F && w >> 29 & 1 == 1; // add/sub (shifted / extended) with S and Rd = 31
    let imm = w & 0x1F00_001F == 0x1100_001F && w >> 29 & 1 == 1;
    let logic = w & 0x1F00_001F == 0x0A00_001F && w >> 29 & 3 == 3;
    sub_add || imm || logic
}

/// Offset-table `br x18`: all targets, or None. `at` = index of the br in `insl`.
pub fn jump_offsets(img: &[u8], insl: &[I], j: usize, copy_first: bool, far_base: bool) -> Option<Vec<i64>> {
    let cs = j.saturating_sub(16);
    let ctx = &insl[cs..j];
    let add = ctx.iter().rev().find_map(|p| add_x18_xa_xb(p.w))?;
    let (t, rb) = add;
    let ld_i = ctx.iter().rposition(|p| matches!(ldrsw_lsl2(p.w), Some((tt, bb, _)) if tt == t && bb == rb))?;
    let (_, _, ix) = ldrsw_lsl2(ctx[ld_i].w)?;
    let mut idx: Vec<u32> = vec![ix];
    let mut masked: Option<(u32, u64, usize)> = None;
    let mut cmp: Option<usize> = None;
    let mut i = ld_i as i64 - 1;
    while i >= 0 {
        let p = &ctx[i as usize];
        if let Some((d, s)) = mov_w(p.w) {
            if idx.contains(&d) {
                idx.push(s);
                i -= 1;
                continue;
            }
        }
        if let Some((d, s, mask)) = and_imm(p.w) {
            if idx.contains(&d) && masked.is_none() {
                masked = Some((s, mask, i as usize));
                i -= 1;
                continue;
            }
        }
        let Some((cn, _)) = cmp_imm(p.w, far_base) else {
            i -= 1;
            continue;
        };
        if idx.contains(&cn) {
            cmp = Some(i as usize);
            break;
        }
        if let Some(mk) = masked {
            let mut q = i - 1;
            while q >= 0 {
                let c = &ctx[q as usize];
                if c.d.writes() & bit(cn) == 0 {
                    q -= 1;
                    continue;
                }
                if let Some((_, s, mask)) = and_imm(c.w) {
                    if s == mk.0 && mask == mk.1 && !(q as usize + 1..mk.2).any(|z| ctx[z].d.writes() & bit(mk.0) != 0) {
                        cmp = Some(i as usize);
                    }
                }
                break;
            }
            if cmp.is_some() {
                break;
            }
        }
        i -= 1;
    }
    let mut cmp_global = cmp.map(|c| cs + c);
    if cmp_global.is_none() && copy_first {
        cmp_global = check_before_copy(insl, cs, cs + ld_i, &idx);
    }
    let cmp_g = cmp_global?;
    let n = cmp_imm(insl[cmp_g].w, true)?.1 as i64 + 1;
    let k = cs + ld_i;
    let (mut lo, mut page): (Option<i64>, Option<i64>) = (None, None);
    let mut broke = false;
    for p in insl[k.saturating_sub(256)..k].iter().rev() {
        if p.d.writes() & bit(rb) == 0 {
            continue;
        }
        if lo.is_none() {
            if let Some((d, imm)) = add_imm_same(p.w) {
                if d == rb {
                    lo = Some(imm as i64);
                    continue;
                }
            }
        }
        if lo.is_some() && p.m() == M::Adrp && p.w & 31 == rb {
            page = pcrel_target(p.w, p.a);
        }
        broke = true;
        break;
    }
    let (fa, fb) = (insl[0].a, insl[insl.len() - 1].a + 4);
    let from_base = |tb: i64| -> Option<Vec<i64>> {
        if tb < 0 || tb + 4 * n > img.len() as i64 {
            return None;
        }
        let t: Vec<i64> = (0..n).map(|i| tb + rd32(img, tb + 4 * i).unwrap() as i64).collect();
        t.iter().all(|&x| fa <= x && x < fb && x % 4 == 0).then_some(t)
    };
    if !broke && lo.is_none() && far_base {
        let tb = table_base(insl, k, rb, true).or_else(|| copied_base(insl, k, rb))?;
        return from_base(tb);
    }
    if page.is_none() && far_base {
        let tb = copied_base(insl, k, rb)?;
        return from_base(tb);
    }
    let tb = page? + lo?;
    if tb < 0 || tb + 4 * n > img.len() as i64 {
        return None;
    }
    Some((0..n).map(|i| tb + rd32(img, tb + 4 * i).unwrap() as i64).collect())
}

pub fn jump_adr_offsets(img: &[u8], insl: &[I], j: usize) -> Option<Vec<i64>> {
    let cs = j.saturating_sub(16);
    let ctx = &insl[cs..j];
    let ka = ctx.iter().rposition(|p| add_x18_xa_xb(p.w).is_some())?;
    let (a1, a2) = add_x18_xa_xb(ctx[ka].w)?;
    for (e, a) in [(a1, a2), (a2, a1)] {
        let Some(ld) = ctx[..ka].iter().rposition(|p| matches!(ldrsw_lsl2(p.w), Some((t, _, _)) if t == e)) else { continue };
        let Some(wa) = ctx[..ka].iter().rposition(|p| p.d.writes() & bit(a) != 0) else { continue };
        if ctx[wa].m() != M::Adr {
            continue;
        }
        let (_, tb_reg, ix) = ldrsw_lsl2(ctx[ld].w)?;
        let mut idx = vec![ix];
        let mut cmp = None;
        for p in ctx[..ld].iter().rev() {
            if let Some((d, s)) = mov_w(p.w) {
                if idx.contains(&d) {
                    idx.push(s);
                    continue;
                }
            }
            if let Some((cn, imm)) = cmp_imm(p.w, false) {
                if idx.contains(&cn) {
                    cmp = Some(imm as i64);
                    break;
                }
            }
            if idx.iter().any(|&i| p.d.writes() & bit(i) != 0) {
                return None;
            }
        }
        let n = cmp? + 1;
        let at = pcrel_target(ctx[wa].w, ctx[wa].a)?;
        let targets = |tb: i64| -> Option<Vec<i64>> {
            (tb >= 0 && tb + 4 * n <= img.len() as i64).then(|| (0..n).map(|i| at + rd32(img, tb + 4 * i).unwrap() as i64).collect())
        };
        if let Some(tb) = table_base(insl, cs + ld, tb_reg, false) {
            return targets(tb);
        }
        let (fa, fb) = (insl[0].a, insl[insl.len() - 1].a + 4);
        let ok: Vec<Vec<i64>> = table_cands(insl, tb_reg).into_iter().filter_map(targets).filter(|t| t.iter().all(|&x| fa <= x && x < fb && x % 4 == 0)).collect();
        return if ok.len() == 1 { ok.into_iter().next() } else { None };
    }
    None
}

pub fn tail_call(insl: &[I], j: usize) -> bool {
    let mut popped = j + 1 == insl.len();
    for p in insl[..j].iter().rev() {
        if ldp_fp_lr_post(p.w) {
            popped = true;
        }
        if p.d.writes() & bit(18) == 0 {
            continue;
        }
        return popped && ldr_x18_plain(p.w);
    }
    false
}

/// `(base, width)` of a byte / halfword table whose add is further back
pub fn jump_bytes_far(img: &[u8], insl: &[I], j: usize) -> Option<(i64, usize)> {
    let cs = j.saturating_sub(32);
    let ctx = &insl[cs..j];
    let mut k = None;
    for i in (0..ctx.len()).rev() {
        if add_x18_lsl2(ctx[i].w).is_some() {
            k = Some(i);
            break;
        }
        if ctx[i].d.writes() & bit(18) != 0 {
            return None;
        }
    }
    let k = k?;
    let idx = add_x18_lsl2(ctx[k].w)?;
    let base = ctx[..k].iter().rev().find(|q| q.m() == M::Adr && q.w & 31 == 18).and_then(|q| pcrel_target(q.w, q.a))?;
    let ld = ctx[..k].iter().rposition(|q| matches!(ld_small(q.w), Some((_, _, rt)) if rt == idx))?;
    let (is_byte, _, _) = ld_small(ctx[ld].w)?;
    let width = if is_byte { 256 } else { half_count(img, insl, cs + ld, base).unwrap_or(65536) };
    Some((base, width))
}

pub fn half_count(img: &[u8], insl: &[I], ld: usize, base: i64) -> Option<usize> {
    let (tb_reg, ix) = ldrh_idx(insl[ld].w)?;
    let k = ld;
    let mut idx = vec![ix];
    let mut cmp = None;
    for (off, p) in insl[k.saturating_sub(32)..k].iter().enumerate().rev() {
        if let Some((d, s)) = mov_w(p.w) {
            if idx.contains(&d) {
                idx.push(s);
                continue;
            }
        }
        if let Some((cn, _)) = cmp_imm(p.w, false) {
            if idx.contains(&cn) {
                cmp = Some(k.saturating_sub(32) + off);
                break;
            }
        }
        if idx.iter().any(|&i| p.d.writes() & bit(i) != 0) {
            return None;
        }
    }
    let kc = cmp?;
    if !matches!(insl.get(kc + 1)?.m(), M::BCond(8) | M::BCond(2)) {
        return None;
    }
    let n = cmp_imm(insl[kc].w, false)?.1 as usize + 1;
    let (fa, fb) = (insl[0].a, insl[insl.len() - 1].a + 4);
    let entries = |tb: i64| -> Option<Vec<u16>> {
        (tb >= 0 && tb + 2 * n as i64 <= img.len() as i64).then(|| (0..n).map(|i| u16::from_le_bytes(img[tb as usize + 2 * i..][..2].try_into().unwrap())).collect())
    };
    let fits = |e: &Option<Vec<u16>>| e.as_ref().map_or(false, |e| e.iter().all(|&x| fa <= base + 4 * x as i64 && base + 4 * (x as i64) < fb));
    let tb = table_base(insl, k, tb_reg, false).or_else(|| {
        let ok: Vec<i64> = table_cands(insl, tb_reg).into_iter().filter(|&c| fits(&entries(c))).collect();
        (ok.len() == 1).then(|| ok[0])
    });
    let e = tb.and_then(entries);
    if fits(&e) {
        e.unwrap().into_iter().max().map(|m| m as usize + 1)
    } else {
        None
    }
}

/// How the `br x18` at insl[j] is resolved
pub fn classify_br(img: &[u8], insl: &[I], j: usize) -> Result<Jump, String> {
    let ctx = &insl[j.saturating_sub(16)..j];
    let mut base = None;
    for p in ctx.iter().rev() {
        if p.m() == M::Adr && p.w & 31 == 18 {
            base = pcrel_target(p.w, p.a);
            break;
        }
    }
    let mut width = None;
    for (off, p) in ctx.iter().enumerate().rev() {
        if let Some(idx) = add_x18_lsl2(p.w) {
            let _ = off;
            for (qo, q) in ctx.iter().enumerate().rev() {
                if let Some((is_byte, _, rt)) = ld_small(q.w) {
                    if rt == idx {
                        if is_byte {
                            width = Some(256);
                        } else {
                            width = Some(if let Some(b) = base { half_count(img, insl, j.saturating_sub(16) + qo, b).unwrap_or(65536) } else { 65536 });
                        }
                    }
                }
                if width.is_some() {
                    break;
                }
            }
            break;
        }
    }
    if let (Some(b), Some(w)) = (base, width) {
        return Ok(Jump::Pads { base: b, count: w });
    }
    let mut tgt = jump_offsets(img, insl, j, false, false).or_else(|| jump_adr_offsets(img, insl, j));
    if tgt.is_none() && tail_call(insl, j) {
        return Ok(Jump::TailCall);
    }
    if tgt.is_none() {
        if let Some((b, w)) = jump_bytes_far(img, insl, j) {
            return Ok(Jump::Pads { base: b, count: w });
        }
    }
    if tgt.is_none() {
        tgt = jump_offsets(img, insl, j, true, true);
    }
    tgt.map(Jump::Table).ok_or_else(|| format!("jump table {:x}", insl[j].a))
}
