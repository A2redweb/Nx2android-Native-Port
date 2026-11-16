//! Reads `stage|name|desc|count` lines on stdin, prints each one's classification and its refl_bin row as hex (for tools/oracle checks against shader_vk.py).
use nx_shader::{bind::*, json::Json, spirv::Binding};
use std::io::BufRead;

fn main() {
    let res = Json::parse(r#"{"sbufs":[{"binding":9,"slot":0,"cb_slot":0,"cb_offset":132,"flags":1},{"binding":4,"slot":1,"cb_slot":3,"cb_offset":2,"flags":0},{"binding":2,"slot":2,"cb_slot":0,"cb_offset":68,"flags":0}],"fetch1d":[1,130]}"#).unwrap();
    for l in std::io::stdin().lock().lines() {
        let l = l.unwrap();
        let f: Vec<&str> = l.split('|').collect();
        let desc: &'static str = match f[2] {
            "ubo" => "ubo",
            "ssbo" => "ssbo",
            "sampled" => "sampled",
            "storage" => "storage",
            "tbuf" => "tbuf",
            "storage_tbuf" => "storage_tbuf",
            "image" => "image",
            "sampler" => "sampler",
            _ => "?",
        };
        let b = Binding { name: f[1].into(), set: 0, binding: 7, desc, count: f[3].parse().unwrap(), dim: Some(1), arrayed: 0, ms: 0, fmt: 0 };
        let c = classify(f[0], &b, &res, f.get(4) == Some(&"dyn"));
        let bin = refl_bin(&[(b, c)], &[], [0, 0, 0]);
        println!("{}", bin[20..].iter().map(|x| format!("{x:02x}")).collect::<String>());
    }
}
