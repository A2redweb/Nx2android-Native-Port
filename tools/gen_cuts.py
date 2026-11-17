#!/usr/bin/env python3
"""Converts upstream's registered patches (dm/tools/sdk_patches.py and dm/games/*/patches.py) into data the app reads:
  cuts.tsv     build ID, offset, original bytes (hex), new bytes (hex), reason: one cut per line
  framestep.tsv  build ID and the frame step stubs of FRAME_STEP, recorded as ops (see record_frame_step): `site`, `emit`, `put`, `render_scale`
  skipped.tsv  build ID, kind (OBS), count: registered patches the Rust rewriter does not apply yet (OBS are diagnostics only: skipping them changes no behaviour)
Run at build time (tools/fetch_profiles.sh); the app itself carries no Python. Usage: gen_cuts.py <upstream checkout> <out dir>"""
import glob, importlib.util, os, sys, types

up, out = sys.argv[1], sys.argv[2]
dm = os.path.join(up, 'nxnative', 'dm')
files = [os.path.join(dm, 'tools', 'sdk_patches.py')] + sorted(glob.glob(os.path.join(dm, 'games', '*', 'patches.py')))
sys.path.insert(0, os.path.join(dm, 'tools'))   # patch files import nso_asm
STUB, STATE = 0x40000000, 0x50000000   # stand-ins for the patch area's next address and the state page, resolved by the rewriter
import nso_asm


class Rel:
    """A pc-relative word that reaches into the patch area or the state page: kept symbolic (name, arguments, bits or-ed in afterwards)."""

    def __init__(self, name, args, extra=0):
        self.name, self.args, self.extra = name, args, extra

    def __or__(self, m):
        return Rel(self.name, self.args, self.extra | m)

    def text(self):
        return ':'.join([self.name] + [f'{x:#x}' for x in self.args]) + (f'|{self.extra:#x}' if self.extra else '')


def _wrap(name):
    real = getattr(nso_asm, name)

    def f(*a):
        targets = a[1:] if name == 'ADRP' else a   # ADRP(rd, pc, target): rd is a register number
        return Rel(name, a) if any(x >= STUB for x in targets) else real(*a)
    return f


for _n in ('B', 'ADRP', 'BEQ'):   # the patch area's and the state page's place is only known when the module is rewritten
    setattr(nso_asm, _n, _wrap(_n))


def record_frame_step(fn):
    """Runs a build's FRAME_STEP function against a recording context; returns its ops as tsv fields."""
    ops, n = [], [0]

    def word(w):
        if isinstance(w, Rel):
            return w.text()
        return f'{w & 0xFFFFFFFF:#010x}'

    def here():
        return STUB + 4 * n[0]

    def emit(w):
        ops.append(('emit', word(w)))
        n[0] += 1
    ctx = types.SimpleNamespace(here=here, emit=emit, state=STATE, render_scale=0,
                                site=lambda off, orig: ops.append(('site', f'{off:#x}', orig)),
                                put=lambda off, words, why: ops.append(('put', f'{off:#x}', ' '.join(word(w) for w in words), ' '.join(str(why).split()))))
    fn(ctx)
    if ctx.render_scale:
        ops.append(('render_scale', '1'))
    return ops


cuts, skipped, owner, frame_steps = {}, [], {}, {}
for n, path in enumerate(files):
    spec = importlib.util.spec_from_file_location(f'patches_{n}', path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    for bid, v in getattr(m, 'CUTS', {}).items():
        assert bid not in cuts, f'CUTS of build {bid} in both {owner[bid]} and {path}'
        cuts[bid], owner[bid] = v, path
    for bid, fn in getattr(m, 'FRAME_STEP', {}).items():
        frame_steps[bid] = record_frame_step(fn)
    for bid, v in getattr(m, 'OBS', {}).items():
        skipped.append((bid, 'OBS', len(v) if hasattr(v, '__len__') else 1))
os.makedirs(out, exist_ok=True)
n = 0
with open(os.path.join(out, 'cuts.tsv'), 'w') as f:
    for bid in sorted(cuts):
        for off, ob, nb, why in cuts[bid]:
            assert len(ob) == len(nb) and len(ob) % 2 == 0, (bid, hex(off))
            f.write(f'{bid}\t{off:#x}\t{ob}\t{nb}\t{" ".join(str(why).split())}\n')
            n += 1
with open(os.path.join(out, 'framestep.tsv'), 'w') as f:
    for bid in sorted(frame_steps):
        for op in frame_steps[bid]:
            f.write(bid + '\t' + '\t'.join(op) + '\n')
with open(os.path.join(out, 'skipped.tsv'), 'w') as f:
    for bid, kind, c in sorted(skipped):
        f.write(f'{bid}\t{kind}\t{c}\n')
print(f'cuts: {n} in {len(cuts)} builds; frame steps: {len(frame_steps)} builds; skipped (not applied): {len(skipped)} OBS tables')
