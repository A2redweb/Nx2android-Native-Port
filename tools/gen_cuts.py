#!/usr/bin/env python3
"""Converts upstream's registered patches (dm/tools/sdk_patches.py and dm/games/*/patches.py) into data the app reads:
  cuts.tsv     build ID, offset, original bytes (hex), new bytes (hex), reason: one cut per line
  skipped.tsv  build ID, kind (OBS, FRAME_STEP), count: registered patches the Rust rewriter does not apply yet
Run at build time (tools/fetch_profiles.sh); the app itself carries no Python. Usage: gen_cuts.py <upstream checkout> <out dir>"""
import glob, importlib.util, os, sys

up, out = sys.argv[1], sys.argv[2]
dm = os.path.join(up, 'nxnative', 'dm')
files = [os.path.join(dm, 'tools', 'sdk_patches.py')] + sorted(glob.glob(os.path.join(dm, 'games', '*', 'patches.py')))
sys.path.insert(0, os.path.join(dm, 'tools'))   # patch files import nso_asm
cuts, skipped, owner = {}, [], {}
for n, path in enumerate(files):
    spec = importlib.util.spec_from_file_location(f'patches_{n}', path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    for bid, v in getattr(m, 'CUTS', {}).items():
        assert bid not in cuts, f'CUTS of build {bid} in both {owner[bid]} and {path}'
        cuts[bid], owner[bid] = v, path
    for kind in ('OBS', 'FRAME_STEP'):
        for bid, v in getattr(m, kind, {}).items():
            skipped.append((bid, kind, len(v) if hasattr(v, '__len__') else 1))
os.makedirs(out, exist_ok=True)
n = 0
with open(os.path.join(out, 'cuts.tsv'), 'w') as f:
    for bid in sorted(cuts):
        for off, ob, nb, why in cuts[bid]:
            assert len(ob) == len(nb) and len(ob) % 2 == 0, (bid, hex(off))
            f.write(f'{bid}\t{off:#x}\t{ob}\t{nb}\t{" ".join(str(why).split())}\n')
            n += 1
with open(os.path.join(out, 'skipped.tsv'), 'w') as f:
    for bid, kind, c in sorted(skipped):
        f.write(f'{bid}\t{kind}\t{c}\n')
print(f'cuts: {n} in {len(cuts)} builds; skipped (not applied yet): {len(skipped)} OBS / FRAME_STEP tables')
