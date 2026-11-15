//! The stubs the rewriter puts in the patch area (upstream's `site_*` functions, Android target: a guest thread's slots hang off bionic's TLS_SLOT_APP, `tpidr_el0[2]`).

use crate::asm::*;
use crate::pat::*;

pub type R<T> = Result<T, String>;

pub const KEYS: [&str; 4] = ["K18", "KTLS", "KTLR", "KGATE"];

fn key_index(kind: &str) -> u32 {
    KEYS.iter().position(|k| *k == kind).unwrap() as u32
}

/// the first `n` of x0..x15 that are not in `avoid`
pub fn pick(avoid: u32, n: usize) -> R<Vec<u32>> {
    let v: Vec<u32> = (0..16).filter(|r| avoid >> r & 1 == 0).take(n).collect();
    if v.len() == n {
        Ok(v)
    } else {
        Err("no free scratch registers".into())
    }
}

pub struct Gate {
    pub ret: i64,
    pub kind: &'static str,
    pub arg: u32,
    pub orig: i64,
}

pub struct Em {
    pub patch: Vec<u8>,
    pub patch_off: i64,
    pub key_off: i64,
    pub gates: Vec<Gate>,
    pub slot_refs: usize,
}

fn is_cond(m: M) -> bool {
    matches!(m, M::Cbz | M::Cbnz | M::Tbz | M::Tbnz | M::BCond(_))
}

/// the branch word `w` re-aimed by `off` words (cbz / cbnz / b.cond: 19 bits, tbz / tbnz: 14 bits)
fn retarget(w: u32, m: M, off: i64) -> u32 {
    if matches!(m, M::Tbz | M::Tbnz) {
        (w & 0xFFF8_001F) | ((off as u32 & 0x3FFF) << 5)
    } else {
        (w & 0xFF00_001F) | ((off as u32 & 0x7FFFF) << 5)
    }
}

/// x18 → xS in an instruction (only the operand fields that name it)
fn subst(x: &I, s: u32) -> R<u32> {
    if x.d.implied {
        return Err(format!("{:#x}: x18 in an instruction with implied registers", x.a));
    }
    Ok(x.d.rename(18, s))
}

impl Em {
    pub fn new(patch_off: i64, key_off: i64) -> Em {
        Em { patch: vec![], patch_off, key_off, gates: vec![], slot_refs: 0 }
    }
    pub fn here(&self) -> i64 {
        self.patch_off + self.patch.len() as i64
    }
    pub fn emit(&mut self, w: u32) {
        self.patch.extend_from_slice(&w.to_le_bytes());
    }
    fn jump(&mut self, to: i64) -> R<()> {
        let w = b(self.here(), to)?;
        self.emit(w);
        Ok(())
    }
    fn tp(&mut self, t: u32) {
        self.emit(mrs_tpidr(t));
        self.emit(ldrx(t, t, 2));
    }
    fn key_addr(&mut self, reg: u32, kind: &str) -> R<()> {
        self.slot_refs += 1;
        let w = adrp(reg, self.here(), self.key_off)?;
        self.emit(w);
        self.emit(ldrx(reg, reg, key_index(kind)));
        Ok(())
    }
    fn slot_addr(&mut self, base: u32, key_reg: u32, kind: &str) -> R<()> {
        self.key_addr(key_reg, kind)?;
        self.emit(addr(base, base, key_reg));
        Ok(())
    }
    /// stp S,T; slot address of K18 into T; S = guest x18
    fn open18(&mut self, s: u32, t: u32, save: i32) -> R<()> {
        self.emit(stp(s, t, 31, save));
        self.tp(t);
        self.slot_addr(t, s, "K18")?;
        self.emit(ldrx(s, t, 0));
        Ok(())
    }

    pub fn site_plain(&mut self, x: &I) -> R<i64> {
        let st = pick(x.regs(), 2)?;
        let (s, t) = (st[0], st[1]);
        let dlt = sp_delta(x.w);
        let save = if dlt == 0 { -16 } else { -112 };
        if dlt != 0 && dlt < -96 {
            return Err(format!("sp delta {dlt} at {:#x}", x.a));
        }
        let start = self.here();
        self.open18(s, t, save)?;
        self.emit(subst(x, s)?);
        self.emit(strx(s, t, 0));
        self.emit(ldp(s, t, 31, save - dlt as i32));
        self.jump(x.a + 4)?;
        Ok(start)
    }

    /// destination sp and a source x18 (alloca: mov sp, x18)
    pub fn site_sp(&mut self, x: &I) -> R<i64> {
        let st = pick(x.regs(), 2)?;
        let (s, t) = (st[0], st[1]);
        let start = self.here();
        self.open18(s, t, -16)?;
        self.emit(addi(t, 31, 0, 0));
        self.emit(subst(x, s)?);
        self.emit(ldp(s, t, t, -16));
        self.jump(x.a + 4)?;
        Ok(start)
    }

    /// `sub sp, sp, xN`: xN is rounded up to 16 first
    pub fn site_sp_align(&mut self, x: &I, n: u32) -> R<i64> {
        let start = self.here();
        self.emit(addi(n, n, 15, 0));
        self.emit(0x927C_EC00 | n << 5 | n);
        self.emit(x.w);
        self.jump(x.a + 4)?;
        Ok(start)
    }

    pub fn site_cond(&mut self, x: &I) -> R<i64> {
        let st = pick(x.regs(), 2)?;
        let (s, t) = (st[0], st[1]);
        let tgt = x.target().ok_or("cond target")?;
        let start = self.here();
        self.open18(s, t, -16)?;
        let pc = self.here();
        let nw = retarget(x.w, x.m(), 3) & !31 | s;
        self.emit(nw);
        self.emit(ldp(s, t, 31, -16));
        self.jump(x.a + 4)?;
        debug_assert_eq!(self.here(), pc + 12);
        self.emit(ldp(s, t, 31, -16));
        self.jump(tgt)?;
        Ok(start)
    }

    pub fn site_pcrel(&mut self, x: &I) -> R<i64> {
        let tgt = pcrel_target(x.w, x.a).ok_or("pcrel target")?;
        let (s, t) = (0, 1);
        let start = self.here();
        self.emit(stp(s, t, 31, -16));
        self.tp(t);
        self.slot_addr(t, s, "K18")?;
        let w = adrp(s, self.here(), tgt)?;
        self.emit(w);
        if x.m() == M::Adr {
            self.emit(addi(s, s, (tgt & 0xFFF) as u32, 0));
        }
        self.emit(strx(s, t, 0));
        self.emit(ldp(s, t, 31, -16));
        self.jump(x.a + 4)?;
        Ok(start)
    }

    /// `br x18` through a pad table: target = base + 4i
    pub fn site_br(&mut self, base: i64, count: usize) -> R<i64> {
        let start = self.here();
        self.emit(stur(16, 31, -16));
        self.emit(stur(17, 31, -24));
        self.tp(16);
        self.slot_addr(16, 17, "K18")?;
        self.emit(ldrx(16, 16, 0));
        self.emit(ldur(17, 31, -24));
        let padbase = start + 4 * 24;
        let delta = padbase - base;
        let (dl, sub) = (delta.unsigned_abs(), delta < 0);
        let mk = |h: u32, sh: u32| if sub { subi(16, 16, h, sh) } else { addi(16, 16, h, sh) };
        let (mut hi, lo) = (dl >> 12, (dl & 0xFFF) as u32);
        let mut ops = vec![];
        while hi > 0 {
            let h = hi.min(0xFFF);
            ops.push(mk(h as u32, 1));
            hi -= h;
        }
        if lo != 0 {
            ops.push(mk(lo, 0));
        }
        if ops.len() > 12 {
            return Err(format!("pad table base too far from the patch area ({base:#x})"));
        }
        for o in ops {
            self.emit(o);
        }
        self.emit(br(16));
        if self.here() > padbase {
            return Err("pad table prologue too long".into());
        }
        while self.here() < padbase {
            self.emit(NOP);
        }
        let tails = padbase + 4 * count as i64;
        for i in 0..count as i64 {
            self.jump(tails + 8 * i)?;
        }
        for i in 0..count as i64 {
            self.emit(0xF85F_0000 | 31 << 5 | 16);
            self.jump(base + 4 * i)?;
        }
        Ok(start)
    }

    /// `br x18` through an offset table whose targets were read at rewrite time
    pub fn site_br_tab(&mut self, targets: &[i64]) -> R<i64> {
        let start = self.here();
        self.emit(stur(16, 31, -16));
        self.emit(stur(17, 31, -24));
        self.tp(16);
        self.slot_addr(16, 17, "K18")?;
        self.emit(ldrx(16, 16, 0));
        let mut uniq = targets.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        let pads = self.here() + 4 * (4 * uniq.len() as i64 + 1);
        for (k, &t) in uniq.iter().enumerate() {
            let w = adrp(17, self.here(), t)?;
            self.emit(w);
            self.emit(addi(17, 17, (t & 0xFFF) as u32, 0));
            self.emit(cmpr(16, 17));
            let w = beq(self.here(), pads + 12 * k as i64)?;
            self.emit(w);
        }
        self.emit(brk(0x18));
        for &t in &uniq {
            self.emit(ldur(16, 31, -16));
            self.emit(ldur(17, 31, -24));
            self.jump(t)?;
        }
        Ok(start)
    }

    pub fn site_blr(&mut self, x: &I) -> R<i64> {
        let start = self.here();
        self.tp(16);
        self.slot_addr(16, 17, "K18")?;
        self.emit(ldrx(16, 16, 0));
        let ret = x.a + 4;
        let w = adrp(30, self.here(), ret)?;
        self.emit(w);
        self.emit(addi(30, 30, (ret & 0xFFF) as u32, 0));
        self.emit(br(16));
        Ok(start)
    }

    pub fn site_br_call(&mut self) -> R<i64> {
        let start = self.here();
        self.tp(16);
        self.slot_addr(16, 17, "K18")?;
        self.emit(ldrx(16, 16, 0));
        self.emit(br(16));
        Ok(start)
    }

    pub fn site_sysreg_read(&mut self, x: &I, kind: &str) -> R<i64> {
        let n = x.w & 31;
        let tmp = if n != 16 { 16 } else { 17 };
        let start = self.here();
        self.emit(stur(tmp, 31, -16));
        self.tp(n);
        self.slot_addr(n, tmp, kind)?;
        self.emit(ldrx(n, n, 0));
        self.emit(ldur(tmp, 31, -16));
        self.jump(x.a + 4)?;
        Ok(start)
    }

    pub fn site_tpidr_write(&mut self, x: &I) -> R<i64> {
        let n = x.w & 31;
        let t = pick(1 << n, 1)?[0];
        let start = self.here();
        self.emit(0xF81F_0000 | 31 << 5 | t);
        let tmp = [16, 17].into_iter().find(|&r| r != n && r != t).unwrap();
        self.emit(stur(tmp, 31, -24));
        self.tp(t);
        self.slot_addr(t, tmp, "KTLS")?;
        self.emit(strx(n, t, 0));
        self.emit(ldur(tmp, 31, -24));
        self.emit(0xF85F_0000 | 31 << 5 | t);
        self.jump(x.a + 4)?;
        Ok(start)
    }

    /// svc / mrs cntpct_el0 / cntvct_el0 → host gate (looked up by return point in gates.tsv)
    pub fn site_gate(&mut self, x: &I, kind: &'static str, arg: u32) -> R<i64> {
        let start = self.here();
        self.emit(stp_pre(29, 30, 31, -16));
        self.tp(30);
        self.slot_addr(30, 29, "KGATE")?;
        self.emit(ldrx(30, 30, 0));
        self.emit(ldrx(29, 31, 0));
        self.emit(blr(30));
        let ret = self.here();
        self.emit(ldp_post(29, 30, 31, 16));
        self.jump(x.a + 4)?;
        self.gates.push(Gate { ret, kind, arg, orig: x.a });
        Ok(start)
    }

    /// An exclusive-access window: copied with x18 as a scratch register; the last instruction's fall-through and every branch out leave through an exit stub.
    /// Returns (stub, window start, window end)
    pub fn site_window(&mut self, seg: &[I]) -> R<(i64, i64, i64)> {
        let used = seg.iter().fold(0, |m, x| m | x.regs());
        let st = pick(used, 2)?;
        let (s, t) = (st[0], st[1]);
        let (lo, hi) = (seg[0].a, seg[seg.len() - 1].a + 4);
        let start = self.here();
        self.open18(s, t, -16)?;
        let copy0 = self.here();
        let n = seg.len() as i64;
        // the fall-through stub comes first (right after the copy); the stubs of branches that leave the window follow
        let exits = copy0 + 4 * n + 16;
        let mut ex: Vec<i64> = vec![];
        let mut words = vec![];
        for (k, x) in seg.iter().enumerate() {
            let mut w = x.w;
            if x.has18() {
                w = subst(x, s)?;
            }
            let m = x.m();
            let pc = copy0 + 4 * k as i64;
            if is_cond(m) {
                let tgt = x.target().ok_or("branch target")?;
                let newt = if (lo..hi).contains(&tgt) {
                    copy0 + (tgt - lo)
                } else {
                    ex.push(tgt);
                    exits + 16 * (ex.len() as i64 - 1)
                };
                w = retarget(w, m, (newt - pc) >> 2);
            } else if m == M::B {
                let tgt = x.target().ok_or("branch target")?;
                let newt = if (lo..hi).contains(&tgt) {
                    copy0 + (tgt - lo)
                } else if tgt == hi {
                    copy0 + 4 * n
                } else {
                    let i = ex.iter().position(|&e| e == tgt).unwrap_or_else(|| {
                        ex.push(tgt);
                        ex.len() - 1
                    });
                    exits + 16 * i as i64
                };
                w = b(pc, newt)?;
            } else if matches!(m, M::Adr | M::Adrp | M::Bl | M::Br | M::Blr | M::Ret | M::LdrLit) {
                return Err(format!("{:#x}: instruction not allowed in an exclusive window", x.a));
            }
            words.push(w);
        }
        for w in words {
            self.emit(w);
        }
        let mut all = vec![hi];
        all.extend(ex);
        for tgt in all {
            self.emit(strx(s, t, 0));
            self.emit(ldp(s, t, 31, -16));
            self.jump(tgt)?;
            self.emit(NOP);
        }
        Ok((start, lo, hi))
    }

    /// A retry block and the window from its store on, copied under one prologue. parts = [(instructions, fall-through address)]
    pub fn site_retry_window(&mut self, parts: &[(Vec<I>, i64)]) -> R<i64> {
        let used = parts.iter().flat_map(|p| p.0.iter()).fold(0, |m, x| m | x.regs());
        let st = pick(used, 2)?;
        let (s, t) = (st[0], st[1]);
        let start = self.here();
        self.open18(s, t, -16)?;
        let mut pos = std::collections::HashMap::new();
        let mut at = self.here();
        for (seg, _) in parts {
            for x in seg {
                pos.insert(x.a, at);
                at += 4;
            }
            at += 16;
        }
        let exits = at;
        let mut ex: Vec<i64> = vec![];
        for (seg, ft) in parts {
            for x in seg {
                let mut w = x.w;
                if x.has18() {
                    w = subst(x, s)?;
                }
                let m = x.m();
                let pc = self.here();
                if is_cond(m) {
                    let tgt = x.target().ok_or("branch target")?;
                    let newt = match pos.get(&tgt) {
                        Some(&p) => p,
                        None => {
                            let i = ex.iter().position(|&e| e == tgt).unwrap_or_else(|| {
                                ex.push(tgt);
                                ex.len() - 1
                            });
                            exits + 16 * i as i64
                        }
                    };
                    w = retarget(w, m, (newt - pc) >> 2);
                } else if matches!(m, M::Adr | M::Adrp | M::B | M::Bl | M::Br | M::Blr | M::Ret | M::LdrLit) {
                    return Err(format!("{:#x}: instruction not allowed in a retry window", x.a));
                }
                self.emit(w);
            }
            self.emit(strx(s, t, 0));
            self.emit(ldp(s, t, 31, -16));
            self.jump(*ft)?;
            self.emit(NOP);
        }
        for tgt in ex {
            self.emit(strx(s, t, 0));
            self.emit(ldp(s, t, 31, -16));
            self.jump(tgt)?;
            self.emit(NOP);
        }
        Ok(start)
    }
}
