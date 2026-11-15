//! AArch64 instruction encoders of the rewriter's stubs (upstream's `nso_asm.py`). A branch checks its reach.

pub fn b(frm: i64, to: i64) -> Result<u32, String> {
    let off = to - frm;
    if !(-(1 << 27)..(1 << 27)).contains(&off) || off % 4 != 0 {
        return Err(format!("branch from {frm:#x} to {to:#x} is out of reach"));
    }
    Ok(0x1400_0000 | ((off >> 2) as u32 & 0x03FF_FFFF))
}
pub fn stp(t: u32, t2: u32, n: u32, off: i32) -> u32 {
    0xA900_0000 | (((off / 8) as u32 & 0x7F) << 15) | (t2 << 10) | (n << 5) | t
}
pub fn ldp(t: u32, t2: u32, n: u32, off: i32) -> u32 {
    0xA940_0000 | (((off / 8) as u32 & 0x7F) << 15) | (t2 << 10) | (n << 5) | t
}
pub fn stp_pre(t: u32, t2: u32, n: u32, off: i32) -> u32 {
    0xA980_0000 | (((off / 8) as u32 & 0x7F) << 15) | (t2 << 10) | (n << 5) | t
}
pub fn ldp_post(t: u32, t2: u32, n: u32, off: i32) -> u32 {
    0xA8C0_0000 | (((off / 8) as u32 & 0x7F) << 15) | (t2 << 10) | (n << 5) | t
}
pub fn stur(t: u32, n: u32, off: i32) -> u32 {
    0xF800_0000 | ((off as u32 & 0x1FF) << 12) | (n << 5) | t
}
pub fn ldur(t: u32, n: u32, off: i32) -> u32 {
    0xF840_0000 | ((off as u32 & 0x1FF) << 12) | (n << 5) | t
}
pub fn ldrx(t: u32, n: u32, imm12: u32) -> u32 {
    0xF940_0000 | (imm12 << 10) | (n << 5) | t
}
pub fn strx(t: u32, n: u32, imm12: u32) -> u32 {
    0xF900_0000 | (imm12 << 10) | (n << 5) | t
}
pub fn mrs_tpidr(t: u32) -> u32 {
    0xD53B_D040 | t
}
pub fn adrp(dd: u32, pc: i64, target: i64) -> Result<u32, String> {
    let imm = ((target & !0xFFF) - (pc & !0xFFF)) >> 12;
    if !(-(1 << 20)..(1 << 20)).contains(&imm) {
        return Err(format!("adrp from {pc:#x} to {target:#x} is out of reach"));
    }
    Ok(0x9000_0000 | ((imm as u32 & 3) << 29) | (((imm >> 2) as u32 & 0x7FFFF) << 5) | dd)
}
pub fn addi(dd: u32, n: u32, imm12: u32, sh: u32) -> u32 {
    0x9100_0000 | (sh << 22) | (imm12 << 10) | (n << 5) | dd
}
pub fn addr(dd: u32, n: u32, m: u32) -> u32 {
    0x8B00_0000 | (m << 16) | (n << 5) | dd
}
pub fn subi(dd: u32, n: u32, imm12: u32, sh: u32) -> u32 {
    0xD100_0000 | (sh << 22) | (imm12 << 10) | (n << 5) | dd
}
pub fn br(n: u32) -> u32 {
    0xD61F_0000 | (n << 5)
}
pub fn blr(n: u32) -> u32 {
    0xD63F_0000 | (n << 5)
}
pub fn cmpr(n: u32, m: u32) -> u32 {
    0xEB00_001F | (m << 16) | (n << 5)
}
pub fn beq(frm: i64, to: i64) -> Result<u32, String> {
    let off = to - frm;
    if !(-(1 << 20)..(1 << 20)).contains(&off) || off % 4 != 0 {
        return Err(format!("b.eq from {frm:#x} to {to:#x} is out of reach"));
    }
    Ok(0x5400_0000 | (((off >> 2) as u32 & 0x7FFFF) << 5))
}
pub fn brk(imm: u32) -> u32 {
    0xD420_0000 | ((imm & 0xFFFF) << 5)
}
pub const NOP: u32 = 0xD503_201F;

#[cfg(test)]
mod tests {
    use super::*;
    // the expected words are LLVM's (llvm-mc --assemble)
    #[test]
    fn encodings_match_llvm() {
        assert_eq!(b(0x1000, 0x1004).unwrap(), 0x1400_0001);
        assert_eq!(b(0x1004, 0x1000).unwrap(), 0x17FF_FFFF);
        assert!(b(0, 1 << 28).is_err());
        assert_eq!(stp(9, 10, 31, -16), 0xA93F_2BE9);
        assert_eq!(ldp(9, 10, 31, -16), 0xA97F_2BE9);
        assert_eq!(stp_pre(29, 30, 31, -16), 0xA9BF_7BFD);
        assert_eq!(ldp_post(29, 30, 31, 16), 0xA8C1_7BFD);
        assert_eq!(stur(16, 31, -16), 0xF81F_03F0);
        assert_eq!(ldur(17, 31, -24), 0xF85E_83F1);
        assert_eq!(ldrx(9, 10, 2), 0xF940_0949);
        assert_eq!(strx(9, 10, 2), 0xF900_0949);
        assert_eq!(mrs_tpidr(9), 0xD53B_D049);
        assert_eq!(addi(1, 2, 3, 0), 0x9100_0C41);
        assert_eq!(addi(1, 2, 3, 1), 0x9140_0C41);
        assert_eq!(subi(1, 2, 3, 0), 0xD100_0C41);
        assert_eq!(addr(1, 2, 3), 0x8B03_0041);
        assert_eq!(br(16), 0xD61F_0200);
        assert_eq!(blr(30), 0xD63F_03C0);
        assert_eq!(cmpr(16, 17), 0xEB11_021F);
        assert_eq!(brk(0x18), 0xD420_0300);
        assert_eq!(NOP, 0xD503_201F);
        assert_eq!(adrp(1, 0, 0x4000).unwrap(), 0x9000_0021);
    }
}
