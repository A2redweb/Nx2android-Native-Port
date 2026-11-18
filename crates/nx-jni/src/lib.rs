//! The JNI layer: `libnxcore.so`, loaded by `org.nx2a.nativeport.Core`. Only strings and ints cross it, so the layer needs three JNI calls (NewStringUTF, GetStringUTFChars,
//! ReleaseStringUTFChars) and no JNI crate. Their slots in the JNI function table (167, 169, 170) are fixed by the JNI specification.
//! Progress is polled (`nativeStatus`), never called back, so no Java method is ever called from Rust.
//! Not yet run on a device: this environment has no Android toolchain. The symbols compile and can be checked with `nm -D` on the host build.

#![allow(clippy::missing_safety_doc)]

use nx_core::ExtractRequest;
use std::ffi::{c_char, c_void, CStr, CString};

type JNIEnv = *mut *const *const c_void;
type JString = *mut c_void;
type JClass = *mut c_void;

const NEW_STRING_UTF: usize = 167;
const GET_STRING_UTF_CHARS: usize = 169;
const RELEASE_STRING_UTF_CHARS: usize = 170;

unsafe fn slot(env: JNIEnv, i: usize) -> *const c_void {
    *(*env).add(i)
}

unsafe fn get(env: JNIEnv, s: JString) -> String {
    if s.is_null() {
        return String::new();
    }
    let f: extern "system" fn(JNIEnv, JString, *mut u8) -> *const c_char = std::mem::transmute(slot(env, GET_STRING_UTF_CHARS));
    let p = f(env, s, std::ptr::null_mut());
    if p.is_null() {
        return String::new();
    }
    let out = CStr::from_ptr(p).to_string_lossy().into_owned();
    let rel: extern "system" fn(JNIEnv, JString, *const c_char) = std::mem::transmute(slot(env, RELEASE_STRING_UTF_CHARS));
    rel(env, s, p);
    out
}

unsafe fn make(env: JNIEnv, s: &str) -> JString {
    let c = CString::new(s.replace('\0', "")).unwrap_or_default();
    let f: extern "system" fn(JNIEnv, *const c_char) -> JString = std::mem::transmute(slot(env, NEW_STRING_UTF));
    f(env, c.as_ptr())
}

#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeVersion(env: JNIEnv, _c: JClass) -> JString {
    make(env, nx_core::VERSION)
}

#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeGames(env: JNIEnv, _c: JClass, dir: JString, overlay: JString, all: u8) -> JString {
    make(env, &nx_core::games_json(&get(env, dir), &get(env, overlay), all != 0))
}

#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeKeys(env: JNIEnv, _c: JClass, path: JString) -> JString {
    make(env, &nx_core::keys_json(&get(env, path)))
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeExtract(
    env: JNIEnv,
    _c: JClass,
    keys: JString,
    base: JString,
    base_name: JString,
    update: JString,
    update_name: JString,
    out: JString,
    tmp: JString,
    dlc: JString,
) -> i32 {
    let r = ExtractRequest {
        keys_path: get(env, keys),
        base_path: get(env, base),
        base_name: get(env, base_name),
        update_path: get(env, update),
        update_name: get(env, update_name),
        out_dir: get(env, out),
        tmp_dir: get(env, tmp),
        // one add-on dump per line: path, a tab, its file name
        dlc: get(env, dlc).lines().filter_map(|l| l.split_once('\t')).map(|(a, b)| (a.to_string(), b.to_string())).collect(),
    };
    nx_core::start_extract(r) as i32
}

#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeCancel(_env: JNIEnv, _c: JClass, id: i32) {
    nx_core::cancel(id as u32);
}

#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeStatus(env: JNIEnv, _c: JClass, id: i32) -> JString {
    make(env, &nx_core::status_json(id as u32))
}

#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativePrepare(env: JNIEnv, _c: JClass, profiles: JString, id: JString, dir: JString, language: JString) -> JString {
    let r = nx_core::write_switch_files(std::path::Path::new(&get(env, profiles)), &get(env, id), std::path::Path::new(&get(env, dir)), &get(env, language));
    make(env, &r.err().unwrap_or_default())
}

/// Rewrites the game's code modules into mods/ (`cuts.tsv` sits in the profiles folder). Returns "ok: <summary>" or "error: <message>".
#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeRewrite(env: JNIEnv, _c: JClass, profiles: JString, id: JString, dir: JString) -> JString {
    let profiles = get(env, profiles);
    let cuts = std::fs::read_to_string(std::path::Path::new(&profiles).join("cuts.tsv")).unwrap_or_default();
    let steps = std::fs::read_to_string(std::path::Path::new(&profiles).join("framestep.tsv")).unwrap_or_default();
    let r = nx_core::rewrite_modules(std::path::Path::new(&profiles), &cuts, &steps, &get(env, id), std::path::Path::new(&get(env, dir)));
    make(env, &match r {
        Ok(s) => format!("ok: {s}"),
        Err(e) => format!("error: {e}"),
    })
}

/// Builds `shaders.dmsv` of the game's data folder. `xlate` is the translator executable; `threads` the parallel translator processes. Returns "ok: <summary>" or "error: <message>".
#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeBuildShaders(env: JNIEnv, _c: JClass, profiles: JString, id: JString, dir: JString, xlate: JString, threads: i32) -> JString {
    let (profiles, id, dir, xlate) = (get(env, profiles), get(env, id), get(env, dir), get(env, xlate));
    let r = nx_core::build_shaders(std::path::Path::new(&profiles), &id, std::path::Path::new(&dir), vec![xlate], threads.max(1) as usize, &|_, _, _| {});
    make(env, &match r {
        Ok(s) => format!("ok: {s}"),
        Err(e) => format!("error: {e}"),
    })
}

/// The build ID of the game's main module ("ok: <32 hex>") and the warehouse URL of its FPSLocker config ("ok: <id>\t<url>").
#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeFpsPlan(env: JNIEnv, _c: JClass, dir: JString, title_id: JString) -> JString {
    let r = nx_core::fps_build_id(std::path::Path::new(&get(env, dir)));
    make(env, &match r {
        Ok(b) => format!("ok: {b}\t{}", nx_core::fps_url(&get(env, title_id), &b)),
        Err(e) => format!("error: {e}"),
    })
}

/// Converts the config text into the game folder's fpslock.tsv and extra_cuts.tsv. Returns "ok: <summary>" or "error: <what the host cannot apply>".
#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeFpsConvert(env: JNIEnv, _c: JClass, dir: JString, title_id: JString, text: JString, source: JString) -> JString {
    let r = nx_core::fps_convert(std::path::Path::new(&get(env, dir)), &get(env, title_id), &get(env, text), &get(env, source));
    make(env, &match r {
        Ok(s) => format!("ok: {s}"),
        Err(e) => format!("error: {e}"),
    })
}

/// The title ID of a dump read through `path` (a /proc/self/fd/N of the opened file), "" when none is found.
#[no_mangle]
pub unsafe extern "system" fn Java_org_nx2a_nativeport_Core_nativeTitle(env: JNIEnv, _c: JClass, keys: JString, path: JString, name: JString) -> JString {
    let r = nx_core::dump_title(&get(env, keys), &get(env, path), &get(env, name));
    make(env, &r)
}
