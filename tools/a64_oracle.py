#!/usr/bin/env python3
"""Compares crates/nx-a64 with LLVM: for random words LLVM disassembles, the x/w registers its text names must be nx-a64's `used()`.
Usage: tools/a64_oracle.py [count] [seed]   (needs llvm-mc and a built `cargo build -p nx-a64 --example a64dump`)"""
import random, re, subprocess, sys
n = int(sys.argv[1]) if len(sys.argv) > 1 else 200000
rnd = random.Random(int(sys.argv[2]) if len(sys.argv) > 2 else 1)
words = []
for _ in range(n):
    w = rnd.getrandbits(32)
    if rnd.random() < .5: w = w & ~(31 << rnd.choice((0, 5, 10, 16))) | (18 << rnd.choice((0, 5, 10, 16)))   # favour x18
    words.append(w)
inp = '\n'.join(' '.join(f'0x{(w >> s) & 255:02x}' for s in (0, 8, 16, 24)) for w in words) + '\n'
r = subprocess.run(['llvm-mc', '--disassemble', '--triple=aarch64', '-mattr=+crypto,+crc,+fp-armv8,+neon', '--show-encoding'], input=inp, capture_output=True, text=True)
dec = {}
for m in re.finditer(r'^\s*(\S.*?)\s*// encoding: \[(0x[0-9a-f]{2}),(0x[0-9a-f]{2}),(0x[0-9a-f]{2}),(0x[0-9a-f]{2})', r.stdout, re.M):
    b = [int(x, 16) for x in m.groups()[1:]]
    dec[b[0] | b[1] << 8 | b[2] << 16 | b[3] << 24] = m.group(1)
rows = subprocess.run(['target/debug/examples/a64dump'], input='\n'.join(f'{w:08x}' for w in dec) + '\n', capture_output=True, text=True).stdout.split('\n')
mism = {}
for line in rows:
    if not line: continue
    w, used, reads, writes, dsp, known = line.split()
    w = int(w, 16); used = int(used, 16); t = dec[w]
    want = 0
    for m in re.finditer(r'\b[xw](\d+)\b', t): want |= 1 << int(m.group(1)) if int(m.group(1)) < 31 else 0
    # LLVM's text hides registers that are 'sp' / zero register (31, not tracked) and names registers of pairs implicitly
    if want != used:
        mn = t.split()[0]
        mism.setdefault(mn, []).append((w, t, f'{used:08x}', f'{want:08x}'))
# writes: a rough reading of LLVM's text (first operand written for most instructions; loads write the registers before '['; stores write only a store-exclusive's status;
# writeback writes the base), to catch role mistakes
NOWRITE = ('cmp', 'cmn', 'tst', 'ccmp', 'ccmn', 'cbz', 'cbnz', 'tbz', 'tbnz', 'b', 'br', 'blr', 'ret', 'msr', 'sys', 'dc', 'ic', 'tlbi', 'at', 'braa', 'brab', 'blraa', 'blrab', 'setf8', 'setf16', 'rmif', 'prfm', 'prfum', 'stg', 'stzg', 'st2g', 'stz2g')
def expect_writes(t):
    mn = t.split()[0]; ops = t[len(mn):].split('//')[0].strip()
    regs = lambda x: {int(m.group(1)) for m in re.finditer(r'\b[xw](\d+)\b', x) if int(m.group(1)) < 31}
    out = set()
    if mn in ('bl', 'blr', 'blraa', 'blrab', 'blraaz', 'blrabz'): out.add(30)
    if mn in NOWRITE or mn.startswith('b.') or mn.startswith('bc.'): return out
    if '[' in ops:
        pre, _, post = ops.partition('[')
        if mn.startswith(('st', 'ldadd', 'ldclr', 'ldeor', 'ldset', 'ldsmax', 'ldsmin', 'ldumax', 'ldumin', 'swp', 'cas', 'ldapr', 'ldaxp', 'ldxp', 'ldxr', 'ldar', 'ldlar', 'ldp', 'ldn', 'ldur', 'ldr', 'ldt', 'lda', 'ldg')):
            if mn.startswith(('stxr', 'stlxr', 'stxp', 'stlxp', 'stxrb', 'stlxrb', 'stxrh', 'stlxrh')): out |= regs(pre.split(',')[0])
            elif mn.startswith(('ldadd', 'ldclr', 'ldeor', 'ldset', 'ldsmax', 'ldsmin', 'ldumax', 'ldumin', 'swp')): out |= regs(pre.split(',')[1]) if pre.count(',') >= 2 else set()
            elif mn.startswith('cas'): out |= regs(pre.split(',')[0])
            elif mn.startswith('st'): pass
            else: out |= regs(pre)
        if ']!' in ops or re.search(r'\],\s*(#|x)', ops): out |= regs(post.split(']')[0].split(',')[0])
        return out
    return out | regs(ops.split(',')[0])
wr = {}
for line in rows:
    if not line: continue
    w, used, reads, writes, dsp, known = line.split(); w = int(w, 16)
    e = expect_writes(dec[w])
    if int(writes, 16) != sum(1 << r for r in e) and not dec[w].startswith(('casp', 'ldxp', 'ldaxp', 'ldr\t', 'ldrsw')):
        wr.setdefault(dec[w].split()[0], []).append((w, dec[w], writes, sum(1 << r for r in e)))
    elif int(writes, 16) != sum(1 << r for r in e): wr.setdefault(dec[w].split()[0], []).append((w, dec[w], writes, sum(1 << r for r in e)))
print(f'writes: {sum(len(v) for v in wr.values())} differ from the text reading in {len(wr)} mnemonics')
for mn, v in sorted(wr.items(), key=lambda kv: -len(kv[1]))[:25]:
    w, t, a, b = v[0]; print(f'  {mn:10} x{len(v):<6} e.g. {w:08x}  {t}   mine={a} text={b:08x}')
total = sum(len(v) for v in mism.values())
print(f'{len(dec)} words decoded by LLVM, {total} mismatches in {len(mism)} mnemonics')
for mn, v in sorted(mism.items(), key=lambda kv: -len(kv[1]))[:40]:
    w, t, u, wn = v[0]
    print(f'{mn:10} x{len(v):<6} e.g. {w:08x}  {t}   mine={u} llvm={wn}')
sys.exit(1 if total else 0)
