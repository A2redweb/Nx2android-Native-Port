#!/usr/bin/env python3
"""Makes a per-game gamepad pack (the artboards upstream's devtools/theme_render.py renders into atlases) from upstream's plain ergonomic pack: the same controls in the same
places, with an action icon beside each button's letter. The letter stays because the game's own prompts show it. usage: make_pack.py <upstream ergo dir> <out dir> <game>"""
import re, sys
from pathlib import Path

def svg(path, w=22):
    return (f'<svg aria-hidden="true" width="{w}" height="{w}" viewBox="0 0 24 24" fill="none" stroke="#ffffff" stroke-width="2" stroke-linecap="round" '
            f'stroke-linejoin="round" style="position: relative; flex: none">{path}</svg>')

# 24x24 line icons, one per action
ICON = {
    'hand':    '<path d="M8 13V6.5a1.5 1.5 0 0 1 3 0V11M11 10V4.5a1.5 1.5 0 0 1 3 0V10M14 10V6.5a1.5 1.5 0 0 1 3 0V15c0 3.5-2 6-5.5 6-3 0-4.5-1.5-5.5-4l-1.5-3.5a1.5 1.5 0 0 1 2.7-1.2L8 13"/>',
    'dash':    '<path d="M5 6l6 6-6 6M12 6l6 6-6 6"/>',
    'bag':     '<path d="M6 8h12l1 12H5L6 8zM9 8V6a3 3 0 0 1 6 0v2"/>',
    'roll':    '<path d="M20 12a8 8 0 1 1-3-6.2M20 4v5h-5"/>',
    'lock':    '<circle cx="12" cy="12" r="6"/><circle cx="12" cy="12" r="1.5"/><path d="M12 2v4M12 18v4M2 12h4M18 12h4"/>',
    'crouch':  '<path d="M12 3v10M8 9l4 4 4-4M6 19h12"/>',
    'mega':    '<path d="M12 3l2.5 6.5L21 12l-6.5 2.5L12 21l-2.5-6.5L3 12l6.5-2.5L12 3z"/>',
}
# game -> {button's aria-label: icon}
GAMES = {
    # Pokémon Legends: Z-A (the button map of the Switch game: B dash, Y roll, X menu, ZL focus on a target, L3 crouch, R3 Mega Evolve; A confirms and interacts)
    'pokemonza': {'A button': 'hand', 'B button': 'dash', 'X button': 'bag', 'Y button': 'roll', 'ZL trigger': 'lock', 'L3, left stick click': 'crouch', 'R3, right stick click': 'mega'},
}

def convert(src, icons):
    def one(m):
        label = m.group(1)
        if label not in icons: return m.group(0)
        body = m.group(0)
        ic = icons[label]
        tm = re.search(r'<span style="position: relative; ">([A-Z0-9]+)</span>', body)
        if not tm: raise SystemExit(f'no label span in {label}')
        t = tm.group(1)
        wide = label.endswith('trigger') or label.endswith('button') and t in ('L', 'R')   # a pill: icon after the letter
        if label in ('ZL trigger',):
            new = f'<span style="position: relative; display: flex; align-items: center; justify-content: center; width: 100%; gap: 6px; line-height: 1">{t}{svg(ICON[ic], 18)}</span>'
        elif label.endswith('click'):
            new = f'<span style="position: relative; display: flex; flex-direction: column; align-items: center; gap: 0; line-height: 1; font-size: 10px">{svg(ICON[ic], 15)}{t}</span>'
        else:
            new = f'<span style="position: relative; display: flex; flex-direction: column; align-items: center; gap: 0; line-height: 1; font-size: 17px">{svg(ICON[ic], 24)}{t}</span>'
        return body.replace(tm.group(0), new)
    return re.sub(r'<button[^>]*aria-label="([^"]+)"[^>]*>.*?</button>', one, src, flags=re.S)

def main():
    ergo, out, game = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
    out.mkdir(parents=True, exist_ok=True)
    for f in ('phone.dc.html', 'tablet.dc.html'):
        (out / f).write_text(convert((ergo / f).read_text(), GAMES[game]))
    print('wrote', out)

if __name__ == '__main__': main()
