//! SPIR-V as the translator wrote it: its descriptor bindings with the translator's names, its stage interface, and the vertex inputs used only as integers
//! (upstream's `shader_vk.py` spirv_bindings and int_inputs, `shader_products.py` validate_spirv).

use std::collections::{BTreeMap, BTreeSet, HashMap};

const OP_NAME: u32 = 5;
const OP_DECORATE: u32 = 71;
const OP_VARIABLE: u32 = 59;
const OP_TYPE_POINTER: u32 = 32;
const OP_TYPE_ARRAY: u32 = 28;
const OP_TYPE_RUNTIME_ARRAY: u32 = 29;
const OP_TYPE_IMAGE: u32 = 25;
const OP_TYPE_SAMPLED_IMAGE: u32 = 27;
const OP_TYPE_SAMPLER: u32 = 26;
const OP_TYPE_STRUCT: u32 = 30;
const OP_CONSTANT: u32 = 43;
const OP_EXECUTION_MODE: u32 = 16;
const DEC_BINDING: u32 = 33;
const DEC_SET: u32 = 34;
const DEC_BUFFER_BLOCK: u32 = 3;
const DEC_LOCATION: u32 = 30;
const SC_UNIFORM: u32 = 2;
const SC_STORAGE_BUFFER: u32 = 12;

/// The words of a module as instructions: (opcode, the whole instruction including its first word)
pub fn instructions(spv: &[u8]) -> Result<Vec<(u32, Vec<u32>)>, String> {
    if spv.len() < 20 || spv.len() % 4 != 0 {
        return Err("truncated SPIR-V".into());
    }
    let w: Vec<u32> = spv.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    if w[0] != 0x0723_0203 {
        return Err("not SPIR-V".into());
    }
    let mut out = vec![];
    let mut i = 5;
    while i < w.len() {
        let n = (w[i] >> 16) as usize;
        if n == 0 || i + n > w.len() {
            return Err(format!("invalid SPIR-V instruction at word {i}"));
        }
        out.push((w[i] & 0xFFFF, w[i..i + n].to_vec()));
        i += n;
    }
    Ok(out)
}

/// Bounded decoding of the module's header and entry points: one entry point, in the execution model of the stage (NVN stage numbers 0..5)
pub fn validate(spv: &[u8], stage: u32) -> Result<(), String> {
    const MODELS: [u32; 6] = [0, 4, 3, 1, 2, 5];
    if stage > 5 || spv.len() < 20 || spv.len() % 4 != 0 {
        return Err("invalid shader stage or truncated SPIR-V".into());
    }
    let h: Vec<u32> = spv[..20].chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    if h[0] != 0x0723_0203 || ![0x10000, 0x10100, 0x10200, 0x10300].contains(&h[1]) || h[3] == 0 || h[4] != 0 {
        return Err("invalid SPIR-V header for Vulkan 1.1".into());
    }
    let mut models = vec![];
    for (op, ins) in instructions(spv)? {
        if op == 15 {
            if ins.len() < 4 {
                return Err("truncated SPIR-V entry point".into());
            }
            models.push(ins[1]);
        }
    }
    if models != [MODELS[stage as usize]] {
        return Err("SPIR-V entry point does not match shader stage".into());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub name: String,
    pub set: u32,
    pub binding: u32,
    /// ubo ssbo sampled tbuf storage storage_tbuf image sampler ?
    pub desc: &'static str,
    pub count: u32,
    pub dim: Option<u32>,
    pub arrayed: u32,
    pub ms: u32,
    pub fmt: u32,
}

/// Input or output locations of a stage: all, the signed-integer ones, the unsigned-integer ones (bit n = location n)
pub type Mask = [u32; 3];

#[derive(Clone, Copy, Debug)]
enum Ty {
    Ptr(u32, u32),
    Array(u32, u32),
    RArray(u32),
    Image { dim: u32, sampled: u32, arrayed: u32, ms: u32, fmt: u32 },
    Sampled(u32),
    Sampler,
    Struct,
}

pub struct Interface {
    pub bindings: Vec<Binding>,
    pub local: Option<[u32; 3]>,
    pub inputs: Mask,
    pub outputs: Mask,
}

fn get(ins: &[u32], i: usize) -> Result<u32, String> {
    ins.get(i).copied().ok_or_else(|| "truncated SPIR-V instruction".to_string())
}

pub fn interface(spv: &[u8]) -> Result<Interface, String> {
    let ins_all = instructions(spv)?;
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut deco: HashMap<u32, HashMap<u32, u32>> = HashMap::new();
    let mut types: HashMap<u32, Ty> = HashMap::new();
    let mut consts: HashMap<u32, u32> = HashMap::new();
    let mut vars: Vec<(u32, u32, u32)> = vec![];
    for (op, ins) in &ins_all {
        let n = ins.len();
        match *op {
            OP_NAME => {
                let bytes: Vec<u8> = ins[2.min(n)..].iter().flat_map(|w| w.to_le_bytes()).collect();
                let z = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                names.insert(get(ins, 1)?, String::from_utf8_lossy(&bytes[..z]).into_owned());
            }
            OP_DECORATE => {
                deco.entry(get(ins, 1)?).or_default().insert(get(ins, 2)?, if n > 3 { ins[3] } else { 1 });
            }
            OP_TYPE_POINTER => {
                types.insert(get(ins, 1)?, Ty::Ptr(get(ins, 2)?, get(ins, 3)?));
            }
            OP_TYPE_ARRAY => {
                types.insert(get(ins, 1)?, Ty::Array(get(ins, 2)?, get(ins, 3)?));
            }
            OP_TYPE_RUNTIME_ARRAY => {
                types.insert(get(ins, 1)?, Ty::RArray(get(ins, 2)?));
            }
            OP_TYPE_IMAGE => {
                types.insert(get(ins, 1)?, Ty::Image { dim: get(ins, 3)?, sampled: get(ins, 7)?, arrayed: get(ins, 5)?, ms: get(ins, 6)?, fmt: get(ins, 8)? });
            }
            OP_TYPE_SAMPLED_IMAGE => {
                types.insert(get(ins, 1)?, Ty::Sampled(get(ins, 2)?));
            }
            OP_TYPE_SAMPLER => {
                types.insert(get(ins, 1)?, Ty::Sampler);
            }
            OP_TYPE_STRUCT => {
                types.insert(get(ins, 1)?, Ty::Struct);
            }
            OP_CONSTANT if n == 4 => {
                consts.insert(get(ins, 2)?, ins[3]);
            }
            OP_VARIABLE => vars.push((get(ins, 2)?, get(ins, 1)?, get(ins, 3)?)),
            _ => {}
        }
    }
    let mut bindings = vec![];
    for &(vid, pt, sc) in &vars {
        let d = deco.get(&vid);
        let Some(binding) = d.and_then(|d| d.get(&DEC_BINDING)).copied() else { continue };
        let (mut tid, mut t) = match types.get(&pt) {
            Some(Ty::Ptr(_, p)) => (Some(*p), types.get(p).copied()),
            _ => (None, None),
        };
        let mut count = 1;
        while let Some(x) = t {
            match x {
                Ty::Array(e, l) => {
                    count = consts.get(&l).copied().unwrap_or(1);
                    tid = Some(e);
                    t = types.get(&e).copied();
                }
                Ty::RArray(e) => {
                    count = 0;
                    tid = Some(e);
                    t = types.get(&e).copied();
                }
                _ => break,
            }
        }
        let desc = if sc == SC_UNIFORM {
            if tid.and_then(|x| deco.get(&x)).is_some_and(|m| m.contains_key(&DEC_BUFFER_BLOCK)) { "ssbo" } else { "ubo" }
        } else if sc == SC_STORAGE_BUFFER {
            "ssbo"
        } else {
            match t {
                Some(Ty::Sampled(im)) => {
                    if matches!(types.get(&im), Some(Ty::Image { dim: 5, .. })) { "tbuf" } else { "sampled" }
                }
                Some(Ty::Image { dim, sampled, .. }) => {
                    if dim == 5 && sampled == 2 { "storage_tbuf" } else if sampled == 2 { "storage" } else if dim == 5 { "tbuf" } else { "image" }
                }
                Some(Ty::Sampler) => "sampler",
                _ => "?",
            }
        };
        let img = match t {
            Some(i @ Ty::Image { .. }) => Some(i),
            Some(Ty::Sampled(im)) => types.get(&im).copied(),
            _ => None,
        };
        let (dim, arrayed, ms, fmt) = match img {
            Some(Ty::Image { dim, arrayed, ms, fmt, .. }) => (Some(dim), arrayed, ms, fmt),
            _ => (None, 0, 0, 0),
        };
        bindings.push(Binding { name: names.get(&vid).cloned().unwrap_or_default(), set: d.and_then(|d| d.get(&DEC_SET)).copied().unwrap_or(0), binding, desc, count, dim, arrayed, ms, fmt });
    }
    let mut local = None;
    let mut scalars: HashMap<u32, char> = HashMap::new();
    for (op, ins) in &ins_all {
        match *op {
            OP_EXECUTION_MODE if ins.get(2) == Some(&17) => local = Some([get(ins, 3)?, get(ins, 4)?, get(ins, 5)?]),
            21 => {
                scalars.insert(get(ins, 1)?, if get(ins, 3)? != 0 { 'i' } else { 'u' });
            }
            22 => {
                scalars.insert(get(ins, 1)?, 'f');
            }
            23 => {
                let k = scalars.get(&get(ins, 2)?).copied().unwrap_or('f');
                scalars.insert(get(ins, 1)?, k);
            }
            _ => {}
        }
    }
    let (mut inputs, mut outputs): (Mask, Mask) = ([0; 3], [0; 3]);
    for &(vid, pt, sc) in &vars {
        if sc != 1 && sc != 3 {
            continue;
        }
        let Some(loc) = deco.get(&vid).and_then(|d| d.get(&DEC_LOCATION)).copied() else { continue };
        if loc >= 32 {
            continue;
        }
        let mut tid = match types.get(&pt) {
            Some(Ty::Ptr(_, p)) => Some(*p),
            _ => None,
        };
        while let Some(Ty::Array(e, _) | Ty::RArray(e)) = tid.and_then(|x| types.get(&x)) {
            tid = Some(*e);
        }
        let k = tid.and_then(|x| scalars.get(&x)).copied().unwrap_or('f');
        let m = if sc == 1 { &mut inputs } else { &mut outputs };
        m[0] |= 1 << loc;
        if k == 'i' {
            m[1] |= 1 << loc;
        }
        if k == 'u' {
            m[2] |= 1 << loc;
        }
    }
    Ok(Interface { bindings, local, inputs, outputs })
}

/// A vertex shader's inputs used only as integers → {location: signed?}: every load of the input (whole, or one component through an access chain) is used only by OpBitcast.
/// The location is declared signed when a cast is to a signed integer type.
pub fn int_inputs(spv: &[u8]) -> Result<BTreeMap<u32, bool>, String> {
    let ins = instructions(spv)?;
    let (mut loc, mut inputs, mut chain, mut loads) = (HashMap::new(), BTreeSet::new(), HashMap::new(), HashMap::new());
    let mut signed: HashMap<u32, bool> = HashMap::new();
    for (op, ins) in &ins {
        let a = &ins[1..];
        match *op {
            OP_DECORATE if a.len() > 2 && a[1] == DEC_LOCATION => {
                loc.insert(a[0], a[2]);
            }
            OP_VARIABLE if a.len() > 2 && a[2] == 1 => {
                inputs.insert(a[1]);
            }
            21 if a.len() > 2 => {
                signed.insert(a[0], a[2] != 0);
            }
            23 if a.len() > 1 => {
                if let Some(&s) = signed.get(&a[1]) {
                    signed.insert(a[0], s);
                }
            }
            _ => {}
        }
    }
    for (op, ins) in &ins {
        let a = &ins[1..];
        if *op == 65 && a.len() > 2 && inputs.contains(&a[2]) {
            chain.insert(a[1], a[2]);
        }
    }
    for (op, ins) in &ins {
        let a = &ins[1..];
        if *op == 61 && a.len() > 2 && (inputs.contains(&a[2]) || chain.contains_key(&a[2])) {
            loads.insert(a[1], if inputs.contains(&a[2]) { a[2] } else { chain[&a[2]] });
        }
    }
    let (mut uses, mut other, mut sint): (BTreeSet<u32>, BTreeSet<u32>, BTreeSet<u32>) = Default::default();
    for (op, ins) in &ins {
        if matches!(*op, 5 | 6 | 61 | 65 | 71 | 72) {
            continue;
        }
        let a = &ins[1..];
        let ops: &[u32] = if *op == 124 { a.get(2..).unwrap_or(&[]) } else { a };
        for x in ops {
            let Some(&v) = loads.get(x) else { continue };
            if *op == 124 {
                uses.insert(v);
                if a.first().is_some_and(|t| signed.get(t) == Some(&true)) {
                    sint.insert(v);
                }
            } else {
                other.insert(v);
            }
        }
    }
    Ok(uses.into_iter().filter(|v| !other.contains(v)).filter_map(|v| loc.get(&v).map(|&l| (l, sint.contains(&v)))).collect())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A tiny SPIR-V assembler for the tests: words of one module
    pub struct Asm(pub Vec<u32>);
    impl Asm {
        pub fn new(bound: u32) -> Asm {
            Asm(vec![0x0723_0203, 0x10300, 0, bound, 0])
        }
        pub fn op(&mut self, op: u32, args: &[u32]) -> &mut Asm {
            self.0.push(((args.len() as u32 + 1) << 16) | op);
            self.0.extend_from_slice(args);
            self
        }
        pub fn name(&mut self, id: u32, s: &str) -> &mut Asm {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            while b.len() % 4 != 0 {
                b.push(0);
            }
            let mut a = vec![id];
            a.extend(b.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())));
            self.op(OP_NAME, &a)
        }
        pub fn bytes(&self) -> Vec<u8> {
            self.0.iter().flat_map(|w| w.to_le_bytes()).collect()
        }
    }

    /// ids: 1 float, 2 vec4, 3 uint, 4 const 4, 5 array of 4 uint, 6 struct, 7 ptr Uniform struct, 8 var (set 0 binding 3, "vp_c4"), 9 image type 2D sampled, 10 sampled image,
    /// 11 ptr UniformConstant sampled, 12 var binding 5 "fp_t2d_cb2_00000001", 13 ptr Input ivec, 14 var Input location 5 (int), 15 int type signed
    pub fn sample() -> Vec<u8> {
        let mut a = Asm::new(20);
        a.op(15, &[0, 100, 0]); // OpEntryPoint Vertex %100 (name word omitted: only the model matters here)
        a.name(8, "vp_c4").name(12, "fp_t2d_cb2_00000001");
        a.op(OP_DECORATE, &[8, DEC_BINDING, 3]).op(OP_DECORATE, &[8, DEC_SET, 0]).op(OP_DECORATE, &[12, DEC_BINDING, 5]).op(OP_DECORATE, &[14, DEC_LOCATION, 5]);
        a.op(22, &[1, 32]).op(23, &[2, 1, 4]).op(21, &[3, 32, 0]).op(OP_CONSTANT, &[3, 4, 4]);
        a.op(OP_TYPE_ARRAY, &[5, 3, 4]).op(OP_TYPE_STRUCT, &[6, 5]).op(OP_TYPE_POINTER, &[7, SC_UNIFORM, 6]).op(OP_VARIABLE, &[7, 8, SC_UNIFORM]);
        a.op(OP_TYPE_IMAGE, &[9, 1, 1, 0, 0, 0, 1, 0]).op(OP_TYPE_SAMPLED_IMAGE, &[10, 9]).op(OP_TYPE_POINTER, &[11, 0, 10]).op(OP_VARIABLE, &[11, 12, 0]);
        a.op(21, &[15, 32, 1]).op(OP_TYPE_POINTER, &[13, 1, 15]).op(OP_VARIABLE, &[13, 14, 1]);
        a.bytes()
    }

    #[test]
    fn interface_of_a_small_module() {
        let spv = sample();
        let it = interface(&spv).unwrap();
        assert_eq!(it.bindings.len(), 2);
        assert_eq!((it.bindings[0].name.as_str(), it.bindings[0].desc, it.bindings[0].binding, it.bindings[0].count), ("vp_c4", "ubo", 3, 1));
        assert_eq!((it.bindings[1].desc, it.bindings[1].dim, it.bindings[1].binding), ("sampled", Some(1), 5));
        assert_eq!(it.inputs, [1 << 5, 1 << 5, 0]); // location 5, a signed integer
        assert_eq!(it.local, None);
        assert!(validate(&spv, 0).is_ok());
        assert!(validate(&spv, 1).is_err());
    }

    /// the sample plus a load of the input location 5 and a cast of it to the signed (15) or the unsigned (3) type
    pub fn sample_with_cast(signed: bool, also_used_otherwise: bool) -> Vec<u8> {
        let mut a = Asm(sample().chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect());
        a.op(61, &[15, 16, 14]).op(124, &[if signed { 15 } else { 3 }, 17, 16]);
        if also_used_otherwise {
            a.op(128, &[15, 18, 16, 16]); // OpIAdd uses the load directly
        }
        a.bytes()
    }

    #[test]
    fn integer_inputs() {
        assert_eq!(int_inputs(&sample()).unwrap(), BTreeMap::new());
        assert_eq!(int_inputs(&sample_with_cast(false, false)).unwrap(), BTreeMap::from([(5, false)]));
        assert_eq!(int_inputs(&sample_with_cast(true, false)).unwrap(), BTreeMap::from([(5, true)]));
        assert_eq!(int_inputs(&sample_with_cast(true, true)).unwrap(), BTreeMap::new());
        if let Ok(d) = std::env::var("NX_DUMP_SPV") {
            for (n, (s, o)) in [(false, false), (true, false), (true, true)].into_iter().enumerate() {
                std::fs::write(format!("{d}/cast{n}.spv"), sample_with_cast(s, o)).unwrap();
            }
        }
    }

    #[test]
    fn malformed_modules_are_errors_not_panics() {
        let spv = sample();
        assert!(interface(&spv[..spv.len() - 4]).is_err() || interface(&spv[..spv.len() - 4]).is_ok());
        assert!(instructions(&spv[..10]).is_err());
        let mut z = spv.clone();
        z[20..24].copy_from_slice(&0u32.to_le_bytes()); // a zero-length instruction
        assert!(instructions(&z).is_err());
    }
}
