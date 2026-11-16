//! The shader build (upstream's `shader_vk.py` main): jobs for every shader in the index, the translator over them in batches, a second pass for vertex inputs used as integers,
//! then the bundle: each shader's SPIR-V and its binary reflection.

use crate::bind::{classify, refl_bin};
use crate::job::{gs_variant_key, job_for, Options};
use crate::json::Json;
use crate::scan::Index;
use crate::spirv;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

pub struct Params {
    /// the translator command (its arguments before the job file): the executable, or `dotnet dmsh_xlate.dll`
    pub xlate: Vec<String>,
    pub work: PathBuf,
    pub threads: usize,
    /// shaders per translator process
    pub chunk: usize,
    pub subgroup: i64,
    pub dynamic_slots: bool,
    pub facts: BTreeMap<String, Json>,
    pub handles: BTreeMap<String, (u32, u32, usize)>,
}

#[derive(Debug, Default)]
pub struct Report {
    /// (bundle key, NVN stage) of every shader that was meant to be in the bundle
    pub expected: Vec<(u64, u32)>,
    pub failed: Vec<(u64, String)>,
    pub emitted: usize,
    pub notes: Vec<String>,
    /// the translator's stderr lines, counted
    pub translator_errors: BTreeMap<String, usize>,
    pub kinds: BTreeMap<&'static str, usize>,
}

fn stage_index(name: &str) -> Option<u32> {
    ["vs", "ps", "gs", "hs", "ds", "cs"].iter().position(|s| *s == name).map(|i| i as u32)
}

/// `translated: N ok, M failed` on the last line, N + M = the batch, and the exit status 1 exactly when something failed
fn check_batch(stdout: &str, code: Option<i32>, count: usize) -> Result<(), String> {
    let last = stdout.trim_end().lines().last().unwrap_or("");
    if let Some(rest) = last.strip_prefix("translated: ") {
        if let Some((ok, bad)) = rest.strip_suffix(" failed").and_then(|r| r.split_once(" ok, ")) {
            if let (Ok(ok), Ok(bad)) = (ok.parse::<usize>(), bad.parse::<usize>()) {
                if ok + bad == count && code == Some((bad > 0) as i32) {
                    return Ok(());
                }
            }
        }
    }
    Err("shader translator did not complete its batch".into())
}

pub fn validate_resource(res: &Json, key: &str, stage: u32) -> Result<(), String> {
    let names = ["vs", "ps", "gs", "hs", "ds", "cs"];
    if res.get("key").and_then(|k| k.as_str()) != Some(key) || res.get("stage").and_then(|s| s.as_str()) != Some(names[stage as usize]) {
        return Err(format!("{key}: translator reflection identity mismatch"));
    }
    let fields: [(&str, &[&str]); 4] = [("cbufs", &["binding", "slot"]), ("sbufs", &["binding", "slot", "cb_slot", "cb_offset", "flags"]), ("textures", &["binding", "type", "cbuf_slot", "handle", "array", "flags"]), ("images", &["binding", "type", "cbuf_slot", "handle", "array", "flags"])];
    for (name, ints) in fields {
        let rows = res.get(name).and_then(|r| r.as_arr()).ok_or_else(|| format!("{key}: missing reflection {name}"))?;
        let mut seen = std::collections::BTreeSet::new();
        for row in rows {
            if ints.iter().any(|f| row.get(f).and_then(|v| v.as_i64()).is_none()) {
                return Err(format!("{key}: invalid reflection {name} row"));
            }
            let b = row.get("binding").unwrap().as_i64().unwrap();
            if b < 0 || !seen.insert(b) {
                return Err(format!("{key}: invalid reflection binding"));
            }
            if (name == "textures" || name == "images") && (row.get("format").and_then(|f| f.as_str()).is_none() || row.get("separate").and_then(|s| s.as_bool()).is_none()) {
                return Err(format!("{key}: invalid image reflection"));
            }
        }
    }
    if let Some(f) = res.get("fetch1d") {
        if f.as_arr().is_none_or(|a| a.iter().any(|x| x.as_i64().is_none())) {
            return Err(format!("{key}: invalid fetch1d reflection"));
        }
    }
    Ok(())
}

/// the ordered SBuffer rows must agree with the SPIR-V's `<stage>_s<n>` bindings
fn validate_bindings(binds: &[spirv::Binding], res: &Json) -> Result<(), String> {
    let mut sb: Vec<i64> = res.get("sbufs").and_then(|s| s.as_arr()).map(|a| a.iter().filter_map(|r| r.get("binding").and_then(|b| b.as_i64())).collect()).unwrap_or_default();
    sb.sort_unstable();
    for b in binds {
        let n = b.name.bytes().take_while(|c| c.is_ascii_lowercase()).count();
        if n == 0 {
            continue;
        }
        if let Some(d) = b.name[n..].strip_prefix("_s").filter(|d| !d.is_empty() && d.bytes().all(|c| c.is_ascii_digit())) {
            let idx: usize = d.parse().unwrap_or(usize::MAX);
            if idx >= sb.len() || sb[idx] != b.binding as i64 {
                return Err("storage buffer reflection does not match SPIR-V".into());
            }
        }
    }
    Ok(())
}

fn product(w: &Path, key: &str, stage: u32) -> Result<Option<(Vec<u8>, Json)>, String> {
    let spv = w.join(format!("{key}.spv"));
    if !spv.is_file() {
        return Ok(None);
    }
    let data = std::fs::read(&spv).map_err(|e| e.to_string())?;
    spirv::validate(&data, stage)?;
    let r = w.join(format!("{key}.res.json"));
    if !r.is_file() {
        return Err(format!("{key}: translator omitted reflection"));
    }
    let res = Json::parse(&std::fs::read_to_string(&r).map_err(|e| e.to_string())?)?;
    validate_resource(&res, key, stage)?;
    Ok(Some((data, res)))
}

fn run_chunk(p: &Params, jobs: &BTreeMap<String, Json>, keys: &[String], tag: &str) -> (Option<String>, Option<String>) {
    let list = Json::Arr(keys.iter().map(|k| jobs[k].clone()).collect());
    let jp = p.work.join(format!("job_{tag}.json"));
    if let Err(e) = std::fs::write(&jp, list.to_string()) {
        return (None, Some(e.to_string()));
    }
    let out = Command::new(&p.xlate[0]).args(&p.xlate[1..]).arg(&jp).arg(&p.work).output();
    let _ = std::fs::remove_file(&jp);
    match out {
        Err(e) => (None, Some(format!("{}: {e}", p.xlate[0]))),
        Ok(o) => {
            let (so, se) = (String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned());
            match check_batch(&so, o.status.code(), keys.len()) {
                Ok(()) => (Some(se), None),
                Err(e) => (Some(se.clone()), Some(format!("{e}: {}", se.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect::<String>()))),
            }
        }
    }
}

/// Runs the translator over `keys`, `chunk` shaders to a process, on `threads` threads. → (the translator's stderr lines counted, a batch error per key)
fn translate(p: &Params, jobs: &BTreeMap<String, Json>, keys: &[String], progress: &(dyn Fn(&str, usize, usize) + Sync), label: &str, pass: &str) -> (BTreeMap<String, usize>, BTreeMap<String, String>) {
    let chunks: Vec<&[String]> = keys.chunks(p.chunk.max(1)).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let errs: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());
    let berr: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
    std::thread::scope(|sc| {
        for _ in 0..p.threads.max(1).min(chunks.len().max(1)) {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(ch) = chunks.get(i) else { break };
                let (se, e) = run_chunk(p, jobs, ch, &format!("{pass}{i}"));
                if let Some(se) = se {
                    let mut m = errs.lock().unwrap();
                    for l in se.lines().filter(|l| !l.trim().is_empty()) {
                        *m.entry(l.trim().chars().take(120).collect()).or_default() += 1;
                    }
                }
                if let Some(e) = e {
                    let mut m = berr.lock().unwrap();
                    for k in ch.iter() {
                        m.insert(k.clone(), e.clone());
                    }
                }
                let d = done.fetch_add(ch.len(), Ordering::Relaxed) + ch.len();
                progress(label, d, keys.len());
            });
        }
    });
    (errs.into_inner().unwrap(), berr.into_inner().unwrap())
}

/// Builds the bundle at `out` from every shader of `index`.
pub fn build(index: &Index, p: &Params, out: &Path, progress: &(dyn Fn(&str, usize, usize) + Sync)) -> Result<Report, String> {
    std::fs::create_dir_all(&p.work).map_err(|e| e.to_string())?;
    let o = Options { subgroup: p.subgroup };
    let mut rep = Report::default();
    let mut jobs: BTreeMap<String, Json> = BTreeMap::new();
    let mut c1s: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut stages: BTreeMap<String, crate::split::Stage> = BTreeMap::new();
    let mut gs_var: BTreeMap<String, u64> = BTreeMap::new();
    let mut keys: Vec<String> = vec![];
    for (&key, ent) in &index.ents {
        let k = format!("{key:016x}");
        let (ctl, code) = index.load(key)?;
        let j = job_for(&k, ent.stage, &ctl, &code, p.facts.get(&k), &o).map_err(|e| format!("{k}: {e}"))?;
        let cp = p.work.join(format!("{k}.bin"));
        std::fs::write(&cp, &j.code).map_err(|e| e.to_string())?;
        let mut json = j.json;
        json.set("code", Json::Str(cp.to_string_lossy().into_owned()));
        rep.notes.extend(j.notes);
        jobs.insert(k.clone(), json);
        c1s.insert(k.clone(), j.c1);
        stages.insert(k.clone(), ent.stage);
        keys.push(k);
    }
    // a geometry shader's input primitive is the draw's: one translation for each kind, triangles under the shader's own key, the others under gs_variant_key
    let gs_keys: Vec<String> = keys.iter().filter(|k| stages[*k] == crate::split::Stage::Gs).cloned().collect();
    for k in gs_keys {
        for t in [0u64, 1, 2, 4] {
            let vk = format!("{k}_t{t}");
            let mut j = jobs[&k].clone();
            j.set("key", Json::Str(vk.clone()));
            j.set("gs_input", Json::Int(t as i64));
            jobs.insert(vk.clone(), j);
            c1s.insert(vk.clone(), c1s[&k].clone());
            stages.insert(vk.clone(), crate::split::Stage::Gs);
            gs_var.insert(vk.clone(), gs_variant_key(u64::from_str_radix(&k, 16).unwrap(), t));
            keys.push(vk);
        }
    }
    if keys.is_empty() {
        return Err("no shaders selected for export".into());
    }
    let bkey = |k: &str| gs_var.get(k).copied().unwrap_or_else(|| u64::from_str_radix(k, 16).unwrap());
    let mut seen = std::collections::BTreeSet::new();
    for k in &keys {
        rep.expected.push((bkey(k), stages[k].nvn()));
        if !seen.insert(bkey(k)) {
            return Err("shader keys collide with geometry variants".into());
        }
    }
    let (errs, mut batch_errors) = translate(p, &jobs, &keys, progress, "translating", "a");
    rep.translator_errors = errs;
    // second pass: vertex shaders with inputs used only as integers are translated again with those inputs reported as sint / uint
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    let mut again: Vec<String> = vec![];
    for k in &keys {
        if stages[k] != crate::split::Stage::Vs {
            continue;
        }
        match product(&p.work, k, 0) {
            Ok(None) => {}
            Ok(Some((spv, _))) => match spirv::int_inputs(&spv) {
                Ok(ints) => {
                    let attr = match jobs.get(k).and_then(|j| j.get("attr")) {
                        Some(Json::Obj(m)) => m.clone(),
                        _ => Default::default(),
                    };
                    let extra: Vec<(u32, bool)> = ints.into_iter().filter(|(l, _)| !attr.contains_key(&l.to_string())).collect();
                    if !extra.is_empty() {
                        let mut at = Json::Obj(attr);
                        for (l, s) in extra {
                            at.set(&l.to_string(), Json::Str(if s { "sint" } else { "uint" }.into()));
                        }
                        jobs.get_mut(k).unwrap().set("attr", at);
                        for ext in ["spv", "res.json"] {
                            let _ = std::fs::remove_file(p.work.join(format!("{k}.{ext}")));
                        }
                        again.push(k.clone());
                    }
                }
                Err(e) => {
                    failures.insert(k.clone(), e);
                }
            },
            Err(e) => {
                failures.insert(k.clone(), e);
            }
        }
    }
    if !again.is_empty() {
        let (errs, be) = translate(p, &jobs, &again, progress, "translating again", "b");
        for (l, n) in errs {
            *rep.translator_errors.entry(l).or_default() += n;
        }
        batch_errors.extend(be);
    }
    let _ = std::fs::remove_file(out);
    let mut bundle = nx_dmsv::Builder::new(out).map_err(|e| e.to_string())?;
    for (i, k) in keys.iter().enumerate() {
        progress("packing", i, keys.len());
        let st = stages[k];
        let r: Result<(), String> = (|| {
            if let Some(f) = failures.get(k) {
                return Err(f.clone());
            }
            let (mut spv, res) = product(&p.work, k, st.nvn())?.ok_or_else(|| batch_errors.get(k).cloned().unwrap_or_else(|| "no output".into()))?;
            let it = spirv::interface(&spv)?;
            validate_bindings(&it.bindings, &res)?;
            if let Some(&(s, t, n)) = p.handles.get(k) {
                spv = spirv::remap_handles(&spv, s, t, n)?;
            }
            let name = st.name();
            let cl: Vec<_> = it.bindings.iter().map(|b| (b.clone(), classify(name, b, &res, p.dynamic_slots))).collect();
            for (_, c) in &cl {
                *rep.kinds.entry(c.kind).or_default() += 1;
            }
            let local = jobs[k].get("local").and_then(|l| l.as_arr()).map(|a| [0, 1, 2].map(|i| a.get(i).and_then(|x| x.as_i64()).unwrap_or(1) as u32));
            let hdr3 = match st {
                crate::split::Stage::Cs => it.local.or(local).unwrap_or([1, 1, 1]),
                crate::split::Stage::Vs => it.inputs,
                crate::split::Stage::Ps => it.outputs,
                _ => [0, 0, 0],
            };
            bundle.add(bkey(k), st.nvn(), &spv, &refl_bin(&cl, &c1s[k], hdr3)).map_err(|e| e.to_string())
        })();
        if let Err(e) = r {
            rep.failed.push((bkey(k), e));
        } else {
            rep.emitted += 1;
        }
    }
    bundle.finish().map_err(|e| e.to_string())?;
    let _ = stage_index("vs");
    Ok(rep)
}
