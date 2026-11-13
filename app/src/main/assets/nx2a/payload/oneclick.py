#!/usr/bin/env python3
"""One-click setup. Everything the old Setup tab asked for, one step after another, each safe to repeat:
free space, Ubuntu build tools, the porter (upstream's newest, patched), the ready-made .NET tools, the Android NDK, prod.keys (found on its own where Switch tools keep it),
the output folder, adb (found and reconnected on its own once paired), and a final check with the porter's own `doctor`.
check() only looks (nothing is downloaded); run() fixes what is missing. A step that needs the person (prod.keys not found, adb not paired) says so and never blocks the others."""
import json, os, shutil, subprocess, sys, threading, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import adb_auto, fast_tools, porter_sync, prebuilt   # noqa: E402

MIN_FREE_GB = 4
MARK = Path(os.environ.get('NX2A_SETUP_MARK', '/root/.nx2a_setup'))
KEY_PLACES = ['/sdcard/switch/prod.keys', '/sdcard/Switch/prod.keys', '/sdcard/Download/prod.keys', '/sdcard/Downloads/prod.keys', '/sdcard/Documents/prod.keys', '/sdcard/prod.keys',
              '/sdcard/.switch/prod.keys', '/sdcard/Android/data/org.yuzu.yuzu_emu/files/keys/prod.keys', '/sdcard/yuzu/keys/prod.keys', '/sdcard/Ryujinx/system/prod.keys']
DEFAULT_OUT = '/sdcard/Switch/Ports'
NETWORK = {'system', 'porter', 'dotnet', 'ndk'}   # steps that download: tried up to three times
S = {'running': False, 'steps': [], 'ready': False, 'log': [], 'seq': 0, 'started': 0.0, 'finished': 0.0}
LOCK = threading.Lock()


class Ctx:
    """What the steps need from the GUI server: porter(*args, timeout=) -> (out, err, code); cfg() -> dict; env; busy() -> bool (a port is running)."""
    def __init__(self, porter, cfg, env, busy):
        self.porter, self.cfg, self.env, self.busy = porter, cfg, env, busy
        self.fix = False

    def log(self, line):
        with LOCK:
            S['log'].append(str(line)[:300]); del S['log'][:-400]


def _run(c, cmd, timeout=3600):
    """Runs a command, copies its output into the setup log; returns the exit code."""
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors='replace', env=c.env)
    t = threading.Timer(timeout, p.kill); t.start()
    try:
        for ln in p.stdout:
            if ln.strip(): c.log(ln.rstrip())
        return p.wait()
    finally: t.cancel()


def find_keys():
    """prod.keys in the usual places, then a shallow look through internal storage (never inside Android/)."""
    for p in KEY_PLACES:
        if Path(p).is_file(): return p
    root = Path('/sdcard'); seen = 0
    try:
        for d, dirs, files in os.walk(root):
            dirs[:] = [x for x in dirs if x != 'Android' and not x.startswith('.')]
            if len(Path(d).relative_to(root).parts) >= 3: dirs[:] = []
            if 'prod.keys' in files: return str(Path(d) / 'prod.keys')
            seen += 1
            if seen > 4000: break
    except OSError: pass
    return ''


def _setup_numbers():
    try: have = int(MARK.read_text().strip())
    except (OSError, ValueError): have = 0
    try: want = int(json.loads((HERE / 'payload.json').read_text()).get('setup', 1))
    except (OSError, ValueError): want = 1
    return have, want


# ---- steps: each returns (status, detail); status is ok | todo (check only) | needs (the person) | warn | fail ----

def s_space(c):
    free = shutil.disk_usage('/root').free / 2**30
    if free < MIN_FREE_GB: return 'fail', f'{free:.1f} GB free; setup needs about {MIN_FREE_GB} GB, and each port about 6 GB plus twice the game. Free some space and try again.'
    return 'ok', f'{free:.0f} GB free'


def s_system(c):
    missing = [t for t in ('clang', 'ld.lld', 'glslc', 'adb') if not shutil.which(t)]
    have, want = _setup_numbers()
    if not missing and have >= want: return 'ok', 'Ubuntu build tools installed'
    why = ('missing ' + ', '.join(missing)) if missing else 'setup is newer than what is installed'
    if not c.fix: return 'todo', f'will install the build tools ({why})'
    c.log(f'installing the Ubuntu build tools ({why})...')
    rc = _run(c, ['bash', str(HERE / 'inner_setup.sh')])
    missing = [t for t in ('clang', 'ld.lld', 'glslc', 'adb') if not shutil.which(t)]
    if rc != 0 or missing: return 'fail', f'install failed (exit {rc})' + (', still missing ' + ', '.join(missing) if missing else '') + '. Check the connection and set up again.'
    return 'ok', 'Ubuntu build tools installed'


def s_porter(c):
    if porter_sync.complete() and not c.fix: return 'ok', f'porter {porter_sync.commit()} ({porter_sync.mode()})'
    if not c.fix: return 'todo', 'will download the porter'
    ok, msg = porter_sync.sync(log=c.log)
    return ('ok' if ok else 'fail'), msg


def s_dotnet(c):
    if prebuilt.active(): return 'ok', prebuilt.state()
    if not porter_sync.complete(): return 'todo', 'needs the porter first'
    if not c.fix: return 'todo', 'will download the ready-made .NET tools (about 6 MB)'
    for ln in prebuilt.install(): c.log(ln)
    return ('ok', prebuilt.state()) if prebuilt.active() else ('fail', 'the ready-made .NET tools did not start on this phone (see the log)')


def s_ndk(c):
    if fast_tools.have_ndk(): return 'ok', 'Android NDK (trimmed) installed'
    if not c.fix: return 'todo', 'will download the trimmed Android NDK'
    try: fast_tools.seed_ndk()
    except Exception as e: return 'fail', f'trimmed NDK not downloaded: {e}'
    return ('ok', 'Android NDK (trimmed) installed') if fast_tools.have_ndk() else ('fail', 'NDK unpacked but not found')


def s_keys(c):
    k = c.cfg().get('keys')
    if k and Path(k).is_file(): return 'ok', k
    found = find_keys()
    if not found: return 'needs', 'prod.keys not found. Choose the file you dumped from your own Switch.'
    if not c.fix: return 'todo', f'found {found}: will use it'
    out, err, code = c.porter('config', 'keys', found)
    return ('ok', found) if code == 0 else ('fail', (err or out)[-200:])


def s_output(c):
    out = c.cfg().get('out') or DEFAULT_OUT
    if Path(out).is_dir() and c.cfg().get('out'): return 'ok', out
    if not c.fix: return 'todo', f'will use {out}'
    try: Path(out).mkdir(parents=True, exist_ok=True)
    except OSError as e: return 'fail', f'cannot create {out}: {e}'
    o, e, code = c.porter('config', 'out', out)
    return ('ok', out) if code == 0 else ('fail', (e or o)[-200:])


def s_adb(c):
    if not shutil.which('adb'): return 'todo', 'adb comes with the build tools'
    ok, msg = adb_auto.auto(c.env)
    if ok: return 'ok', msg
    paired = bool(adb_auto.load().get('paired'))
    return 'warn', ('Not connected: turn on Wireless debugging and the app reconnects by itself. ' if paired else
                    'Optional, one time: pair wireless debugging (Device tab). Needed for games whose shaders are made on the phone and for copying game data. ') + msg


def s_check(c):
    if not porter_sync.complete(): return 'todo', 'needs the porter first'
    out, err, code = c.porter('doctor', '--json', timeout=120)
    try: rows = json.loads(out)
    except ValueError: return 'fail', (err or out)[-300:] or 'the porter check failed'
    bad = [r for r in rows if not r.get('ok') and r.get('required')]
    # keys and the tools this app installs itself are judged by their own steps above
    bad = [r for r in bad if r.get('name') not in ('prod.keys',)]
    if bad: return ('fail' if c.fix else 'todo'), '; '.join(f"{r['name']}: {r.get('detail', '')}" for r in bad)[:400]
    return 'ok', 'the porter\'s own check passes'


def s_games(c):
    if not porter_sync.complete(): return 'todo', 'needs the porter first'
    try:
        import profiles_catalog
        d = profiles_catalog.load(c.porter, force=c.fix)
    except Exception as e: return 'warn', str(e)
    return 'ok', f"{d['total']} games known ({d['counts'].get('playable', 0)} playable)"


STEPS = [  # id, title, required for "ready"
    ('space', 'Free space', True, s_space), ('system', 'Ubuntu build tools', True, s_system), ('porter', 'Porter (upstream newest)', True, s_porter),
    ('dotnet', 'Ready-made .NET tools', True, s_dotnet), ('ndk', 'Android NDK', True, s_ndk), ('keys', 'prod.keys', True, s_keys),
    ('output', 'Output folder', True, s_output), ('adb', 'adb (wireless debugging)', False, s_adb), ('check', 'Final check', True, s_check), ('games', 'Game list', False, s_games)]


def _blank():
    return [dict(id=i, title=t, required=r, status='pending', detail='') for i, t, r, _ in STEPS]


def _go(c, fix):
    c.fix = fix
    with LOCK:
        S.update(running=True, ready=False, steps=_blank(), seq=S['seq'] + 1, started=time.time(), finished=0.0)
        if fix: S['log'].clear()
    stop = False
    for idx, (sid, _, req, fn) in enumerate(STEPS):
        st = S['steps'][idx]; st['status'] = 'running'
        tries = 3 if (fix and sid in NETWORK) else 1
        for n in range(tries):
            try: res = fn(c)
            except Exception as e: res = ('fail', f'{type(e).__name__}: {e}')
            if res[0] in ('ok', 'needs', 'todo', 'warn') or n == tries - 1: break
            c.log(f'{sid}: {res[1]} (trying again)'); time.sleep(5 * (n + 1))
        st['status'], st['detail'] = res
        if res[0] == 'fail' and sid in ('space', 'system', 'porter'): stop = True   # nothing after these can work
        if stop:
            for rest in S['steps'][idx + 1:]: rest['status'], rest['detail'] = 'pending', 'waiting for the step above'
            break
    with LOCK:
        S['ready'] = all(s['status'] == 'ok' for s in S['steps'] if s['required'])
        S['running'] = False; S['finished'] = time.time()


def start(c):
    """One click: fixes everything that is missing, in the background. False when it is already running or a port is running."""
    with LOCK:
        if S['running'] or c.busy(): return False
        S['running'] = True
    threading.Thread(target=_go, args=(c, True), daemon=True).start()
    return True


def check(c):
    """Looks only (no downloads); fills the step list. Safe to call at start-up."""
    with LOCK:
        if S['running']: return False
        S['running'] = True
    threading.Thread(target=_go, args=(c, False), daemon=True).start()
    return True


def status():
    with LOCK:
        return {'running': S['running'], 'ready': S['ready'], 'steps': [dict(s) for s in S['steps']] or _blank(), 'log': S['log'][-60:], 'seq': S['seq']}
