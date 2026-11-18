#!/usr/bin/env python3
"""Regenerates the 'Supported by upstream' section of docs/SUPPORTED_GAMES.md from the profiles in android/app/src/main/assets/games (tools/fetch_profiles.sh copies them)."""
import re, sys, pathlib
root = pathlib.Path(__file__).resolve().parent.parent
games = root / 'android/app/src/main/assets/games'
doc = root / 'docs/SUPPORTED_GAMES.md'
old = {m.group(2): m.group(1) for m in re.finditer(r'^\| (.+?) \| `([0-9A-F]{16})` \|', doc.read_text(encoding='utf-8'), re.M)}   # names the doc already has
rows = []; count = {}
for d in sorted(games.iterdir()):
    a = d / 'android.tsv'
    if not a.is_file(): continue
    kv = {}
    for ln in a.read_text(encoding='utf-8').splitlines():
        if ln and not ln.startswith('#'):
            k, _, v = ln.partition('\t'); kv.setdefault(k, v)
    g = (d / 'game.tsv').read_text(encoding='utf-8') if (d / 'game.tsv').is_file() else ''
    m = re.search(r'^# Game profile of (.+?) \(', g, re.M) or re.search(r'^# (.+?) on Android', a.read_text(encoding='utf-8'), re.M)
    name = old.get(d.name) or (m.group(1).strip() if m else d.name)
    st = kv.get('status', '?'); count[st] = count.get(st, 0) + 1
    need = ', '.join(x for x in [('BC textures' if 'bc' in kv.get('requires', '').lower() else '')] if x) or '-'
    rows.append((name, d.name, st, kv.get('tested', '-') or '-', need))
rows.sort(key=lambda r: r[0].lower())
head = f"## Supported by upstream ({len(rows)} games)\n\n" + ', '.join(f'{n} {s}' for s, n in sorted(count.items(), key=lambda x: -x[1])) + \
    ". Generated from upstream's Android profiles by `tools/make_games_doc.py`; \"tested on\" is what upstream recorded (game version and device), not a promise for every phone.\n\n| Game | Title ID | Status | Tested on | Needs |\n|---|---|---|---|---|\n"
body = ''.join(f'| {n} | `{i}` | {s} | {t} | {r} |\n' for n, i, s, t, r in rows)
s = doc.read_text(encoding='utf-8')
m = re.search(r'## Supported by upstream.*?(?=\n## |\Z)', s, re.S)
new = head + body
s = s[:m.start()] + new + s[m.end():] if m else s.rstrip() + '\n\n' + new
doc.write_text(s, encoding='utf-8'); print(len(rows), 'games')
