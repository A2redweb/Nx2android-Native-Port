//! Instruction patterns the rewriter's analysis looks for, over decoded words. Upstream matches capstone's operand text; each function here states which encoding that text stands for.

use nx_a64::Insn;

/// One decoded instruction of a function.
#[derive(Clone, Debug)]
pub struct I {
    pub a: i64,
    pub w: u32,
    pub d: Insn,
}

impl I {
    pub fn new(a: i64, w: u32) -> I {
        I { a, w, d: nx_a64::decode(w) }
    }
    /// capstone's "this word is an instruction"
    pub fn decodes(w: u32) -> bool {
        nx_a64::valid(w)
    }
    pub fn has18(&self) -> bool {
        self.d.uses(18)
    }
    /// registers named by the instruction or read / written by it (upstream's regs_in)
    pub fn regs(&self) -> u32 {
        self.d.used() | self.d.reads() | self.d.writes()
    }
    pub fn m(&self) -> M {
        mnem(self.w)
    }
    /// the target of a direct branch, b.cond, cbz / cbnz, tbz / tbnz
    pub fn target(&self) -> Option<i64> {
        branch_target(self.w, self.a)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum M {
    B,
    Bl,
    /// plain `br xN` (not the pointer-authenticated forms)
    Br,
    Blr,
    Ret,
    Cbz,
    Cbnz,
    Tbz,
    Tbnz,
    /// b.cond (condition code)
    BCond(u8),
    Adr,
    Adrp,
    ExclLd,
    ExclSt,
    LdrLit,
    Other,
}

pub fn mnem(w: u32) -> M {
    match w >> 24 {
        0x14..=0x17 => return M::B,
        0x94..=0x97 => return M::Bl,
        0x54 if w & 0x10 == 0 => return M::BCond((w & 15) as u8),
        0x34 | 0xB4 => return M::Cbz,
        0x35 | 0xB5 => return M::Cbnz,
        0x36 | 0xB6 => return M::Tbz,
        0x37 | 0xB7 => return M::Tbnz,
        _ => {}
    }
    if w & 0xFFFF_FC1F == 0xD61F_0000 {
        return M::Br;
    }
    if w & 0xFFFF_FC1F == 0xD63F_0000 {
        return M::Blr;
    }
    if w & 0xFFFF_FC1F == 0xD65F_0000 {
        return M::Ret;
    }
    if w & 0x9F00_0000 == 0x1000_0000 {
        return M::Adr;
    }
    if w & 0x9F00_0000 == 0x9000_0000 {
        return M::Adrp;
    }
    if w >> 24 & 0x3F == 0x08 && w >> 23 & 1 == 0 {
        return if w >> 22 & 1 == 1 { M::ExclLd } else { M::ExclSt };
    }
    if w & 0x3B00_0000 == 0x1800_0000 && w >> 30 < 2 {
        return M::LdrLit;
    }
    M::Other
}

pub fn is_branch(m: M) -> bool {
    matches!(m, M::B | M::Bl | M::Cbz | M::Cbnz | M::Tbz | M::Tbnz | M::BCond(_))
}

fn sext(v: u32, bits: u32) -> i64 {
    ((v << (32 - bits)) as i32 >> (32 - bits)) as i64
}

pub fn branch_target(w: u32, a: i64) -> Option<i64> {
    match mnem(w) {
        M::B | M::Bl => Some(a + 4 * sext(w & 0x03FF_FFFF, 26)),
        M::Cbz | M::Cbnz | M::BCond(_) => Some(a + 4 * sext(w >> 5 & 0x7FFFF, 19)),
        M::Tbz | M::Tbnz => Some(a + 4 * sext(w >> 5 & 0x3FFF, 14)),
        _ => None,
    }
}

/// adr / adrp: the address it computes
pub fn pcrel_target(w: u32, a: i64) -> Option<i64> {
    let imm = ((w >> 5 & 0x7FFFF) << 2 | (w >> 29 & 3)) as u32;
    let imm = sext(imm, 21);
    match mnem(w) {
        M::Adr => Some(a + imm),
        M::Adrp => Some((a & !0xFFF) + (imm << 12)),
        _ => None,
    }
}

/// `mov wD, wM` (orr wD, wzr, wM): (D, M); sp / zero registers excluded as in the text
pub fn mov_w(w: u32) -> Option<(u32, u32)> {
    (w & 0xFFE0_FFE0 == 0x2A00_03E0 && w & 31 != 31 && w >> 16 & 31 != 31).then_some((w & 31, w >> 16 & 31))
}

/// `mov xD, xM`
pub fn mov_x(w: u32) -> Option<(u32, u32)> {
    (w & 0xFFE0_FFE0 == 0xAA00_03E0 && w & 31 != 31 && w >> 16 & 31 != 31).then_some((w & 31, w >> 16 & 31))
}

/// `cmp wN, #imm` (subs wzr, wN, #imm, no shift) or with `wide` the 64-bit form: (N, imm)
pub fn cmp_imm(w: u32, wide: bool) -> Option<(u32, u32)> {
    let ok = w & 0xFFC0_001F == 0x7100_001F || (wide && w & 0xFFC0_001F == 0xF100_001F);
    (ok && w >> 5 & 31 != 31).then_some((w >> 5 & 31, w >> 10 & 0xFFF))
}

/// the value of a logical-immediate encoding (N, immr, imms) for a register of `bits` bits
pub fn decode_bitmask(n: u32, immr: u32, imms: u32, bits: u32) -> Option<u64> {
    let x = (n << 6) | (!imms & 0x3F);
    if x == 0 {
        return None;
    }
    let len = 31 - x.leading_zeros();
    if len < 1 || (1 << len) > bits {
        return None;
    }
    let size = 1u32 << len;
    let levels = size - 1;
    let s = imms & levels;
    let r = immr & levels;
    if s == levels {
        return None;
    }
    let ones = (1u64 << (s + 1)) - 1;
    let elem = if r == 0 { ones } else { ((ones >> r) | (ones << (size - r))) & (u64::MAX >> (64 - size)) };
    let mut v = 0u64;
    let mut i = 0;
    while i < 64 {
        v |= elem << i;
        i += size;
    }
    Some(if bits == 32 { v & 0xFFFF_FFFF } else { v })
}

/// `and rD, rN, #imm`: (D, N, mask)
pub fn and_imm(w: u32) -> Option<(u32, u32, u64)> {
    let sf = w >> 31;
    if w & 0x7F80_0000 != 0x1200_0000 {
        return None;
    }
    let (rd, rn) = (w & 31, w >> 5 & 31);
    if rd == 31 || rn == 31 {
        return None;
    }
    let n = w >> 22 & 1;
    if sf == 0 && n == 1 {
        return None;
    }
    decode_bitmask(n, w >> 16 & 0x3F, w >> 10 & 0x3F, if sf == 1 { 64 } else { 32 }).map(|m| (rd, rn, m))
}

/// `add x18, xA, xB` (shifted register, no shift): (A, B)
pub fn add_x18_xa_xb(w: u32) -> Option<(u32, u32)> {
    (w & 0xFFE0_FC1F == 0x8B00_0012 && w >> 5 & 31 != 31 && w >> 16 & 31 != 31).then_some((w >> 5 & 31, w >> 16 & 31))
}

/// `ldrsw xT, [xB, xI, lsl #2]`: (T, B, I)
pub fn ldrsw_lsl2(w: u32) -> Option<(u32, u32, u32)> {
    (w & 0xFFE0_FC00 == 0xB8A0_7800 && w & 31 != 31 && w >> 5 & 31 != 31 && w >> 16 & 31 != 31).then_some((w & 31, w >> 5 & 31, w >> 16 & 31))
}

/// `add xD, xD, #imm` (no shift): (D, imm)
pub fn add_imm_same(w: u32) -> Option<(u32, u32)> {
    (w & 0xFFC0_0000 == 0x9100_0000 && w & 31 == w >> 5 & 31 && w & 31 != 31).then_some((w & 31, w >> 10 & 0xFFF))
}

/// `add x18, x18, xN, lsl #2`: N
pub fn add_x18_lsl2(w: u32) -> Option<u32> {
    (w & 0xFFE0_FFFF == 0x8B00_0A52).then_some(w >> 16 & 31)
}

/// byte / halfword loads: (is byte, signed, Rt) for the unsigned-offset, register-offset and pre / post-index forms (not the unscaled `ldurb`)
pub fn ld_small(w: u32) -> Option<(bool, bool, u32)> {
    let size = w >> 30;
    if size > 1 || w >> 26 & 1 == 1 {
        return None;
    }
    let opc = w >> 22 & 3;
    if opc == 0 || opc == 2 && false {
        return None;
    }
    let g = w >> 24 & 0x3F;
    let ok = match g {
        0b111001 => true,
        0b111000 => {
            let b21 = w >> 21 & 1;
            let b1110 = w >> 10 & 3;
            (b21 == 1 && b1110 == 2) || (b21 == 0 && (b1110 == 1 || b1110 == 3))
        }
        _ => false,
    };
    ok.then_some((size == 0, opc >= 2, w & 31))
}

/// `ldrh wE, [xT, xI, lsl #1]` / `uxtw #1`: (T, I) when it is a halfword load with a scaled register index
pub fn ldrh_idx(w: u32) -> Option<(u32, u32)> {
    // size 01, opc 01, register offset (bit 21, bits 11:10 = 10), option 010 (uxtw) or 011 (lsl), S = 1
    let ok = w & 0xFFE0_0C00 == 0x7860_0800 && matches!(w >> 13 & 7, 2 | 3) && w >> 12 & 1 == 1;
    (ok && w >> 5 & 31 != 31).then_some((w >> 5 & 31, w >> 16 & 31))
}

/// `ldr x18, [xN|sp (, #imm)]` (unsigned offset, 64-bit)
pub fn ldr_x18_plain(w: u32) -> bool {
    w & 0xFFC0_001F == 0xF940_0012
}

/// `ldp x29, x30, [sp], #imm` with a non-negative immediate
pub fn ldp_fp_lr_post(w: u32) -> bool {
    w & 0xFFC0_7FFF == 0xA8C0_7BFD && w >> 21 & 1 == 0
}

/// A write-back amount to sp (pre-index `[sp, #i]!`, post-index `[sp], #i`), 0 otherwise (upstream's sp_delta)
pub fn sp_delta(w: u32) -> i64 {
    if w >> 5 & 31 != 31 {
        return 0;
    }
    let v = w >> 26 & 1;
    match w >> 27 & 7 {
        0b101 => {
            let ty = w >> 23 & 3;
            if ty != 1 && ty != 3 {
                return 0;
            }
            let opc = w >> 30;
            let scale = if v == 0 { if opc == 2 { 8 } else { 4 } } else { 4 << opc };
            sext(w >> 15 & 0x7F, 7) * scale
        }
        0b111 if w >> 24 & 1 == 0 && w >> 21 & 1 == 0 && matches!(w >> 10 & 3, 1 | 3) => sext(w >> 12 & 0x1FF, 9),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks() {
        // and w0, w1, #0x3f (llvm: 0x12001420); and x16, x18, #0xffffffff00000000 (0x92607e50)
        assert_eq!(and_imm(0x1200_1420), Some((0, 1, 0x3F)));
        assert_eq!(and_imm(0x9260_FE50 & !0x8000), Some((16, 18, 0xFFFF_FFFF_0000_0000)));
        assert_eq!(decode_bitmask(0, 0, 0x1E, 32), Some(0x7FFF_FFFF));
    }

    #[test]
    fn jump_table_forms() {
        // add x18, x17, x16 (0x8b100232); ldrsw x18, [x17, x0, lsl #2] (0xb8a07a32)
        assert_eq!(add_x18_xa_xb(0x8B10_0232), Some((17, 16)));
        assert_eq!(ldrsw_lsl2(0xB8A0_7A32), Some((18, 17, 0)));
        assert_eq!(cmp_imm(0x7100_141F, false), Some((0, 5))); // cmp w0, #5
        assert_eq!(mov_w(0x2A01_03E2), Some((2, 1))); // mov w2, w1
        assert_eq!(add_x18_lsl2(0x8B00_0A52 | 3 << 16), Some(3));
        assert_eq!(mnem(0xD61F_0240), M::Br);
        assert_eq!(mnem(0xD71F_0240), M::Other); // braa-family is not a plain br
    }

    #[test]
    fn targets() {
        assert_eq!(branch_target(0x1400_0002, 0x100), Some(0x108)); // b .+8
        assert_eq!(branch_target(0x5400_0040, 0x100), Some(0x108)); // b.eq .+8
        assert_eq!(branch_target(0xB400_0040, 0x100), Some(0x108)); // cbz x0, .+8
        assert_eq!(pcrel_target(0x1000_0081, 0x100), Some(0x110)); // adr x1, #16
        assert_eq!(pcrel_target(0x10FF_FFC2, 0x100), Some(0xF8)); // adr x2, #-8
        assert_eq!(pcrel_target(0xF0FF_FFE3, 0x5000), Some(0x4000)); // adrp x3, #-4096 (pages)
    }

    #[test]
    fn sp_writeback() {
        assert_eq!(sp_delta(0xA9BF_7BFD), -16); // stp x29, x30, [sp, #-16]!
        assert_eq!(sp_delta(0xA8C1_7BFD), 16); // ldp x29, x30, [sp], #16
        assert_eq!(sp_delta(0xF81F_0FE0), -16); // str x0, [sp, #-16]!
        assert_eq!(sp_delta(0xA93F_2BE9), 0); // stp x9, x10, [sp, #-16] (no writeback)
    }
}
