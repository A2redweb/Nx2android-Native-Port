#!/usr/bin/env python3
"""Every game the porter knows, taken from upstream's own `porter profiles --all --json` (so a game upstream adds shows up here by itself, with no change in this app),
plus what this app adds on top of upstream for a few games, and the advice card for a game folder (from `porter inspect --json`)."""
import json, re
from pathlib import Path

import porter_sync

# What this app adds for a game on top of upstream (shown on the game's card). Keyed by title ID, or by the game name in lower case letters and digits when no title ID is known.
APP_ADDS = {
    '010055D009F78000': {'needs_adb': True, 'note': 'Upstream has no Android profile and cannot make this game\'s run-time shaders without a Mac: this app makes them on your phone over adb. Needs the base game plus update 1.2.0, "Experimental" and adb connected.'},
    'runefactoryguardiansofazuma': {'name': 'Rune Factory: Guardians of Azuma', 'needs_adb': False, 'note': 'No upstream profile. Port with "Experimental". Runs at 10-15 fps handheld at 720p on a Snapdragon 7 class phone; this app sets DM_VK_ASYNC_PIPE=0 for it, which measured as a clear improvement.'},
}
ORDER = {'playable': 0, 'boots': 1, 'wip': 2, 'experimental': 3, 'ios only': 4}
_CACHE = {'key': None, 'data': None}


def slug(name): return re.sub(r'[^a-z0-9]', '', (name or '').lower())


def _note(g):
    return APP_ADDS.get(g['id'].upper()) or APP_ADDS.get(slug(g['name'])) or {}


def load(porter, force=False):
    """{'commit', 'games': [...], 'counts': {...}}. Cached per porter commit; porter(*args, timeout=) -> (stdout, stderr, code)."""
    key = porter_sync.commit()
    if not force and _CACHE['key'] == key and _CACHE['data'] is not None: return _CACHE['data']
    out, err, code = porter('profiles', '--all', '--json', timeout=180)
    try: rows = json.loads(out)
    except ValueError: raise RuntimeError('the porter could not list its games: ' + (err or out)[-300:])
    games = []
    for r in rows:
        g = dict(id=str(r.get('id', '')), name=str(r.get('name', '')), status=str(r.get('status', '')), tested=str(r.get('tested', '')), android=bool(r.get('android')))
        n = _note(g)
        g['app_note'] = n.get('note', ''); g['needs_adb'] = bool(n.get('needs_adb'))
        games.append(g)
    for k, n in APP_ADDS.items():   # games with no profile upstream at all (the key is a name, not a title ID)
        if not re.fullmatch(r'[0-9A-F]{16}', k) and not any(slug(g['name']) == k for g in games):
            games.append(dict(id='', name=n.get('name', k), status='experimental', tested='', android=False, app_note=n['note'], needs_adb=bool(n.get('needs_adb'))))
    games.sort(key=lambda g: (ORDER.get(g['status'], 9), g['name'].lower()))
    counts = {}
    for g in games: counts[g['status']] = counts.get(g['status'], 0) + 1
    data = {'commit': key, 'games': games, 'counts': counts, 'total': len(games)}
    _CACHE.update(key=key, data=data)
    return data


def advice(info, catalog=None):
    """The card shown after a game folder is picked. info: what `porter inspect --json` printed."""
    if not isinstance(info, dict) or info.get('error'):
        return {'ok': False, 'headline': 'Cannot read this folder', 'detail': (info or {}).get('error', '') if isinstance(info, dict) else '', 'can_port': False, 'experimental': False, 'needs_adb': False, 'notes': []}
    tid = str(info.get('title_id', '')).upper(); name = info.get('name') or tid
    android = info.get('android') or {}
    n = APP_ADDS.get(tid) or APP_ADDS.get(slug(name)) or {}
    notes = []
    if info.get('reason'): notes.append(info['reason'])
    if n.get('note'): notes.append(n['note'])
    if info.get('compressed'): notes.append('NSZ/XCZ files are decompressed during the port (needs extra free space).')
    if info.get('dlc'): notes.append(f"{len(info['dlc'])} DLC file(s) found and included.")
    if info.get('updates'): notes.append('Update: ' + ', '.join(str(u.get('version')) for u in info['updates'] if u.get('version')))
    supported = bool(info.get('supported'))
    if supported: head = f"{name}: Android {android.get('status', 'supported')}" + (f", tested on {android['tested']}" if android.get('tested') else '')
    elif info.get('profile') or n: head = f'{name}: no Android profile in upstream yet'
    else: head = f'{tid}: not known to the porter'
    return {'ok': True, 'title_id': tid, 'name': name, 'headline': head, 'status': android.get('status', ''), 'can_port': supported, 'experimental': bool(info.get('experimental')) and not supported,
            'needs_adb': bool(n.get('needs_adb')), 'notes': notes}
