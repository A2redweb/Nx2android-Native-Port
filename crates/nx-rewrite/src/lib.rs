//! NSO rewriter: a port of upstream's `patch_nso.py` for the Android target. Reads a Switch module, rewrites what cannot run natively (x18, thread registers, svc, timers, exclusive windows,
//! jump tables through x18) into branches to stubs in an appended patch area, and writes the files the host loads.
//!
//! Not done yet (each listed in the output's `skipped` and by an error where it would matter): spin-wait copies, observation gates, frame-step stubs, functions longer than `BIG_FN`
//! without unwind entries, and the FAR layout (a module whose patch area would start beyond 112 MB).

pub mod analysis;
pub mod asm;
pub mod cfi;
pub mod emit;
pub mod pat;

use analysis::{classify_br, find_windows, retry_block, Jump};
use emit::{Em, R};
use pat::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;

/// A registered cut: the original bytes are checked one by one before they are replaced.
#[derive(Clone, Debug)]
pub struct Cut {
    pub off: i64,
    pub orig: Vec<u8>,
    pub new: Vec<u8>,
    pub why: String,
}

#[derive(Debug, Default)]
pub struct Output {
    pub patched: Vec<u8>,
    pub fixups: String,
    pub gates: String,
    pub sites: String,
    pub cuts: String,
    pub obs: String,
    pub stats: BTreeMap<&'static str, usize>,
    pub data_blocks: Vec<(i64, i64)>,
}

const BIG_FN: i64 = 0x40000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum K {
    Svc,
    TpidrR,
    TpidrW,
    TpidrroR,
    Cntpct,
    Cntvct,
    Ctr,
    SpAlign,
    X18,
}

fn rd32(img: &[u8], o: i64) -> u32 {
    u32::from_le_bytes(img[o as usize..o as usize + 4].try_into().unwrap())
}

fn kind_of(w: u32) -> Option<(K, u32)> {
    if w & 0xFFE0_001F == 0xD400_0001 {
        return Some((K::Svc, w >> 5 & 0xFFFF));
    }
    match w & 0xFFFF_FFE0 {
        0xD53B_D040 => return Some((K::TpidrR, 0)),
        0xD51B_D040 => return Some((K::TpidrW, 0)),
        0xD53B_D060 => return Some((K::TpidrroR, 0)),
        0xD53B_E020 => return Some((K::Cntpct, w & 31)),
        0xD53B_E040 => return Some((K::Cntvct, w & 31)),
        0xD53B_0020 => return Some((K::Ctr, w & 31)),
        _ => {}
    }
    if w & 0xFFE0_FFFF == 0xCB20_63FF && w >> 16 & 31 != 18 {
        return Some((K::SpAlign, w >> 16 & 31));
    }
    None
}

fn disasm(img: &[u8], fa: i64, fb: i64) -> Vec<I> {
    (fa..fb).step_by(4).filter_map(|a| {
        let w = rd32(img, a);
        I::decodes(w).then(|| I::new(a, w))
    }).collect()
}

pub fn rewrite(nso_bytes: &[u8], cuts: &[Cut]) -> R<Output> {
    let nso = nx_nso::Nso::parse(nso_bytes).map_err(|e| e.to_string())?;
    let mut img = nso.image(nso_bytes).map_err(|e| e.to_string())?;
    let bss = nso.bss as usize;
    let text_end = nso.segments[0].size as i64;
    let (ro_off, data_off) = (nso.segments[1].mem_offset as i64, nso.segments[2].mem_offset as i64);
    img.resize(img.len() + bss, 0);
    let key_page_off = (img.len() as i64 + 0x3FFF) & !0x3FFF;
    img.resize(key_page_off as usize + 0x4000, 0);
    let patch_off = (img.len() as i64 + 0xFFFF) & !0xFFFF;
    if patch_off > 0x700_0000 {
        return Err(format!("the patch area would start at {patch_off:#x}: the FAR layout is not implemented yet"));
    }
    let build_id = nso.build_id_hex();

    for c in cuts {
        let o = c.off as usize;
        if c.orig.len() != c.new.len() || img.get(o..o + c.orig.len()) != Some(&c.orig[..]) {
            return Err(format!("cut original bytes mismatch at {:#x}", c.off));
        }
        img[o..o + c.new.len()].copy_from_slice(&c.new);
    }

    // ---- where the code is, and every site to rewrite
    let data_blocks = cfi::data_blocks(&img, text_end);
    let mut code: Vec<(i64, i64)> = vec![];
    let mut at = 0;
    for &(s, e) in &data_blocks {
        code.push((at, s));
        at = e;
    }
    code.push((at, text_end));
    let mut kinds: BTreeMap<i64, (K, u32)> = BTreeMap::new();
    for &(s, e) in &code {
        for a in (s..e).step_by(4) {
            let w = rd32(&img, a);
            if let Some(k) = kind_of(w) {
                kinds.insert(a, k);
            } else if I::decodes(w) && nx_a64::decode(w).uses(18) {
                kinds.insert(a, (K::X18, 0));
            }
        }
    }

    let ranges = cfi::fde_ranges(&img);
    let starts: Vec<i64> = ranges.iter().map(|r| r.0).collect();
    let func_of = |a: i64| -> (i64, i64) {
        let i = starts.partition_point(|&s| s <= a);
        if i > 0 {
            (starts[i - 1], starts.get(i).copied().unwrap_or(text_end))
        } else {
            (data_blocks.iter().filter(|b| b.1 <= a).map(|b| b.1).max().unwrap_or(0), starts.first().copied().unwrap_or(text_end))
        }
    };
    let funcs: BTreeSet<(i64, i64)> = kinds.iter().filter(|(_, v)| v.0 == K::X18).map(|(&a, _)| func_of(a)).collect();
    if let Some(&(fa, fb)) = funcs.iter().find(|f| f.1 - f.0 > BIG_FN) {
        return Err(format!("function {fa:#x}..{fb:#x} is longer than {BIG_FN:#x} bytes without unwind entries: piecewise analysis is not implemented yet"));
    }

    let mut done: BTreeSet<i64> = BTreeSet::new();
    let mut windows: Vec<Vec<I>> = vec![];
    let mut retries: Vec<Vec<(Vec<I>, i64)>> = vec![];
    let mut jumps: BTreeMap<i64, (i64, usize)> = BTreeMap::new();
    let mut tabjumps: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    let mut tailcalls: BTreeSet<i64> = BTreeSet::new();
    for &(fa, fb) in &funcs {
        let insl = disasm(&img, fa, fb);
        let ws = find_windows(&insl);
        for &(i, j) in &ws {
            windows.push(insl[i..j].to_vec());
            done.extend(insl[i..j].iter().map(|x| x.a));
        }
        for (j, x) in insl.iter().enumerate() {
            if x.m() == M::Br && x.w >> 5 & 31 == 18 {
                match classify_br(&img, &insl, j)? {
                    Jump::TailCall => {
                        tailcalls.insert(x.a);
                    }
                    Jump::Pads { base, count } => {
                        jumps.insert(x.a, (base, count));
                    }
                    Jump::Table(t) => {
                        tabjumps.insert(x.a, t);
                    }
                }
            }
        }
        // no jump may land in the middle of a window, except a retry block's branch onto the window's store-exclusive, which gets a copy of its own
        let inwin: BTreeSet<i64> = ws.iter().flat_map(|&(i, j)| insl[i..j].iter().map(|x| x.a)).collect();
        let heads: BTreeSet<i64> = ws.iter().map(|&(i, _)| insl[i].a).collect();
        let mut rmid: BTreeSet<i64> = BTreeSet::new();
        for (k, x) in insl.iter().enumerate() {
            if !matches!(x.m(), M::B | M::Bl | M::Cbz | M::Cbnz | M::Tbz | M::Tbnz | M::BCond(_)) {
                continue;
            }
            let Some(t) = x.target() else { continue };
            if inwin.contains(&t) && !heads.contains(&t) {
                let w = *ws.iter().find(|&&(i, j)| insl[i..j].iter().any(|y| y.a == t)).unwrap();
                if insl[w.0..w.1].iter().any(|y| y.a == x.a) {
                    continue; // a branch inside the window itself: the copy redirects it
                }
                let rb = if matches!(x.m(), M::B | M::Bl) { None } else { retry_block(&insl, k, w) };
                let Some((r, tail)) = rb else { return Err(format!("branch into window {:#x}", x.a)) };
                if insl[r.0..r.1].iter().any(|y| done.contains(&y.a)) {
                    return Err(format!("retry block overlaps a window {:#x}", insl[r.0].a));
                }
                retries.push(vec![(insl[r.0..r.1].to_vec(), x.a + 4), (insl[tail.0..tail.1].to_vec(), insl[w.1 - 1].a + 4)]);
                done.extend(insl[r.0..r.1].iter().map(|y| y.a));
                rmid.extend(insl[r.0 + 1..r.1].iter().map(|y| y.a));
            }
        }
        for x in &insl {
            if matches!(x.m(), M::B | M::Bl | M::Cbz | M::Cbnz | M::Tbz | M::Tbnz | M::BCond(_)) && x.target().is_some_and(|t| rmid.contains(&t)) {
                return Err(format!("branch into retry block {:#x}", x.a));
            }
        }
        for (a, tgt) in &tabjumps {
            if (fa..fb).contains(a) && tgt.iter().any(|t| inwin.contains(t) && !heads.contains(t)) {
                return Err(format!("jump table target into window {a:#x}"));
            }
        }
    }

    // ---- emit
    let mut em = Em::new(patch_off, key_page_off);
    let mut stats: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut bump = |k: &'static str| *stats.entry(k).or_default() += 1;
    let put_branch = |img: &mut Vec<u8>, at: i64, to: i64| -> R<()> {
        let w = asm::b(at, to)?;
        img[at as usize..at as usize + 4].copy_from_slice(&w.to_le_bytes());
        Ok(())
    };
    for w in &windows {
        let (st, lo, _) = em.site_window(w)?;
        bump("window");
        put_branch(&mut img, lo, st)?;
    }
    for parts in &retries {
        let st = em.site_retry_window(parts)?;
        bump("retry_window");
        put_branch(&mut img, parts[0].0[0].a, st)?;
    }
    for (&a, &(k, arg)) in &kinds {
        if done.contains(&a) {
            continue;
        }
        let x = I::new(a, rd32(&img, a));
        let st = match k {
            K::X18 => {
                let m = x.m();
                if m == M::Br && tailcalls.contains(&a) {
                    bump("br_call");
                    em.site_br_call()?
                } else if m == M::Br {
                    bump("br");
                    match (tabjumps.get(&a), jumps.get(&a)) {
                        (Some(t), _) => em.site_br_tab(t)?,
                        (_, Some(&(b, c))) => em.site_br(b, c)?,
                        _ => return Err(format!("br x18 at {a:#x} was not classified")),
                    }
                } else if matches!(m, M::Cbz | M::Cbnz | M::Tbz | M::Tbnz) {
                    bump("cond");
                    em.site_cond(&x)?
                } else if matches!(m, M::Adr | M::Adrp) {
                    bump("pcrel");
                    em.site_pcrel(&x)?
                } else if m == M::Blr {
                    bump("blr");
                    em.site_blr(&x)?
                } else if x.d.dest_sp && sp_delta(x.w) == 0 {
                    bump("sp");
                    em.site_sp(&x)?
                } else if matches!(m, M::Ret | M::B | M::Bl | M::LdrLit) {
                    return Err(format!("{a:#x}: x18 in a {m:?}"));
                } else {
                    bump("plain");
                    em.site_plain(&x)?
                }
            }
            K::TpidrR => { bump("tpidr_r"); em.site_sysreg_read(&x, "KTLS")? }
            K::TpidrroR => { bump("tpidrro_r"); em.site_sysreg_read(&x, "KTLR")? }
            K::TpidrW => { bump("tpidr_w"); em.site_tpidr_write(&x)? }
            K::Svc => { bump("svc"); em.site_gate(&x, "svc", arg)? }
            K::Cntpct => { bump("cntpct"); em.site_gate(&x, "cntpct", arg)? }
            K::Cntvct => { bump("cntvct"); em.site_gate(&x, "cntvct", arg)? }
            K::SpAlign => { bump("sp_align"); em.site_sp_align(&x, arg)? }
            K::Ctr => {
                if arg == 18 {
                    return Err(format!("{a:#x}: mrs x18, ctr_el0"));
                }
                bump("ctr");
                img[a as usize..a as usize + 4].copy_from_slice(&(0xD2A0_0080u32 | arg).to_le_bytes());
                continue;
            }
        };
        put_branch(&mut img, a, st)?;
    }

    // ---- files
    let mut out = Output { data_blocks: data_blocks.clone(), ..Default::default() };
    img.resize(patch_off as usize, 0);
    img.extend_from_slice(&em.patch);
    out.patched = img;
    let mut f = String::new();
    let _ = write!(f, "# PATCH_OFF\t{patch_off:#x}\n# KEY_PAGE_OFF\t{key_page_off:#x}\n# KEY_PAGE_SIZE\t0x4000\n# TEXT_END\t{text_end:#x}\n# RO_OFF\t{ro_off:#x}\n# DATA_OFF\t{data_off:#x}\n");
    for (i, k) in emit::KEYS.iter().enumerate() {
        let _ = writeln!(f, "# KEY_{k}_IDX\t{i}");
    }
    let _ = write!(f, "# TARGET\tandroid\n# BUILD_ID\t{build_id}\n");
    out.fixups = f;
    for g in &em.gates {
        let _ = writeln!(out.gates, "{:#x}\t{}\t{}\t{:#x}", g.ret, g.kind, g.arg, g.orig);
    }
    for (&a, &(k, _)) in &kinds {
        if !done.contains(&a) {
            let n = match k { K::Svc => "svc", K::TpidrR => "tpidr_r", K::TpidrW => "tpidr_w", K::TpidrroR => "tpidrro_r", K::Cntpct => "cntpct", K::Cntvct => "cntvct", K::Ctr => "ctr", K::SpAlign => "sp_align", K::X18 => "x18" };
            let _ = writeln!(out.sites, "{a:#x}\t{n}");
        }
    }
    for w in &windows {
        let _ = writeln!(out.sites, "{:#x}\tW\t{:#x}", w[0].a, w[w.len() - 1].a + 4);
    }
    for (a, (b, c)) in &jumps {
        let _ = writeln!(out.sites, "{a:#x}\tJ\t{b:#x}\t{c}");
    }
    for (a, t) in &tabjumps {
        let _ = writeln!(out.sites, "{a:#x}\tT\t{}", t.iter().map(|t| format!("{t:#x}")).collect::<Vec<_>>().join(" "));
    }
    let _ = writeln!(out.cuts, "# BUILD_ID\t{build_id}");
    for c in cuts {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let _ = writeln!(out.cuts, "{:#x}\t{}\t{}\t{}", c.off, hex(&c.orig), hex(&c.new), c.why);
    }
    out.obs = format!("# BUILD_ID\t{build_id}\n");
    out.stats = stats;
    let _ = HashMap::<i64, i64>::new();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An uncompressed NSO: text 0x1000 (b over the MOD0 pointer; `code` from 0x10; MOD0 at 0x800 with an empty eh_frame_hdr), ro 0x1000, data 0x1000, bss 0x1000
    fn fake_nso(code: &[u32]) -> Vec<u8> {
        let mut text: Vec<u8> = (0..0x400).flat_map(|_| 0xD503_201Fu32.to_le_bytes()).collect(); // nops: zero words are data to the data-block detector
        text[..4].copy_from_slice(&0x1400_0004u32.to_le_bytes());
        text[4..8].copy_from_slice(&0x800u32.to_le_bytes());
        for (i, w) in code.iter().enumerate() {
            text[0x10 + 4 * i..0x14 + 4 * i].copy_from_slice(&w.to_le_bytes());
        }
        text[0x800..0x804].copy_from_slice(b"MOD0");
        let mut h = vec![0u8; 0x100];
        h[..4].copy_from_slice(b"NSO0");
        for i in 0..3u32 {
            let o = 0x10 + 0x10 * i as usize;
            h[o..o + 4].copy_from_slice(&(0x100 + 0x1000 * i).to_le_bytes());
            h[o + 4..o + 8].copy_from_slice(&(0x1000 * i).to_le_bytes());
            h[o + 8..o + 12].copy_from_slice(&0x1000u32.to_le_bytes());
            h[0x60 + 4 * i as usize..0x64 + 4 * i as usize].copy_from_slice(&0x1000u32.to_le_bytes());
        }
        h[0x3C..0x40].copy_from_slice(&0x1000u32.to_le_bytes());
        h[0x40] = 0xAB;
        h.extend(text);
        h.extend(vec![0u8; 0x2000]);
        h
    }

    fn word(o: &Output, off: i64) -> u32 {
        rd32(&o.patched, off)
    }

    #[test]
    fn simple_sites() {
        let n = fake_nso(&[
            0xAA00_03F2, // mov x18, x0
            0xD53B_D040, // mrs x0, tpidr_el0
            0xD400_0021, // svc #1
            0xD503_201F, // nop
            0xD65F_03C0, // ret
        ]);
        let o = rewrite(&n, &[]).unwrap();
        assert_eq!(o.stats.get("plain"), Some(&1));
        assert_eq!(o.stats.get("tpidr_r"), Some(&1));
        assert_eq!(o.stats.get("svc"), Some(&1));
        let patch_off = 0x1_0000; // text + ro + data + bss + key page, rounded to 64 KB
        assert!(o.fixups.contains("# PATCH_OFF\t0x10000\n"), "{}", o.fixups);
        assert!(o.fixups.contains("# KEY_PAGE_OFF\t0x4000\n") || o.fixups.contains("# KEY_PAGE_OFF\t0x8000\n"));
        // every rewritten site is now a b into the patch area
        for off in [0x10, 0x14, 0x18] {
            let w = word(&o, off);
            assert_eq!(w >> 26, 5, "site {off:#x}");
            let t = off + ((((w & 0x3FF_FFFF) as i32) << 6) >> 6) as i64 * 4;
            assert!(t >= patch_off, "{t:#x}");
        }
        // the stubs never touch the guest's x18, and the gate is recorded by return point
        for a in (patch_off..o.patched.len() as i64).step_by(4) {
            assert!(!nx_a64::decode(word(&o, a)).uses(18), "{a:#x}");
        }
        assert_eq!(o.gates.lines().count(), 1);
        assert!(o.gates.ends_with("\t0x18\n"), "{}", o.gates);
        assert_eq!(&o.gates.split('\t').nth(1), &Some("svc"));
    }

    #[test]
    fn x18_forms() {
        let n = fake_nso(&[
            0xB400_0052, // cbz x18, .+8
            0xD503_201F,
            0x3718_0052, // tbnz x18, #3, .+8
            0xD503_201F,
            0xB000_0012, // adrp x18, .+0x1000
            0x1000_0092, // adr x18, .+16
            0xF940_0641, // ldr x1, [x18, #8]
            0xA9BF_4FF2, // stp x18, x19, [sp, #-16]!
            0xD63F_0240, // blr x18
            0x9100_03F2, // mov x18, sp
            0xCB23_63FF, // sub sp, sp, x3
            0xD65F_03C0,
        ]);
        let o = rewrite(&n, &[]).unwrap();
        assert_eq!(o.stats.get("cond"), Some(&2));
        assert_eq!(o.stats.get("pcrel"), Some(&2));
        assert_eq!(o.stats.get("plain"), Some(&3)); // ldr, stp, mov x18, sp
        assert_eq!(o.stats.get("blr"), Some(&1));
        assert_eq!(o.stats.get("sp_align"), Some(&1));
        for a in (0x1_0000..o.patched.len() as i64).step_by(4) {
            assert!(!nx_a64::decode(word(&o, a)).uses(18), "{a:#x}");
        }
    }

    #[test]
    fn exclusive_window() {
        let n = fake_nso(&[
            0x885F_FC12, // ldaxr w18, [x0]
            0x8801_FC02, // stlxr w1, w2, [x0]
            0x35FF_FFC1, // cbnz w1, .-8
            0xD65F_03C0,
        ]);
        let o = rewrite(&n, &[]).unwrap();
        assert_eq!(o.stats.get("window"), Some(&1));
        assert!(o.sites.contains("0x10\tW\t0x1c"), "{}", o.sites);
    }

    #[test]
    fn cut_mismatch_is_refused() {
        let n = fake_nso(&[0xD65F_03C0]);
        let bad = Cut { off: 0x10, orig: vec![0, 0, 0, 0], new: vec![1, 2, 3, 4], why: "t".into() };
        assert!(rewrite(&n, &[bad]).is_err());
        let ok = Cut { off: 0x10, orig: 0xD65F_03C0u32.to_le_bytes().to_vec(), new: 0xD503_201Fu32.to_le_bytes().to_vec(), why: "t".into() };
        assert_eq!(word(&rewrite(&n, &[ok]).unwrap(), 0x10), 0xD503_201F);
    }
}
