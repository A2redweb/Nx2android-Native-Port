#!/usr/bin/env python3
"""Writes the type-tree layouts upstream's unityfs.py keeps for players that ship none (crates/nx-shader/data/unity/): one file per layout and known.tsv (class, type hash, root type, file).
Usage: tools/gen_unity_layouts.py <upstream checkout>"""
import os, sys
up = sys.argv[1]
sys.path.insert(0, os.path.join(up, 'nxnative/dm/gfx/tools'))
import unityfs
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), '../crates/nx-shader/data/unity')
os.makedirs(out, exist_ok=True)
names = {}
for k, v in vars(unityfs).items():
    if isinstance(v, str) and k.isupper() and (k.startswith('SHADER_') or k.startswith('COMPUTE_')) and not k.startswith('_'):
        names[id(v)] = k
rows = []
for (cid, h), (root, layout) in unityfs.KNOWN_TREES.items():
    n = names[id(layout)].lower()
    open(os.path.join(out, n + '.txt'), 'w').write(layout)
    rows.append(f'{cid}\t{h.hex()}\t{root}\t{n}.txt')
open(os.path.join(out, 'known.tsv'), 'w').write('\n'.join(rows) + '\n')
print(len(rows), 'layouts')
