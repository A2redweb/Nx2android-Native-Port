// Cheats for the universal host (added by tools/gen_universal_host.py as host/dm_cheats.c; upstream has none).
// The app puts two files in the game's data folder: cheats.txt (the cheats of this game's main build in Atmosphère's text format: "[Name]" then lines of hex words,
// "{Name}" for a master code that is always on) and cheats_on.txt (the names switched on, one per line). The host runs the switched-on cheats once per present with
// a virtual machine that follows Atmosphère's cheat VM (opcodes 0-A, C0, C1; what is not implemented makes its cheat stop with a line in the log, never a crash).
// The pause menu's Cheats tab (android/overlay_layout.h, front_android.c) toggles them and rewrites cheats_on.txt.
#include "hle.h"
#include <mach/mach.h>

#define MAXC 512
#define MAXW 4096
typedef struct { char name[72]; int master, on, bad; uint32_t *w; int nw; } cheat_t;
static cheat_t g_c[MAXC]; static int g_n, g_page, g_loaded, g_rev;
uint8_t *dm_guest_heap_base(void);

static int crd(uint64_t a, void *out, size_t n) { vm_size_t got = 0; return a >= 0x1000 && vm_read_overwrite(mach_task_self(), (vm_address_t)a, n, (vm_address_t)out, &got) == KERN_SUCCESS && got == n; }
static const char *root(void) { const char *r = getenv("DM_GAME_ROOT"); return r && *r ? r : NULL; }

static void on_save(void) {
    const char *r = root(); if (!r) return;
    char p[600]; snprintf(p, sizeof p, "%s/cheats_on.txt", r); FILE *f = fopen(p, "w"); if (!f) return;
    for (int i = 0; i < g_n; i++) if (g_c[i].on && !g_c[i].master) fprintf(f, "%s\n", g_c[i].name);
    fclose(f);
}

static void load(void) {
    g_loaded = 1; const char *r = root(); if (!r) return;
    char p[600]; snprintf(p, sizeof p, "%s/cheats.txt", r); FILE *f = fopen(p, "r"); if (!f) return;
    char ln[512]; cheat_t *cur = NULL; uint32_t *buf = malloc(MAXW * 4); int nb = 0;
    #define FLUSH() do { if (cur) { cur->w = malloc((size_t)(nb ? nb : 1) * 4); memcpy(cur->w, buf, (size_t)nb * 4); cur->nw = nb; } nb = 0; } while (0)
    while (fgets(ln, sizeof ln, f)) {
        char *s = ln; while (*s == ' ' || *s == '\t' || *s == '\r' || *s == '\n') s++;
        size_t L = strlen(s); while (L && (s[L - 1] == '\n' || s[L - 1] == '\r' || s[L - 1] == ' ')) s[--L] = 0;
        if (!*s) continue;
        if (*s == '[' || *s == '{') {
            FLUSH(); cur = NULL;
            if (g_n >= MAXC) continue;
            cur = &g_c[g_n++]; memset(cur, 0, sizeof *cur); cur->master = *s == '{';
            char *e = strrchr(s, cur->master ? '}' : ']'); if (e) *e = 0;
            snprintf(cur->name, sizeof cur->name, "%s", s + 1); if (cur->master) cur->on = 1;
            continue;
        }
        if (!cur) continue;
        char *q = s;   // hex words; a line that is not hex (a comment, a note) is skipped
        while (*q) {
            while (*q == ' ') q++;
            char *e; unsigned long v = strtoul(q, &e, 16);
            if (e == q || e - q > 8) { break; }
            if (nb < MAXW) buf[nb++] = (uint32_t)v;
            q = e;
        }
    }
    FLUSH(); fclose(f); free(buf);
    snprintf(p, sizeof p, "%s/cheats_on.txt", r);
    if ((f = fopen(p, "r"))) {
        while (fgets(ln, sizeof ln, f)) { size_t L = strlen(ln); while (L && (ln[L - 1] == '\n' || ln[L - 1] == '\r')) ln[--L] = 0;
            for (int i = 0; i < g_n; i++) if (!g_c[i].master && !strcmp(g_c[i].name, ln)) g_c[i].on = 1; }
        fclose(f);
    }
    int on = 0; for (int i = 0; i < g_n; i++) on += g_c[i].on;
    LOG("cheats: %d cheats for this build, %d on", g_n, on);
}

// ---- the virtual machine ----
typedef struct { uint64_t r[16], sv[16]; uint8_t *main, *heap; cheat_t *c; int pc, fuel; const char *err; uint32_t errop; } vm_t;
static uint64_t base_of(vm_t *v, int m) { return m == 0 ? (uint64_t)(uintptr_t)v->main : m == 1 ? (uint64_t)(uintptr_t)v->heap : 0; }
static int wmask(int t, uint64_t *m) { if (t == 1) *m = 0xff; else if (t == 2) *m = 0xffff; else if (t == 4) *m = 0xffffffffull; else if (t == 8) *m = ~0ull; else return 0; return 1; }
static int mread(uint64_t a, int t, uint64_t *out) { *out = 0; return (t == 1 || t == 2 || t == 4 || t == 8) && crd(a, out, (size_t)t); }
static void mwrite(uint64_t a, int t, uint64_t v) {
    uint64_t cur; if (!mread(a, t, &cur)) return; if (cur == v) return;
    uint8_t *p = (uint8_t *)(uintptr_t)a; dm_guest_write_prepare(p, (size_t)t); memcpy(p, &v, (size_t)t);
}
static int cmp(int c, uint64_t a, uint64_t b) { switch (c) { case 1: return a > b; case 2: return a >= b; case 3: return a < b; case 4: return a <= b; case 5: return a == b; case 6: return a != b; } return 0; }
// the index after the end of the block that starts at pc (a conditional's block ends at its End or its Else, nesting counted); -1 if there is none
static int skip_block(const cheat_t *c, int pc, int stop_at_else) {
    int depth = 1;
    while (pc < c->nw) {
        uint32_t w = c->w[pc]; unsigned op = w >> 28, ext = w >> 24 & 0xf, n = 1;
        if (op == 0) n = ((w >> 24 & 0xf) == 8 ? 4 : 3);
        else if (op == 1) n = ((w >> 24 & 0xf) == 8 ? 4 : 3);
        else if (op == 3) n = (ext == 0 ? 2 : 1);
        else if (op == 4) n = 3; else if (op == 5) n = 2; else if (op == 6) n = 3; else if (op == 7) n = 2;
        else if (op == 9) n = ((w >> 8 & 0xf) && (w >> 20 & 0xf) != 7 && (w >> 20 & 0xf) != 9) ? ((w >> 24 & 0xf) == 8 ? 3 : 2) : 1;
        else if (op == 0xa) { unsigned ot = w >> 8 & 0xf; n = ot >= 2 && ot <= 5 ? 2 : 1; }
        else if (op == 0xc) { unsigned s = w >> 24; if (s == 0xc0) { unsigned x = w >> 8 & 0xf; n = x == 4 ? ((w >> 24 & 0xf) == 8 ? 3 : 2) : x == 5 ? 1 : 2; } }
        if (op == 1 || (op == 0xc && (w >> 24) == 0xc0) || op == 8) depth++;
        else if (op == 2) { if (ext == 1 && depth == 1 && stop_at_else) return pc + 1; if (ext == 0 && --depth == 0) return pc + 1; }
        pc += (int)n;
    }
    return -1;
}
static void fail(vm_t *v, uint32_t op, const char *why) { if (!v->err) { v->err = why; v->errop = op; } }
static void run(cheat_t *c, uint8_t *main, uint8_t *heap) {
    vm_t V = {.main = main, .heap = heap, .c = c, .fuel = 20000}, *v = &V; uint64_t m;
    int loops[16], nl = 0, ne = 0;
    while (v->pc < c->nw && v->fuel-- > 0 && !v->err) {
        uint32_t w = c->w[v->pc], w1 = v->pc + 1 < c->nw ? c->w[v->pc + 1] : 0, w2 = v->pc + 2 < c->nw ? c->w[v->pc + 2] : 0, w3 = v->pc + 3 < c->nw ? c->w[v->pc + 3] : 0;
        unsigned op = w >> 28, T = w >> 24 & 0xf, M = w >> 20 & 0xf, R = w >> 16 & 0xf; int pc0 = v->pc;
        uint64_t rel = (uint64_t)(w & 0xff) << 32 | w1;
        if (op == 0) {   // 0TMR00AA AAAAAAAA VVVVVVVV (VVVVVVVV): store a value
            if (!wmask(T, &m) || M > 3) { fail(v, w, "unsupported width or region"); break; }
            uint64_t val = T == 8 ? (uint64_t)w2 << 32 | w3 : w2; v->pc += T == 8 ? 4 : 3;
            mwrite(base_of(v, M) + v->r[R] + rel, (int)T, val & m);
        } else if (op == 1) {   // 1TMCXrAA AAAAAAAA VVVVVVVV (VVVVVVVV): begin a conditional block
            if (!wmask(T, &m) || M > 3) { fail(v, w, "unsupported width or region"); break; }
            unsigned C = w >> 16 & 0xf, useofs = w >> 12 & 0xf, ofr = w >> 8 & 0xf;
            uint64_t val = T == 8 ? (uint64_t)w2 << 32 | w3 : w2, cur; v->pc += T == 8 ? 4 : 3;
            uint64_t a = base_of(v, M) + ((uint64_t)(w & 0xff) << 32 | w1) + (useofs ? v->r[ofr] : 0);
            if (!mread(a, (int)T, &cur) || !cmp((int)C, cur, val & m)) { int e = skip_block(c, v->pc, 1); if (e < 0) break; v->pc = e; if (c->w[e - 1] >> 24 == 0x21) { ne++; } }
            else ne++;
        } else if (op == 2) {   // 20000000 end, 21000000 else
            if (T == 1) { v->pc = skip_block(c, v->pc + 1, 0); if (v->pc < 0) break; if (ne) ne--; }
            else { v->pc++; if (ne) ne--; }
        } else if (op == 3) {   // 300R0000 VVVVVVVV: loop start; 310R0000: loop end
            if (T == 0) { v->r[R] = w1; if (nl < 16) loops[nl++] = v->pc + 2; v->pc += 2; }
            else { v->pc++; if (nl) { if (v->r[R] > 0) { v->r[R]--; v->pc = loops[nl - 1]; } else nl--; } }
        } else if (op == 4) { v->r[R] = (uint64_t)w1 << 32 | w2; v->pc += 3; }   // 400R0000 VVVVVVVV VVVVVVVV
        else if (op == 5) {   // 5TMR00AA AAAAAAAA ; 5T0R10AA..: R = [R + A]; 5T0R2: [region + R + A]
            unsigned mode = w >> 12 & 0xf; v->pc += 2; uint64_t a = mode == 1 ? v->r[R] + rel : mode == 2 ? base_of(v, M) + v->r[R] + rel : base_of(v, M) + rel, val;
            if (!wmask(T, &m) || M > 3 || !mread(a, (int)T, &val)) { v->r[R] = 0; if (!wmask(T, &m)) fail(v, w, "unsupported width"); } else v->r[R] = val;
        } else if (op == 6) {   // 6T0RIor0 VVVVVVVV VVVVVVVV: [R (+ off reg)] = value, R += width when I
            unsigned inc = w >> 12 & 0xf, addo = w >> 8 & 0xf, ofr = w >> 4 & 0xf; uint64_t val = (uint64_t)w1 << 32 | w2; v->pc += 3;
            if (!wmask(T, &m)) { fail(v, w, "unsupported width"); break; }
            mwrite(v->r[R] + (addo ? v->r[ofr] : 0), (int)T, val & m); if (inc) v->r[R] += T;
        } else if (op == 7) {   // 7T0RC000 VVVVVVVV: R = R op value
            unsigned C = w >> 12 & 0xf; v->pc += 2; if (!wmask(T, &m)) { fail(v, w, "unsupported width"); break; }
            uint64_t a = v->r[R], b = w1, o = 0;
            switch (C) { case 0: o = a + b; break; case 1: o = a - b; break; case 2: o = a * b; break; case 3: o = b < 64 ? a << b : 0; break; case 4: o = b < 64 ? (a & m) >> b : 0; break; default: fail(v, w, "unsupported arithmetic"); }
            v->r[R] = o & m;
        } else if (op == 8) {   // 8KKKKKKK: key-pressed block: the host has no key state for it, so the block does not run
            v->pc++; int e = skip_block(c, v->pc, 0); if (e < 0) break; v->pc = e;
        } else if (op == 9) {   // 9TCRS0s0: R = S op s (or an immediate)
            unsigned C = w >> 20 & 0xf, S = w >> 12 & 0xf, imm = w >> 8 & 0xf, s2 = w >> 4 & 0xf; v->pc++; if (!wmask(T, &m)) { fail(v, w, "unsupported width"); break; }
            uint64_t a = v->r[S], b = 0;
            if (C != 7 && C != 9) { if (imm) { if (T == 8) { b = (uint64_t)w1 << 32 | w2; v->pc += 2; } else { b = w1; v->pc += 1; } } else b = v->r[s2]; }
            uint64_t o = 0; switch (C) { case 0: o = a + b; break; case 1: o = a - b; break; case 2: o = a * b; break; case 3: o = b < 64 ? a << b : 0; break; case 4: o = b < 64 ? (a & m) >> b : 0; break;
                case 5: o = a & b; break; case 6: o = a | b; break; case 7: o = ~a; break; case 8: o = a ^ b; break; case 9: o = a; break; default: fail(v, w, "unsupported arithmetic"); }
            v->r[R] = o & m; (void)w2; (void)w3;
        } else if (op == 0xa) {   // ATSRIOxa: [A + offset] = S
            unsigned S = w >> 20 & 0xf, A = w >> 16 & 0xf, inc = w >> 12 & 0xf, ot = w >> 8 & 0xf, ofr = w >> 4 & 0xf; uint64_t addr = v->r[A];
            T = w >> 24 & 0xf; v->pc++;
            if (ot == 1) addr += v->r[ofr]; else if (ot == 2) { addr += (uint64_t)(w & 0xf) << 32 | w1; v->pc++; }
            else if (ot == 3) addr = base_of(v, 0) + v->r[ofr]; else if (ot == 4) addr = base_of(v, 1) + v->r[ofr]; else if (ot != 0) { fail(v, w, "unsupported offset type"); break; }
            if (!wmask(T, &m)) { fail(v, w, "unsupported width"); break; }
            mwrite(addr, (int)T, v->r[S] & m); if (inc) v->r[A] += T;
        } else if (op == 0xc) {
            unsigned x = w >> 24; v->pc++;
            if (x == 0xc0) {   // C0TcSX##: register conditional: against a value (X 4) or a register (X 5)
                unsigned C = w >> 16 & 0xf, S = w >> 12 & 0xf, X = w >> 8 & 0xf;
                uint64_t b = 0; int ok = 1; unsigned W = w >> 20 & 0xf; if (!wmask(W, &m)) ok = 0;
                if (X == 4) { if (W == 8) { b = (uint64_t)w1 << 32 | w2; v->pc += 2; } else { b = w1; v->pc += 1; } } else if (X == 5) b = v->r[w >> 4 & 0xf]; else ok = 0;
                if (!ok || !cmp((int)C, v->r[S] & m, b & m)) { int e = skip_block(c, v->pc, 1); if (e < 0) break; v->pc = e; }
            } else if (x == 0xc1) {   // C10D0Sx0: save / restore registers
                unsigned D = w >> 16 & 0xf, S = w >> 8 & 0xf, k = w >> 4 & 0xf; (void)S;
                if (k == 0) v->r[D] = v->sv[D]; else if (k == 1) v->sv[D] = v->r[D]; else if (k == 2) v->sv[D] = 0; else if (k == 3) v->r[D] = 0;
            } else fail(v, w, "unsupported extended opcode");
        } else fail(v, w, "unsupported opcode");
        if (v->pc == pc0) v->pc++;
    }
    if (v->err && !c->bad) { c->bad = 1; LOG("cheats: \"%s\" stopped: %s (opcode %08x)", c->name, v->err, v->errop); }
}

// ---- host hooks ----
void dm_cheat_tick(void) {
    static int tick; static uint8_t *main_base;
    if (!g_loaded) { load(); for (int i = 0; i < g_nmod; i++) if (!strcmp(g_mod[i].name, "main")) main_base = g_mod[i].base; }
    if (!g_n || !main_base || (++tick & 1)) return;   // every second present: half the frame rate is plenty for a value that is held
    uint8_t *heap = dm_guest_heap_base();
    for (int i = 0; i < g_n; i++) if (g_c[i].on && !g_c[i].bad) run(&g_c[i], main_base, heap);
}
// the pause menu: slots 0..3 of the open page
int dm_cheat_count(void) { if (!g_loaded) load(); int n = 0; for (int i = 0; i < g_n; i++) n += !g_c[i].master; return n; }
static cheat_t *slot(int k) { int n = 0, want = g_page * 4 + k; for (int i = 0; i < g_n; i++) if (!g_c[i].master) { if (n++ == want) return &g_c[i]; } return NULL; }
int dm_cheat_slot_valid(int k) { if (!g_loaded) load(); if (!dm_cheat_count()) return k == 0; return slot(k) != NULL; }
int dm_cheat_pages(void) { return (dm_cheat_count() + 3) / 4; }
void dm_cheat_page_next(void) { int p = dm_cheat_pages(); g_page = p ? (g_page + 1) % p : 0; g_rev++; }
int dm_cheat_rev(void) { return g_rev; }   // the menu's state changes with it, so the panel is drawn again
void dm_cheat_toggle(int k) { cheat_t *c = slot(k); if (!c) return; c->on = !c->on; g_rev++; if (c->on) c->bad = 0; on_save(); LOG("cheats: \"%s\" %s", c->name, c->on ? "on" : "off"); }
unsigned dm_cheat_bits(void) { unsigned b = 0; for (int k = 0; k < 4; k++) { cheat_t *c = slot(k); if (c && c->on) b |= 1u << k; } return b; }
// the label a menu row shows: a cheat's name, the page row's counter, or the row's own label
const char *dm_cheat_label(int k, const char *fallback) {
    static char buf[96];
    if (k >= 0 && k < 4) { cheat_t *c = slot(k); if (c) { snprintf(buf, sizeof buf, "%s", c->name); return buf; } return dm_cheat_count() ? "" : "No cheats: add them in the app"; }
    if (k == 4) { snprintf(buf, sizeof buf, "Cheats %d of %d", g_page + 1, dm_cheat_pages()); return buf; }
    return fallback;
}
