//! Which general-purpose registers an AArch64 instruction names, reads and writes, and where a branch goes. This is what the NSO rewriter needs from a disassembler
//! (upstream uses capstone and the operand text); it is not a full disassembler. Covers ARMv8.0-A with the FP / SIMD / crypto / CRC extensions, which is what Switch code uses;
//! SVE, SME and the later extensions' general-register forms are not decoded (`Insn::known` is false for words in those groups).
//!
//! Checked against `llvm-mc --disassemble` (the `a64dump` example and `tools/a64_oracle.py`): for every random word LLVM decodes, the set of `x`/`w` registers named in its text
//! must equal `Insn::used()`.

/// What an instruction does with one general register field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Read,
    Write,
    ReadWrite,
}

/// A general-register operand: the bit position of its 5-bit field and what the instruction does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    pub shift: u8,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Insn {
    pub word: u32,
    /// every general-register operand field (the ones that can name x0..x30; a field holding 31 is sp or the zero register)
    pub fields: Vec<Field>,
    /// the instruction writes sp (add/sub/logical immediate or extended-register form with Rd = 31 and no flags; a load or store with sp as a written-back base)
    pub dest_sp: bool,
    /// the instruction also uses registers its fields do not name (CASP, LDXP-style pairs name only the first of a pair for some forms): not rewritable by field substitution
    pub implied: bool,
    /// `bl` / `blr` write x30
    pub writes_lr: bool,
    /// the word is in a group this decoder covers (false: SVE, SME, reserved)
    pub known: bool,
}

fn f(shift: u8, role: Role) -> Field {
    Field { shift, role }
}
use Role::{Read as R, ReadWrite as RW, Write as W};

impl Insn {
    fn reg(&self, shift: u8) -> u32 {
        (self.word >> shift) & 31
    }

    /// x0..x30 named by an operand field, as a bit mask
    pub fn used(&self) -> u32 {
        self.fields.iter().fold(0, |m, fl| m | bit(self.reg(fl.shift)))
    }
    pub fn reads(&self) -> u32 {
        self.fields.iter().filter(|fl| fl.role != W).fold(0, |m, fl| m | bit(self.reg(fl.shift)))
    }
    pub fn writes(&self) -> u32 {
        let m = self.fields.iter().filter(|fl| fl.role != R).fold(0, |m, fl| m | bit(self.reg(fl.shift)));
        m | if self.writes_lr { 1 << 30 } else { 0 }
    }
    pub fn uses(&self, n: u32) -> bool {
        self.used() >> n & 1 != 0
    }

    /// The word with every operand field that names register `from` renamed to `to`.
    pub fn rename(&self, from: u32, to: u32) -> u32 {
        let mut w = self.word;
        for fl in &self.fields {
            if self.reg(fl.shift) == from {
                w = w & !(31 << fl.shift) | (to << fl.shift);
            }
        }
        w
    }
}

fn bit(r: u32) -> u32 {
    if r < 31 {
        1 << r
    } else {
        0
    }
}

/// Decodes the register fields of `w`.
pub fn decode(w: u32) -> Insn {
    let mut i = Insn { word: w, known: true, ..Default::default() };
    let op0 = w >> 25 & 0xF;
    match op0 {
        0b1000 | 0b1001 => dp_imm(&mut i),
        0b1010 | 0b1011 => branch_sys(&mut i),
        0b0100 | 0b0110 | 0b1100 | 0b1110 => ldst(&mut i),
        0b0101 | 0b1101 => dp_reg(&mut i),
        0b0111 | 0b1111 => simd_fp(&mut i),
        _ => i.known = false, // reserved, SVE, SME
    }
    i
}

fn dp_imm(i: &mut Insn) {
    let w = i.word;
    let s = w >> 29 & 1;
    match w >> 23 & 7 {
        0 | 1 => i.fields.push(f(0, W)), // adr, adrp
        2 | 3 => {
            // add / subtract (immediate); with tags (op 011) the same fields
            i.fields.push(f(0, W));
            i.fields.push(f(5, R));
            if s == 0 && w & 31 == 31 {
                i.dest_sp = true;
            }
        }
        4 => {
            // logical immediate: ands (opc 11) sets flags, so Rd = 31 is the zero register
            i.fields.push(f(0, W));
            i.fields.push(f(5, R));
            if w >> 29 & 3 != 3 && w & 31 == 31 {
                i.dest_sp = true;
            }
        }
        5 => i.fields.push(f(0, if w >> 29 & 3 == 3 { RW } else { W })), // move wide: movk keeps the other bits
        6 => {
            // bitfield: bfm (opc 01) keeps the destination's other bits
            i.fields.push(f(0, if w >> 29 & 3 == 1 { RW } else { W }));
            i.fields.push(f(5, R));
        }
        _ => {
            // extract
            i.fields.push(f(0, W));
            i.fields.push(f(5, R));
            i.fields.push(f(16, R));
        }
    }
}

fn branch_sys(i: &mut Insn) {
    let w = i.word;
    if w >> 26 == 0b000101 {
        return; // b
    }
    if w >> 26 == 0b100101 {
        i.writes_lr = true; // bl
        return;
    }
    if matches!(w >> 25 & 0x3F, 0b011010 | 0b011011) {
        i.fields.push(f(0, R)); // cbz / cbnz, tbz / tbnz
        return;
    }
    if w >> 25 == 0b0101010 {
        return; // b.cond
    }
    if w >> 22 == 0b1101010100 {
        // system: msr / mrs / sys / sysl carry Rt (hints, barriers and MSR immediate hold 31 there)
        let l = w >> 21 & 1;
        i.fields.push(f(0, if l == 1 { W } else { R }));
        return;
    }
    if w >> 25 == 0b1101011 {
        // unconditional branch (register): br, blr, ret, eret, drps and the pointer-authenticated forms
        let opc = w >> 21 & 15;
        i.fields.push(f(5, R));
        if opc == 1 || opc == 9 {
            i.writes_lr = true; // blr, blraa
        }
        if w >> 11 & 1 == 1 && (opc == 8 || opc == 9) {
            i.fields.push(f(0, R)); // braa / blraa with a modifier register (Z = 0)
        }
        return;
    }
    // exception generation (svc, hvc, smc, brk, hlt, dcps) and the rest: no general registers
}

fn ldst(i: &mut Insn) {
    let w = i.word;
    let v = w >> 26 & 1;
    let b29_24 = w >> 24 & 0x3F;
    // Advanced SIMD load / store structures
    if w >> 31 == 0 && (b29_24 == 0b001100 || b29_24 == 0b001101) {
        if w >> 23 & 1 == 1 {
            i.fields.push(f(5, RW)); // post-index: the base is written back
            i.fields.push(f(16, R)); // by register (31 = by the transfer size)
        } else {
            i.fields.push(f(5, R));
        }
        return;
    }
    // load / store exclusive, ordered, compare-and-swap
    if b29_24 == 0b001000 && v == 0 {
        let o2 = w >> 23 & 1; // 0 exclusive, 1 ordered / CAS
        let l = w >> 22 & 1;
        let o1 = w >> 21 & 1; // pair or CAS
        let o0 = w >> 15 & 1;
        let _ = o0;
        let size = w >> 30;
        i.fields.push(f(5, R));
        if o2 == 0 && o1 == 0 {
            // ldxr / stxr, ldaxr / stlxr: store-exclusive's status is Rs
            if l == 1 {
                i.fields.push(f(0, W));
            } else {
                i.fields.push(f(0, R));
                i.fields.push(f(16, W));
            }
        } else if o2 == 0 && o1 == 1 {
            // ldxp / stxp (only size 10 / 11): pair Rt, Rt2
            if l == 1 {
                i.fields.push(f(0, W));
                i.fields.push(f(10, W));
            } else {
                i.fields.push(f(0, R));
                i.fields.push(f(10, R));
                i.fields.push(f(16, W));
            }
        } else if o1 == 0 {
            // ldar / stlr / ldlar / stllr
            i.fields.push(f(0, if l == 1 { W } else { R }));
        } else if size >= 2 || true {
            // CAS family: compare reg Rs is read and overwritten, Rt is the new value; CASP uses Rs, Rs+1 and Rt, Rt+1 (bit 23 = 0 is excluded above, so 1)
            i.fields.push(f(16, RW));
            i.fields.push(f(0, R));
            if o2 == 0 {
                i.implied = true;
            }
        }
        return;
    }
    // RCPC: load-acquire / store-release with unscaled offset, and memory tags
    if b29_24 == 0b011001 && v == 0 {
        i.fields.push(f(5, R));
        let size = w >> 30;
        let tag = size == 3 && w >> 21 & 1 == 1;
        if !tag {
            i.fields.push(f(0, if w >> 22 & 3 == 0 { R } else { W }));
        } else {
            i.fields.push(f(0, R)); // stg / stzg / st2g / stz2g / ldg(Xt written)
            if w >> 22 & 3 == 1 {
                i.fields[1].role = W;
            }
        }
        return;
    }
    let b29_27 = w >> 27 & 7;
    let opc = w >> 30;
    match b29_27 {
        0b011 if w >> 24 & 3 == 0 => {
            // load register (literal): prfm holds a prefetch operation in Rt
            if v == 0 && opc != 3 {
                i.fields.push(f(0, W));
            }
        }
        0b101 => {
            // pairs: no-allocate, post-index, offset, pre-index
            let ty = w >> 23 & 3;
            let load = w >> 22 & 1;
            i.fields.push(f(5, R));
            if v == 0 {
                let r = if load == 1 { W } else { R };
                i.fields.push(f(0, r));
                i.fields.push(f(10, r));
            }
            if ty == 1 || ty == 3 {
                i.fields[0].role = RW; // writeback
                if w >> 5 & 31 == 31 {
                    i.dest_sp = true;
                }
            }
        }
        0b111 => {
            let unsigned = w >> 24 & 1 == 1;
            i.fields.push(f(5, R));
            let size = w >> 30;
            let opc2 = w >> 22 & 3;
            let prfm = v == 0 && size == 3 && opc2 == 2;
            if unsigned {
                if v == 0 && !prfm {
                    i.fields.push(f(0, if opc2 == 0 { R } else { W }));
                }
                return;
            }
            let b21 = w >> 21 & 1;
            let b1110 = w >> 10 & 3;
            let load = opc2 != 0;
            if b21 == 0 {
                // unscaled, post-index, unprivileged, pre-index
                if v == 0 && !prfm {
                    i.fields.push(f(0, if load { W } else { R }));
                }
                if b1110 == 1 || b1110 == 3 {
                    i.fields[0].role = RW;
                    if w >> 5 & 31 == 31 {
                        i.dest_sp = true;
                    }
                }
            } else if b1110 == 0 {
                if v == 0 {
                    // atomic memory operations (LDADD .. SWP, and the LDAPR / ST forms)
                    i.fields.push(f(16, R));
                    i.fields.push(f(0, W));
                }
            } else if b1110 == 2 {
                // register offset
                i.fields.push(f(16, R));
                if v == 0 && !prfm {
                    i.fields.push(f(0, if load { W } else { R }));
                }
            } else if v == 0 && size == 3 {
                // pointer-authenticated load (LDRAA / LDRAB): pre-index with writeback when bit 11 is set
                i.fields.push(f(0, W));
                if w >> 11 & 1 == 1 {
                    i.fields[0].role = RW;
                }
            }
        }
        _ => i.known = false,
    }
}

fn dp_reg(i: &mut Insn) {
    let w = i.word;
    match w >> 24 & 0x1F {
        0b01010 => {
            i.fields.push(f(0, W));
            i.fields.push(f(5, R));
            i.fields.push(f(16, R));
        }
        0b01011 => {
            // add / subtract (shifted register, extended register); the extended form with flags clear may write sp
            i.fields.push(f(0, W));
            i.fields.push(f(5, R));
            i.fields.push(f(16, R));
            if w >> 21 & 1 == 1 && w >> 29 & 1 == 0 && w & 31 == 31 {
                i.dest_sp = true;
            }
        }
        0b11010 => match w >> 21 & 7 {
            0 => {
                // add / subtract with carry; SETF8 / SETF16 / RMIF (opcode2 non-zero) name only Rn
                if w >> 10 & 0x3F == 0 && w >> 15 & 1 == 0 {
                    i.fields.push(f(0, W));
                    i.fields.push(f(5, R));
                    i.fields.push(f(16, R));
                } else {
                    i.fields.push(f(5, R));
                }
            }
            2 => {
                // conditional compare: register form (bit 11 = 0) names Rm, the immediate form an immediate
                i.fields.push(f(5, R));
                if w >> 11 & 1 == 0 {
                    i.fields.push(f(16, R));
                }
            }
            4 => {
                i.fields.push(f(0, W));
                i.fields.push(f(5, R));
                i.fields.push(f(16, R));
            }
            6 => {
                i.fields.push(f(0, W));
                i.fields.push(f(5, R));
                if w >> 30 & 1 == 0 {
                    i.fields.push(f(16, R)); // 2-source; 1-source's bits 20:16 are an opcode
                }
            }
            _ => i.known = false,
        },
        0b11011 => {
            i.fields.push(f(0, W));
            i.fields.push(f(5, R));
            i.fields.push(f(16, R));
            i.fields.push(f(10, R)); // Ra (smaddl, madd, ...; msub / umulh name only the used ones, 31 where unused)
        }
        _ => i.known = false,
    }
}

fn simd_fp(i: &mut Insn) {
    let w = i.word;
    // floating-point <-> integer conversion and fixed-point conversion: 
    if w >> 24 & 0x1F == 0b11110 && w >> 29 & 3 == 0 {
        let fixed = w >> 21 & 1 == 0;
        let opcode = w >> 16 & 7;
        let rmode = w >> 19 & 3;
        if fixed {
            // scvtf / ucvtf (fixed-point) read Rn; fcvtzs / fcvtzu write Rd
            match opcode {
                0 | 1 => i.fields.push(f(0, W)),
                2 | 3 => i.fields.push(f(5, R)),
                _ => {}
            }
        } else if w >> 10 & 0x3F == 0 {
            match opcode {
                0 | 1 | 4 | 5 => i.fields.push(f(0, W)),
                2 | 3 => i.fields.push(f(5, R)),
                6 => i.fields.push(f(0, W)),
                _ => {
                    if rmode == 0 || rmode == 1 {
                        i.fields.push(f(5, R)); // fmov Sd, Wn / Vd.d[1], Xn
                    } else {
                        i.fields.push(f(0, W)); // fjcvtzs
                    }
                }
            }
        }
        return;
    }
    // Advanced SIMD copy: dup (general), ins (general), smov, umov
    if w >> 31 == 0 && w >> 29 & 1 == 0 && w >> 21 & 0xFF == 0b01110000 && w >> 15 & 1 == 0 && w >> 10 & 1 == 1 {
        match w >> 11 & 0xF {
            1 | 3 => i.fields.push(f(5, R)),
            5 | 7 => i.fields.push(f(0, W)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x18_forms_and_rename() {
        // ldur w18, [x0]   mov x0, x18 (orr x0, xzr, x18)   add x18, x18, #1
        let a = decode(0xb8400012);
        assert_eq!((a.used(), a.writes(), a.reads()), (1 << 18 | 1, 1 << 18, 1));
        let m = decode(0xaa1203e0);
        assert!(m.uses(18) && m.writes() == 1 && m.reads() == 1 << 18);
        let add = decode(0x91000652);
        assert_eq!(add.rename(18, 9), 0x91000529); // add x9, x9, #1
        // str q18, [x0]: q18 is a vector register
        assert!(!decode(0x3d800012).uses(18));
        // svc #0: no registers; mrs x18, tpidr_el0
        assert_eq!(decode(0xd4000001).used(), 0);
        let mrs = decode(0xd53bd052);
        assert!(mrs.uses(18) && mrs.writes() == 1 << 18);
        // bl writes x30; br x18 reads x18
        assert!(decode(0x94000000).writes() == 1 << 30);
        assert_eq!(decode(0xd61f0240).reads(), 1 << 18);
        // stp x29, x30, [sp, #-16]! writes back sp
        let stp = decode(0xa9bf7bfd);
        assert!(stp.dest_sp && stp.used() == 1 << 29 | 1 << 30);
        // sub sp, sp, x0 (extended register) writes sp
        assert!(decode(0xcb2063ff).dest_sp);
    }
}
