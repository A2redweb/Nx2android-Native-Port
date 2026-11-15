//! `rewrite <nso> <out dir> <name>`: writes <name>.patched.bin, .fixups.tsv, .gates.tsv, .sites.tsv, .cuts.tsv, .obs.tsv
fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 4 {
        eprintln!("usage: rewrite <nso> <out dir> <name>");
        std::process::exit(2);
    }
    let d = std::fs::read(&a[1]).expect("read");
    match nx_rewrite::rewrite(&d, &[]) {
        Ok(o) => {
            std::fs::create_dir_all(&a[2]).unwrap();
            let p = |e: &str| format!("{}/{}.{e}", a[2], a[3]);
            std::fs::write(p("patched.bin"), &o.patched).unwrap();
            for (e, s) in [("fixups.tsv", &o.fixups), ("gates.tsv", &o.gates), ("sites.tsv", &o.sites), ("cuts.tsv", &o.cuts), ("obs.tsv", &o.obs)] {
                std::fs::write(p(e), s).unwrap();
            }
            println!("{} {:?} data blocks {}", a[3], o.stats, o.data_blocks.len());
        }
        Err(e) => {
            eprintln!("rewrite failed: {e}");
            std::process::exit(1);
        }
    }
}
