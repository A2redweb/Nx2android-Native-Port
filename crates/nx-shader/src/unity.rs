//! Unity shaders (upstream's `unityfs.py` and the Unity half of `shader_split.py`): UnityFS bundles and serialized files, objects decoded by their type trees,
//! the Switch subprograms of each Shader and the kernels of each ComputeShader.

use crate::split::{check, Found, Stage, CTL_MAGIC};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const CLASS_SHADER: i32 = 48;
pub const CLASS_COMPUTE_SHADER: i32 = 72;

/// the serialized file has no type tree and no tree of the same class and hash to borrow, or the file is damaged
#[derive(Debug)]
pub enum UErr {
    NoTree(String),
    Bad(String),
}
impl From<String> for UErr {
    fn from(s: String) -> Self {
        UErr::Bad(s)
    }
}
impl From<&str> for UErr {
    fn from(s: &str) -> Self {
        UErr::Bad(s.to_string())
    }
}
impl std::fmt::Display for UErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UErr::NoTree(s) | UErr::Bad(s) => f.write_str(s),
        }
    }
}

// ---- values and type trees ----

#[derive(Debug, Clone)]
pub enum V {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<V>),
    Obj(Vec<(String, V)>),
}

impl V {
    /// a field of an object (the last of equal names, as a dictionary keeps it)
    pub fn get(&self, k: &str) -> Option<&V> {
        match self {
            V::Obj(f) => f.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        match self {
            V::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn list(&self) -> &[V] {
        match self {
            V::List(l) => l,
            _ => &[],
        }
    }
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            V::Bytes(b) => Some(b),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            V::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Node {
    pub ty: String,
    pub name: String,
    pub flags: i32,
    pub kids: Vec<Node>,
}

#[derive(Clone, Copy)]
enum P {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    Bool,
}

fn prim(t: &str) -> Option<P> {
    Some(match t {
        "SInt8" => P::I8,
        "UInt8" | "char" => P::U8,
        "bool" => P::Bool,
        "SInt16" | "short" => P::I16,
        "UInt16" | "unsigned short" => P::U16,
        "SInt32" | "int" | "Type*" => P::I32,
        "UInt32" | "unsigned int" => P::U32,
        "SInt64" | "long long" => P::I64,
        "UInt64" | "unsigned long long" | "FileSize" => P::U64,
        "float" => P::F32,
        "double" => P::F64,
        _ => return None,
    })
}

struct R<'a> {
    d: &'a [u8],
    p: usize,
    le: bool,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let e = self.p.checked_add(n).filter(|&e| e <= self.d.len()).ok_or_else(|| format!("read of {n} bytes at {:#x} runs past the {:#x} there are", self.p, self.d.len()))?;
        let s = &self.d[self.p..e];
        self.p = e;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let mut a: [u8; N] = self.take(N)?.try_into().unwrap();
        if !self.le {
            a.reverse();
        }
        Ok(a)
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.arr::<4>()?))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.arr::<4>()?))
    }
    fn align(&mut self) {
        self.p = (self.p + 3) & !3;
    }
    fn prim(&mut self, k: P) -> Result<V, String> {
        Ok(match k {
            P::I8 => V::Int(self.arr::<1>()?[0] as i8 as i64),
            P::U8 => V::Int(self.arr::<1>()?[0] as i64),
            P::Bool => V::Bool(self.arr::<1>()?[0] != 0),
            P::I16 => V::Int(i16::from_le_bytes(self.arr::<2>()?) as i64),
            P::U16 => V::Int(u16::from_le_bytes(self.arr::<2>()?) as i64),
            P::I32 => V::Int(i32::from_le_bytes(self.arr::<4>()?) as i64),
            P::U32 => V::Int(u32::from_le_bytes(self.arr::<4>()?) as i64),
            P::I64 => V::Int(i64::from_le_bytes(self.arr::<8>()?)),
            P::U64 => V::Int(u64::from_le_bytes(self.arr::<8>()?) as i64),
            P::F32 => V::Float(f32::from_le_bytes(self.arr::<4>()?) as f64),
            P::F64 => V::Float(f64::from_le_bytes(self.arr::<8>()?)),
        })
    }
}

fn read(r: &mut R, n: &Node) -> Result<V, String> {
    let v = if let (Some(k), true) = (prim(&n.ty), n.kids.is_empty()) {
        r.prim(k)?
    } else if n.ty == "string" {
        let ln = r.i32()?;
        let b = r.take(usize::try_from(ln).map_err(|_| "negative string length")?)?;
        if n.kids.first().is_some_and(|k| k.flags & 0x4000 != 0) {
            r.align();
        }
        V::Str(String::from_utf8_lossy(b).into_owned())
    } else if n.kids.first().is_some_and(|k| k.ty == "Array") {
        let arr = &n.kids[0];
        let cnt = usize::try_from(r.i32()?).ok().filter(|&c| c <= r.d.len()).ok_or("implausible array length")?;
        let el = arr.kids.get(1).ok_or("array without an element type")?;
        let v = match prim(&el.ty).filter(|_| el.kids.is_empty()) {
            Some(P::U8 | P::I8) => V::Bytes(r.take(cnt)?.to_vec()),
            Some(k) => V::List((0..cnt).map(|_| r.prim(k)).collect::<Result<_, _>>()?),
            None => V::List((0..cnt).map(|_| read(r, el)).collect::<Result<_, _>>()?),
        };
        if arr.flags & 0x4000 != 0 {
            r.align();
        }
        v
    } else if n.ty == "TypelessData" {
        let cnt = usize::try_from(r.i32()?).map_err(|_| "negative length")?;
        V::Bytes(r.take(cnt)?.to_vec())
    } else {
        V::Obj(n.kids.iter().map(|k| Ok((k.name.clone(), read(r, k)?))).collect::<Result<_, String>>()?)
    };
    if n.flags & 0x4000 != 0 {
        r.align();
    }
    Ok(v)
}

// ---- layouts the players that ship no type trees need ----

fn split_top(s: &str, sep: char) -> Vec<String> {
    let (mut out, mut depth, mut cur) = (vec![], 0i32, String::new());
    for ch in s.chars() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            _ => {}
        }
        if ch == sep && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    out.push(cur);
    out
}

pub fn tree_from_layout(root: &str, layout: &str) -> Result<Node, String> {
    let mut structs: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for line in layout.trim().lines() {
        let (name, body) = line.split_once(": ").ok_or("layout line without a name")?;
        let fields = body.split("; ").map(|f| f.rsplit_once(' ').ok_or("layout field without a name")).collect::<Result<Vec<_>, _>>()?;
        structs.insert(name, fields);
    }
    fn array(arr_align: i32, el: Node) -> Node {
        Node { ty: "Array".into(), name: "Array".into(), flags: arr_align, kids: vec![Node { ty: "int".into(), name: "size".into(), ..Default::default() }, el] }
    }
    fn node(structs: &HashMap<&str, Vec<(&str, &str)>>, t: &str, name: &str) -> Result<Node, String> {
        let (mut t, mut flags, mut arr_align) = (t, 0, 0x4000);
        if let Some(x) = t.strip_suffix('!') {
            t = x;
            flags = 0x4000;
        }
        if let Some(x) = t.strip_suffix('~') {
            t = x;
            arr_align = 0;
        }
        let mk = |ty: &str, kids| Node { ty: ty.into(), name: name.into(), flags, kids };
        Ok(if t == "string" {
            mk("string", vec![array(arr_align, Node { ty: "char".into(), name: "data".into(), ..Default::default() })])
        } else if t.starts_with("vector<") || t.starts_with("map<") {
            let (kind, inner) = t[..t.len() - 1].split_once('<').unwrap();
            let (el_name, el_type) = inner.split_once(':').ok_or("container without an element")?;
            mk(kind, vec![array(arr_align, node(structs, el_type, el_name)?)])
        } else if let Some(p) = t.strip_prefix("pair(") {
            let parts = split_top(&p[..p.len() - 1], ',');
            if parts.len() != 2 {
                return Err("pair without two types".into());
            }
            mk("pair", vec![node(structs, &parts[0], "first")?, node(structs, &parts[1], "second")?])
        } else if let Some(fields) = structs.get(t) {
            mk(t, fields.iter().map(|(ft, fnm)| node(structs, ft, fnm)).collect::<Result<_, _>>()?)
        } else if prim(t).is_some() {
            mk(t, vec![])
        } else {
            return Err(format!("type tree layout: unknown type {t}"));
        })
    }
    node(&structs, root, "Base")
}

const LAYOUTS: [(&str, &str); 9] = [
    ("shader_2022_3.txt", include_str!("../data/unity/shader_2022_3.txt")),
    ("shader_6000_0.txt", include_str!("../data/unity/shader_6000_0.txt")),
    ("shader_2020_3.txt", include_str!("../data/unity/shader_2020_3.txt")),
    ("shader_2018_3.txt", include_str!("../data/unity/shader_2018_3.txt")),
    ("shader_2019_4.txt", include_str!("../data/unity/shader_2019_4.txt")),
    ("compute_2019_4.txt", include_str!("../data/unity/compute_2019_4.txt")),
    ("compute_2020_3.txt", include_str!("../data/unity/compute_2020_3.txt")),
    ("compute_2022_3.txt", include_str!("../data/unity/compute_2022_3.txt")),
    ("compute_6000_3.txt", include_str!("../data/unity/compute_6000_3.txt")),
];
const KNOWN: &str = include_str!("../data/unity/known.tsv");

pub type TreeKey = (i32, [u8; 16]);
pub type Trees = HashMap<TreeKey, Arc<Node>>;

/// the layouts upstream keeps, as the type trees a file with trees would give
pub fn known_trees() -> Result<Trees, String> {
    let mut t = Trees::new();
    for row in KNOWN.lines().filter(|l| !l.is_empty()) {
        let f: Vec<&str> = row.split('\t').collect();
        let cid: i32 = f[0].parse().map_err(|_| "known.tsv: class")?;
        let mut h = [0u8; 16];
        for i in 0..16 {
            h[i] = u8::from_str_radix(&f[1][2 * i..2 * i + 2], 16).map_err(|_| "known.tsv: hash")?;
        }
        let text = LAYOUTS.iter().find(|(n, _)| *n == f[3]).ok_or("known.tsv: layout")?.1;
        t.insert((cid, h), Arc::new(tree_from_layout(f[2], text)?));
    }
    Ok(t)
}

// ---- bundles and serialized files ----

fn cstr(d: &[u8], p: usize) -> Result<(String, usize), String> {
    let e = d.get(p..).and_then(|s| s.iter().position(|&b| b == 0)).ok_or("unterminated string")?;
    Ok((String::from_utf8_lossy(&d[p..p + e]).into_owned(), p + e + 1))
}

fn be32(d: &[u8], o: usize) -> Result<u32, String> {
    d.get(o..o + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap())).ok_or_else(|| "bundle header is cut short".to_string())
}
fn be64(d: &[u8], o: usize) -> Result<u64, String> {
    d.get(o..o + 8).map(|b| u64::from_be_bytes(b.try_into().unwrap())).ok_or_else(|| "bundle header is cut short".to_string())
}

fn lz4(raw: &[u8], size: usize) -> Result<Vec<u8>, String> {
    nx_nso::lz4_block(raw, size).map_err(|e| format!("lz4: {e}"))
}

fn decomp(raw: &[u8], size: usize, kind: u32) -> Result<Vec<u8>, String> {
    match kind {
        0 => Ok(raw.to_vec()),
        1 => crate::lzma::unity(raw, size),
        2 | 3 => lz4(raw, size),
        k => Err(format!("UnityFS: unknown compression {k}")),
    }
}

/// UnityFS bundle → [(path, bytes)]
pub fn bundle_files(d: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    if d.get(..8) != Some(b"UnityFS\0") {
        return Err("not UnityFS".into());
    }
    let fmt = be32(d, 8)?;
    let (_, p) = cstr(d, 12)?;
    let (_, mut p) = cstr(d, p)?;
    let (csz, usz, flags) = (be32(d, p + 8)? as usize, be32(d, p + 12)? as usize, be32(d, p + 16)?);
    p += 20;
    if fmt >= 7 {
        p = (p + 15) & !15;
    }
    let info_raw = if flags & 0x80 != 0 { d.get(d.len().checked_sub(csz).ok_or("block info outside the file")?..) } else { d.get(p..p + csz) }.ok_or("block info outside the file")?;
    if flags & 0x80 == 0 {
        p += csz;
    }
    let info = decomp(info_raw, usz, flags & 0x3f)?;
    let mut q = 16;
    let nb = be32(&info, q)? as usize;
    q += 4;
    let mut blocks = vec![];
    for _ in 0..nb {
        let (u, c, f) = (be32(&info, q)? as usize, be32(&info, q + 4)? as usize, info.get(q + 8..q + 10).map(|b| u16::from_be_bytes([b[0], b[1]])).ok_or("block table is cut short")?);
        q += 10;
        blocks.push((u, c, f));
    }
    let nn = be32(&info, q)? as usize;
    q += 4;
    let mut nodes = vec![];
    for _ in 0..nn {
        let (off, sz) = (be64(&info, q)? as usize, be64(&info, q + 8)? as usize);
        q += 20;
        let (name, nq) = cstr(&info, q)?;
        q = nq;
        nodes.push((off, sz, name));
    }
    if flags & 0x200 != 0 {
        p = (p + 15) & !15;
    }
    let mut out = Vec::new();
    for (u, c, f) in blocks {
        out.extend(decomp(d.get(p..p + c).ok_or("a data block lies outside the file")?, u, (f & 0x3f) as u32)?);
        p += c;
    }
    nodes.into_iter().map(|(off, sz, name)| Ok((name, out.get(off..off.checked_add(sz).ok_or("node size")?).ok_or("a file node lies outside the data")?.to_vec()))).collect()
}

/// Unity's common string table in the game's main → {offset: string}; None when the image has none (not a Unity game)
pub fn common_strings(image: &[u8]) -> Option<HashMap<u32, String>> {
    let pat = b"AABB\0AnimationClip\0AnimationCurve\0";
    let a = image.windows(pat.len()).position(|w| w == pat)?;
    let (mut tab, mut p) = (HashMap::new(), a);
    loop {
        let e = p + image.get(p..)?.iter().position(|&b| b == 0)?;
        if e == p {
            break;
        }
        let s = &image[p..e];
        if !s.iter().all(|&c| (0x20..0x7f).contains(&c)) {
            break;
        }
        tab.insert((p - a) as u32, String::from_utf8_lossy(s).into_owned());
        p = e + 1;
    }
    Some(tab)
}

/// does it look like a serialized file (format 17..22; from 22 the sizes follow +0x14)
pub fn is_serialized(d: &[u8]) -> bool {
    if d.len() < 48 {
        return false;
    }
    let ver = u32::from_be_bytes(d[8..12].try_into().unwrap());
    if !(17..=22).contains(&ver) || d[16] > 1 {
        return false;
    }
    let (msz, fsz, doff) = if ver >= 22 {
        (u32::from_be_bytes(d[20..24].try_into().unwrap()) as u64, u64::from_be_bytes(d[24..32].try_into().unwrap()), u64::from_be_bytes(d[32..40].try_into().unwrap()))
    } else {
        (u32::from_be_bytes(d[0..4].try_into().unwrap()) as u64, u32::from_be_bytes(d[4..8].try_into().unwrap()) as u64, u32::from_be_bytes(d[12..16].try_into().unwrap()) as u64)
    };
    fsz == d.len() as u64 && 0 < msz && msz < fsz && doff <= fsz
}

/// The objects of a serialized file of the wanted classes, each handed to `f` as (class ID, path ID, value).
/// `trees`: files with type trees record theirs (the wanted classes'), files without borrow the one of the same class and hash
pub fn serialized_objects(d: &[u8], common: &HashMap<u32, String>, want: &[i32], trees: &mut Trees, f: &mut dyn FnMut(i32, i64, V) -> Result<(), UErr>) -> Result<(), UErr> {
    let (_msz, _fsz, ver, mut doff) = (be32(d, 0)?, be32(d, 4)?, be32(d, 8)?, be32(d, 12)? as u64);
    if !(17..=22).contains(&ver) {
        return Err(format!("serialized file format {ver} is not supported").into());
    }
    let mut p = 16;
    let le = *d.get(p).ok_or("short file")? == 0;
    p += 4;
    if ver >= 22 {
        doff = be64(d, p + 12)?;
        p += 28;
    }
    let mut r = R { d, p, le };
    let (_, np) = cstr(d, r.p)?;
    r.p = np;
    r.i32()?;
    let tt = r.take(1)?[0] != 0;
    let mut tlist: Vec<(i32, [u8; 16], Option<Arc<Node>>)> = vec![];
    for _ in 0..r.i32()? {
        let cid = r.i32()?;
        r.take(1)?;
        r.take(2)?;
        if cid == 114 {
            r.take(16)?;
        }
        let h: [u8; 16] = r.take(16)?.try_into().unwrap();
        let mut root: Option<Arc<Node>> = None;
        if tt {
            let (nn, sb) = (r.i32()?, r.i32()?);
            let (nn, sb) = (usize::try_from(nn).map_err(|_| "type tree size")?, usize::try_from(sb).map_err(|_| "type tree size")?);
            let mut raw = vec![];
            for _ in 0..nn {
                let b = r.take(if ver >= 19 { 32 } else { 24 })?;
                let at = |o: usize| u32::from_le_bytes(if le { b[o..o + 4].try_into().unwrap() } else { [b[o + 3], b[o + 2], b[o + 1], b[o]] });
                raw.push((b[2] as usize, at(4), at(8), at(20) as i32)); // level, type offset, name offset, meta flags
            }
            let buf = r.take(sb)?;
            let st = |o: u32| -> String {
                if o & 0x8000_0000 != 0 {
                    common.get(&(o & 0x7fff_ffff)).cloned().unwrap_or_else(|| format!("?{}", o & 0x7fff_ffff))
                } else {
                    let s = buf.get(o as usize..).unwrap_or(&[]);
                    String::from_utf8_lossy(&s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())]).into_owned()
                }
            };
            // the tree is rebuilt from (level, node) rows: a node's parent is the last node of the level above
            let mut flat: Vec<(usize, Node)> = vec![];
            for (lv, to, no, mf) in raw {
                flat.push((lv, Node { ty: st(to), name: st(no), flags: mf, kids: vec![] }));
            }
            let mut stack: Vec<(usize, Node)> = vec![];
            let mut top: Option<Node> = None;
            let fold = |stack: &mut Vec<(usize, Node)>, to_level: usize, top: &mut Option<Node>| {
                while stack.len() > to_level {
                    let (_, nd) = stack.pop().unwrap();
                    match stack.last_mut() {
                        Some((_, parent)) => parent.kids.push(nd),
                        None => *top = Some(nd),
                    }
                }
            };
            for (lv, nd) in flat {
                fold(&mut stack, lv, &mut top);
                stack.push((lv, nd));
            }
            fold(&mut stack, 0, &mut top);
            root = top.map(Arc::new);
            if ver >= 21 {
                let n = r.i32()?;
                r.take(4 * usize::try_from(n).map_err(|_| "type dependencies")?)?;
            }
            if let (Some(rt), true) = (&root, want.is_empty() || want.contains(&cid)) {
                trees.entry((cid, h)).or_insert_with(|| rt.clone());
            }
        } else if let Some(t) = trees.get(&(cid, h)) {
            root = Some(t.clone());
        }
        tlist.push((cid, h, root));
    }
    let mut objs = vec![];
    for _ in 0..r.i32()? {
        r.align();
        let pid = i64::from_le_bytes(r.arr::<8>()?);
        let start = if ver >= 22 { u64::from_le_bytes(r.arr::<8>()?) } else { r.u32()? as u64 };
        let size = r.u32()? as u64;
        let ti = r.i32()?;
        objs.push((pid, start, size, ti));
    }
    for (pid, start, size, ti) in objs {
        let (cid, h, root) = tlist.get(usize::try_from(ti).map_err(|_| "type index")?).ok_or("object of an unknown type")?;
        if !want.is_empty() && !want.contains(cid) {
            continue;
        }
        let root = root.as_ref().ok_or_else(|| UErr::NoTree(format!("class {cid} (type hash {}) has no type tree available", h.iter().map(|b| format!("{b:02x}")).collect::<String>())))?;
        let a = (doff + start) as usize;
        let data = d.get(a..a + size as usize).ok_or("an object lies outside the file")?;
        let mut o = R { d: data, p: 0, le };
        f(*cid, pid, read(&mut o, root).map_err(|e| format!("object {pid}: {e}"))?)?;
    }
    Ok(())
}

/// Shader object → (platform, subprogram index, subprogram bytes) for the indices in `only` (None: all)
pub fn shader_programs(sh: &V, only: Option<&HashSet<i64>>) -> Result<Vec<(i64, usize, Vec<u8>)>, String> {
    let blob = sh.get("compressedBlob").and_then(V::bytes).ok_or("Shader without its blob")?;
    let mut out = vec![];
    let slice = |o: i64, n: i64| -> Result<&[u8], String> { blob.get(usize::try_from(o).map_err(|_| "negative offset")?..usize::try_from(o + n).map_err(|_| "negative length")?).ok_or_else(|| "a shader segment lies outside the blob".to_string()) };
    let (offs, clens, dlens) = (sh.get("offsets").map(V::list).unwrap_or(&[]), sh.get("compressedLengths").map(V::list).unwrap_or(&[]), sh.get("decompressedLengths").map(V::list).unwrap_or(&[]));
    for (i, plat) in sh.get("platforms").map(V::list).unwrap_or(&[]).iter().enumerate() {
        let plat = plat.int().ok_or("platform")?;
        let (o, c, u) = (offs.get(i).ok_or("offsets")?, clens.get(i).ok_or("compressedLengths")?, dlens.get(i).ok_or("decompressedLengths")?);
        let want = |k: usize| only.is_none_or(|s| s.contains(&(k as i64)));
        let rd = |s: &[u8], o: usize| -> Result<u32, String> { s.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| "a shader segment is cut short".to_string()) };
        if let V::Int(off) = o {
            // before 2019.3: one segment per platform, 8-byte entries {offset, length}
            let s0 = lz4(slice(*off, c.int().ok_or("compressedLengths")?)?, u.int().ok_or("decompressedLengths")? as usize)?;
            for k in 0..rd(&s0, 0)? as usize {
                let (a, l) = (rd(&s0, 4 + 8 * k)? as usize, rd(&s0, 8 + 8 * k)? as usize);
                if want(k) {
                    out.push((plat, k, s0.get(a..a + l).ok_or("a subprogram lies outside its segment")?.to_vec()));
                }
            }
            continue;
        }
        let (o, c, u) = (o.list(), c.list(), u.list());
        let segs = o.iter().zip(c).zip(u).map(|((o, c), u)| lz4(slice(o.int().ok_or("offset")?, c.int().ok_or("length")?)?, u.int().ok_or("length")? as usize)).collect::<Result<Vec<_>, _>>()?;
        let Some(s0) = segs.first() else { continue };
        for k in 0..rd(s0, 0)? as usize {
            let (a, l, sg) = (rd(s0, 4 + 12 * k)? as usize, rd(s0, 8 + 12 * k)? as usize, rd(s0, 12 + 12 * k)? as usize);
            if want(k) {
                out.push((plat, k, segs.get(sg).ok_or("a subprogram names a segment that is not there")?.get(a..a + l).ok_or("a subprogram lies outside its segment")?.to_vec()));
            }
        }
    }
    Ok(out)
}

/// the program code bytes of a subprogram
pub fn subprogram_code(b: &[u8]) -> Result<&[u8], String> {
    let mut r = R { d: b, p: 0, le: true };
    let ver = r.i32()?;
    r.i32()?;
    r.take(12)?;
    if ver >= 201608170 {
        r.take(4)?;
    }
    let strs = |r: &mut R| -> Result<(), String> {
        for _ in 0..r.i32()? {
            let ln = r.i32()?;
            r.take(usize::try_from(ln).map_err(|_| "negative keyword length")?)?;
            r.align();
        }
        Ok(())
    };
    strs(&mut r)?;
    if (201806140..202012090).contains(&ver) {
        strs(&mut r)?;
    }
    let ln = r.i32()?;
    r.take(usize::try_from(ln).map_err(|_| "negative code length")?)
}

// ---- programs ----

const NV_STAGES: [Stage; 6] = [Stage::Vs, Stage::Ps, Stage::Gs, Stage::Hs, Stage::Ds, Stage::Cs];

/// Unity's Switch program code → [(stage, control, code)]; `stage` is the subprogram's stage from m_ParsedForm, which only the pre-2019.3 layout needs
pub fn unity_program(code: &[u8], at: &str, stage: Option<Stage>) -> Result<Vec<(Stage, Vec<u8>, Vec<u8>)>, String> {
    let le = |o: usize| code.get(o..o + 4).map(|b| i32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("{at}: program code is cut short"));
    if code.len() >= 0x14 && code[0x10..0x14] == CTL_MAGIC {
        let st = stage.ok_or_else(|| format!("{at}: pre-2019.3 subprogram whose stage is not in m_ParsedForm"))?;
        let (nctl, ncode) = (le(8)? as usize, le(12)? as usize);
        let end = 0x10usize.checked_add(nctl).and_then(|x| x.checked_add(ncode)).filter(|&e| e <= code.len()).ok_or_else(|| format!("{at} {}: block out of bounds", st.name()))?;
        let (ctl, cd) = (&code[0x10..0x10 + nctl], &code[0x10 + nctl..end]);
        if let Some(w) = check(st, ctl, cd) {
            return Err(format!("{at} {}: {w}", st.name()));
        }
        if end != code.len() {
            return Err(format!("{at}: program code is {:#x} bytes, but control section + code area end at {end:#x}", code.len()));
        }
        return Ok(vec![(st, ctl.to_vec(), cd.to_vec())]);
    }
    let mut out = vec![];
    let mut end = 0x60usize;
    for (i, st) in NV_STAGES.iter().enumerate() {
        let (off, nctl, ncode) = (le(4 * (6 + i))?, le(4 * (12 + i))?, le(4 * (18 + i))?);
        if off == -1 {
            continue;
        }
        let bad = || format!("{at} {}: block out of bounds", st.name());
        let a = 0x60usize.checked_add(usize::try_from(off).map_err(|_| bad())?).ok_or_else(bad)?;
        let (nctl, ncode) = (usize::try_from(nctl).map_err(|_| bad())?, usize::try_from(ncode).map_err(|_| bad())?);
        let e = a.checked_add(nctl).and_then(|x| x.checked_add(ncode)).filter(|&e| e <= code.len()).ok_or_else(bad)?;
        let (ctl, cd) = (&code[a..a + nctl], &code[a + nctl..e]);
        if let Some(w) = check(*st, ctl, cd) {
            return Err(format!("{at} {}: {w}", st.name()));
        }
        out.push((*st, ctl.to_vec(), cd.to_vec()));
        end = end.max(e);
    }
    if end != code.len() {
        return Err(format!("{at}: program code is {:#x} bytes, but the stage blocks end at {end:#x}", code.len()));
    }
    Ok(out)
}

const PROG_FIELDS: [(&str, Stage); 5] = [("progVertex", Stage::Vs), ("progFragment", Stage::Ps), ("progGeometry", Stage::Gs), ("progHull", Stage::Hs), ("progDomain", Stage::Ds)];

fn passes(o: &V) -> Vec<&V> {
    o.get("m_ParsedForm").map(|p| p.get("m_SubShaders").map(V::list).unwrap_or(&[]).iter().flat_map(|s| s.get("m_Passes").map(V::list).unwrap_or(&[]).iter()).collect()).unwrap_or_default()
}

/// Shader object → {subprogram index: stage}, from the passes of m_ParsedForm
pub fn unity_stages(o: &V) -> HashMap<i64, Stage> {
    let mut m = HashMap::new();
    for p in passes(o) {
        for (field, st) in PROG_FIELDS {
            for sp in p.get(field).and_then(|f| f.get("m_SubPrograms")).map(V::list).unwrap_or(&[]) {
                if let Some(i) = sp.get("m_BlobIndex").and_then(V::int) {
                    m.entry(i).or_insert(st);
                }
            }
        }
    }
    m
}

/// the subprogram indices that hold program code, or None when every entry does (before Unity 2022: later layouts keep each program's parameters in the blob as entries of their own)
pub fn unity_code_blobs(o: &V) -> Option<HashSet<i64>> {
    let progs: Vec<&V> = passes(o).into_iter().flat_map(|p| PROG_FIELDS.iter().filter_map(move |(f, _)| p.get(f))).collect();
    if !progs.iter().any(|p| p.get("m_ParameterBlobIndices").is_some()) {
        return None;
    }
    Some(progs.iter().flat_map(|p| p.get("m_PlayerSubPrograms").map(V::list).unwrap_or(&[]).iter().flat_map(|t| t.list().iter().filter_map(|sp| sp.get("m_BlobIndex").and_then(V::int)))).collect())
}

/// ComputeShader objects of a serialized file → shaders: each kernel variant's code is a program code with one stage, compute.
/// In the first pass a missing tree is an error (the file is scanned again with borrowed trees); in the second (`borrowed`) the kernels are left out.
fn unity_compute(x: &[u8], base: &str, common: &HashMap<u32, String>, trees: &mut Trees, borrowed: bool, out: &mut Vec<Found>) -> Result<(), UErr> {
    let mut f = |_cid: i32, _pid: i64, o: V| -> Result<(), UErr> {
        for v in o.get("variants").map(V::list).unwrap_or(&[]) {
            for k in v.get("kernels").map(V::list).unwrap_or(&[]) {
                let units: Vec<&V> = if let Some(u) = k.get("uniqueVariants") {
                    u.list().iter().collect()
                } else if let Some(m) = k.get("variantMap") {
                    m.list().iter().filter_map(|p| p.get("second")).collect()
                } else {
                    vec![k]
                };
                for (i, u) in units.into_iter().enumerate() {
                    let at = format!("{base}#{}#{}.{i}", o.get("m_Name").and_then(V::str).unwrap_or("?"), k.get("name").and_then(V::str).unwrap_or("?"));
                    for (st, ctl, code) in unity_program(u.get("code").and_then(V::bytes).ok_or("kernel without code")?, &at, Some(Stage::Cs))? {
                        out.push(Found { label: at.clone(), stage: st, ctl, code });
                    }
                }
            }
        }
        Ok(())
    };
    match serialized_objects(x, common, &[CLASS_COMPUTE_SHADER], trees, &mut f) {
        Err(UErr::NoTree(_)) if borrowed => Ok(()),
        r => r,
    }
}

/// Shaders and compute shaders in a UnityFS bundle or serialized file → `out`. NoTree when a type tree is missing (first pass).
pub fn unity_shaders(d: &[u8], label: &str, common: &HashMap<u32, String>, trees: &mut Trees, borrowed: bool, out: &mut Vec<Found>) -> Result<(), UErr> {
    // a plain serialized file is read in place; only a bundle's nodes are new buffers
    let files: Vec<(String, std::borrow::Cow<[u8]>)> = if d.get(..8) == Some(b"UnityFS\0") {
        bundle_files(d)?.into_iter().map(|(n, v)| (n, std::borrow::Cow::Owned(v))).collect()
    } else {
        vec![(String::new(), std::borrow::Cow::Borrowed(d))]
    };
    for (node, x) in files {
        let x: &[u8] = &x;
        if !is_serialized(x) {
            continue;
        }
        let base = format!("{label}{}", if node.is_empty() { String::new() } else { format!(":{node}") });
        let mut f = |_cid: i32, _pid: i64, o: V| -> Result<(), UErr> {
            let name = o.get("m_ParsedForm").and_then(|p| p.get("m_Name")).or_else(|| o.get("m_Name")).and_then(V::str).unwrap_or("?").to_string();
            let stages = unity_stages(&o);
            for (plat, k, sp) in shader_programs(&o, unity_code_blobs(&o).as_ref())? {
                let at = format!("{base}#{name}#var{plat}.{k}");
                let code = subprogram_code(&sp)?;
                for (st, ctl, cd) in unity_program(code, &at, stages.get(&(k as i64)).copied())? {
                    out.push(Found { label: at.clone(), stage: st, ctl, code: cd });
                }
            }
            Ok(())
        };
        serialized_objects(x, common, &[CLASS_SHADER], trees, &mut f)?;
        unity_compute(x, &base, common, trees, borrowed, out)?;
    }
    Ok(())
}


/// a synthetic serialized file for other modules' tests
#[cfg(test)]
pub fn tests_serialized(with_tree: bool) -> Vec<u8> {
    tests::serialized(with_tree).0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a control section and a code area that pass `check` for the SPH stage bits `sph`
    fn fake(sph: u32) -> (Vec<u8>, Vec<u8>) {
        let mut ctl = vec![0u8; 0x880];
        ctl[..4].copy_from_slice(&CTL_MAGIC);
        ctl[0x20..0x24].copy_from_slice(&0x880u32.to_le_bytes());
        let mut code = vec![0u8; 0x100];
        code[..4].copy_from_slice(&crate::split::GFX_MAGIC);
        code[0x30..0x34].copy_from_slice(&(sph << 10).to_le_bytes());
        ctl[0x704..0x708].copy_from_slice(&0x100u32.to_le_bytes());
        (ctl, code)
    }

    /// an LZ4 block of literals only
    fn lz4_lit(d: &[u8]) -> Vec<u8> {
        let mut o = vec![(d.len().min(15) as u8) << 4];
        if d.len() >= 15 {
            let mut r = d.len() - 15;
            while r >= 255 {
                o.push(255);
                r -= 255;
            }
            o.push(r as u8);
        }
        o.extend(d);
        o
    }

    fn pad4(v: &mut Vec<u8>) {
        while v.len() % 4 != 0 {
            v.push(0);
        }
    }
    fn s(v: &mut Vec<u8>, t: &str) {
        v.extend((t.len() as i32).to_le_bytes());
        v.extend(t.as_bytes());
        pad4(v);
    }
    fn arr_u32(v: &mut Vec<u8>, a: &[u32]) {
        v.extend((a.len() as i32).to_le_bytes());
        for x in a {
            v.extend(x.to_le_bytes());
        }
    }

    fn rows(n: &Node, lv: u8, out: &mut Vec<u8>, buf: &mut Vec<u8>) {
        let mut put = |t: &str| {
            let o = buf.len() as u32;
            buf.extend(t.as_bytes());
            buf.push(0);
            o
        };
        out.extend(1u16.to_le_bytes());
        out.push(lv);
        out.push(0);
        out.extend(put(&n.ty).to_le_bytes());
        out.extend(put(&n.name).to_le_bytes());
        out.extend((-1i32).to_le_bytes());
        out.extend(0i32.to_le_bytes());
        out.extend(n.flags.to_le_bytes());
        for k in &n.kids {
            rows(k, lv + 1, out, buf);
        }
    }

    /// a format 17 serialized file with one Shader object (the pre-2019.3 layout: one segment per platform) holding a vertex and a pixel stage
    pub(super) fn serialized(with_tree: bool) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let layout = "SerializedShader: string m_Name\nShader: string m_Name; SerializedShader m_ParsedForm; vector<data:unsigned int> platforms; vector<data:unsigned int> offsets; vector<data:unsigned int> compressedLengths; vector<data:unsigned int> decompressedLengths; vector<data:UInt8> compressedBlob";
        let root = tree_from_layout("Shader", layout).unwrap();
        let ((vc, vk), (pc, pk)) = (fake(1), fake(5));
        // program code: the 0x60 header, then the stage blocks
        let mut prog = vec![];
        let mut h = [0i32; 24];
        for i in 0..6 {
            h[6 + i] = -1;
        }
        h[6] = 0;
        h[12] = 0x880;
        h[18] = 0x100;
        h[7] = 0x980;
        h[13] = 0x880;
        h[19] = 0x100;
        for x in h {
            prog.extend(x.to_le_bytes());
        }
        prog.extend(&vc);
        prog.extend(&vk);
        prog.extend(&pc);
        prog.extend(&pk);
        // subprogram: version, type, 12 bytes, keyword count, code
        let mut sub = vec![];
        sub.extend(0i32.to_le_bytes());
        sub.extend(0i32.to_le_bytes());
        sub.extend([0u8; 12]);
        sub.extend(0i32.to_le_bytes());
        sub.extend((prog.len() as i32).to_le_bytes());
        sub.extend(&prog);
        pad4(&mut sub);
        let mut seg = vec![];
        seg.extend(1u32.to_le_bytes());
        seg.extend(12u32.to_le_bytes());
        seg.extend((sub.len() as u32).to_le_bytes());
        seg.extend(&sub);
        let blob = lz4_lit(&seg);
        let mut obj = vec![];
        s(&mut obj, "Test/Shader");
        s(&mut obj, "Test/Shader");
        arr_u32(&mut obj, &[11]);
        arr_u32(&mut obj, &[0]);
        arr_u32(&mut obj, &[blob.len() as u32]);
        arr_u32(&mut obj, &[seg.len() as u32]);
        obj.extend((blob.len() as i32).to_le_bytes());
        obj.extend(&blob);
        pad4(&mut obj);
        let (mut tr, mut buf) = (vec![], vec![]);
        rows(&root, 0, &mut tr, &mut buf);
        let mut meta = vec![];
        meta.extend(b"2017.4.9f1\0");
        meta.extend(0i32.to_le_bytes());
        meta.push(with_tree as u8);
        meta.extend(1i32.to_le_bytes());
        meta.extend(48i32.to_le_bytes());
        meta.push(0);
        meta.extend((-1i16).to_le_bytes());
        meta.extend([7u8; 16]);
        if with_tree {
            meta.extend(((tr.len() / 24) as i32).to_le_bytes());
            meta.extend((buf.len() as i32).to_le_bytes());
            meta.extend(&tr);
            meta.extend(&buf);
        }
        meta.extend(1i32.to_le_bytes()); // objects
        while (20 + meta.len()) % 4 != 0 {
            meta.push(0);
        }
        meta.extend(1i64.to_le_bytes());
        meta.extend(0u32.to_le_bytes());
        meta.extend((obj.len() as u32).to_le_bytes());
        meta.extend(0i32.to_le_bytes());
        let doff = (20 + meta.len() + 15) & !15;
        let mut f = vec![];
        let fsz = doff + obj.len();
        f.extend((meta.len() as u32).to_be_bytes());
        f.extend((fsz as u32).to_be_bytes());
        f.extend(17u32.to_be_bytes());
        f.extend((doff as u32).to_be_bytes());
        f.extend([0u8, 0, 0, 0]);
        f.extend(&meta);
        f.resize(doff, 0);
        f.extend(&obj);
        (f, vc, vk, pc, pk)
    }

    #[test]
    fn a_serialized_shader_gives_its_stages() {
        let (f, vc, vk, pc, pk) = serialized(true);
        if let Ok(p) = std::env::var("NX_DUMP_SER") {
            std::fs::write(p, &f).unwrap();
        }
        assert!(is_serialized(&f));
        let mut out = vec![];
        let mut trees = Trees::new();
        unity_shaders(&f, "/Data/a.assets", &HashMap::new(), &mut trees, false, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].stage, &out[0].ctl, &out[0].code), (Stage::Vs, &vc, &vk));
        assert_eq!((out[1].stage, &out[1].ctl, &out[1].code), (Stage::Ps, &pc, &pk));
        assert_eq!(out[0].label, "/Data/a.assets#Test/Shader#var11.0");
        assert_eq!(trees.len(), 1, "the file's own tree is recorded");
        // a file without a tree needs a borrowed one: none known, NoTree; the tree the first file recorded, and it reads the same
        let (bare, ..) = serialized(false);
        assert!(is_serialized(&bare));
        let mut none = vec![];
        assert!(matches!(unity_shaders(&bare, "x", &HashMap::new(), &mut Trees::new(), false, &mut none), Err(UErr::NoTree(_))));
        let mut again = vec![];
        unity_shaders(&bare, "/Data/b.assets", &HashMap::new(), &mut trees, true, &mut again).unwrap();
        assert_eq!(again.len(), 2);
        assert_eq!(again[1].code, out[1].code);
    }

    #[test]
    fn a_bundle_holds_the_file() {
        let (f, ..) = serialized(true);
        let block = lz4_lit(&f);
        let mut info = vec![0u8; 16];
        info.extend(1u32.to_be_bytes());
        info.extend((f.len() as u32).to_be_bytes());
        info.extend((block.len() as u32).to_be_bytes());
        info.extend(3u16.to_be_bytes()); // LZ4HC
        info.extend(1u32.to_be_bytes());
        info.extend(0u64.to_be_bytes());
        info.extend((f.len() as u64).to_be_bytes());
        info.extend(4u32.to_be_bytes());
        info.extend(b"CAB-1\0");
        let info_c = lz4_lit(&info);
        let mut b = b"UnityFS\0".to_vec();
        b.extend(6u32.to_be_bytes());
        b.extend(b"5.x.x\0");
        b.extend(b"2017.4.9f1\0");
        b.extend(((b.len() + 20 + info_c.len() + block.len()) as u64).to_be_bytes());
        b.extend((info_c.len() as u32).to_be_bytes());
        b.extend((info.len() as u32).to_be_bytes());
        b.extend(2u32.to_be_bytes());
        b.extend(&info_c);
        b.extend(&block);
        let files = bundle_files(&b).unwrap();
        assert_eq!((files.len(), files[0].0.as_str(), &files[0].1), (1, "CAB-1", &f));
        let mut out = vec![];
        unity_shaders(&b, "/b.bundle", &HashMap::new(), &mut Trees::new(), false, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[0].label.starts_with("/b.bundle:CAB-1#"));
    }

    #[test]
    fn the_known_layouts_all_parse() {
        let t = known_trees().unwrap();
        assert_eq!(t.len(), 9);
        let shader = t.values().find(|n| n.ty == "Shader").unwrap();
        assert!(shader.kids.iter().any(|k| k.name == "compressedBlob"));
    }
}
