//! `nxn`: the command-line face of the core, runnable on a PC or in Termux. The Android app calls the same crates.
//!
//!   nxn games <profiles dir> [--all] [--json] [--overlay <overlay_default.tsv>]   the games the porter knows
//!   nxn dump <file>                                                              the files inside an NSP / NSZ / XCI / XCZ
//!   nxn keys <prod.keys>                                                         which keys the file holds (names only)
//!   nxn extract <prod.keys> <dump> <out dir>                                    ExeFS, RomFS and control.nacp of a base game
//!   nxn title <file> [<prod.keys>]                                               the title ID of a dump (ticket, then NCA headers, then the file name)

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage:\n  nxn games <profiles dir> [--all] [--json] [--overlay <overlay_default.tsv>]\n  nxn dump <file>\n  nxn keys <prod.keys>\n  nxn title <file> [<prod.keys>]\n  nxn extract <prod.keys> <dump> <out dir>");
    ExitCode::from(2)
}

fn games(args: &[String]) -> Result<(), String> {
    let Some(dir) = args.first() else { return Err("games needs the profiles folder (nxnative/dm/games)".into()) };
    let flag = |f: &str| args.iter().any(|a| a == f);
    let overlay = args.iter().position(|a| a == "--overlay").and_then(|i| args.get(i + 1));
    let controls = match overlay {
        Some(p) => Some(nx_profile::overlay_controls(&std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?)),
        None => None,
    };
    let entries = nx_profile::catalog(Path::new(dir), controls.as_ref(), !flag("--all"))?;
    if flag("--json") {
        println!("{}", nx_profile::to_json(&entries));
    } else {
        for e in &entries {
            println!("{}\t{}\t{}\t{}", e.id, e.status, e.package, e.name);
        }
        eprintln!("{} games", entries.len());
    }
    Ok(())
}

fn dump(args: &[String]) -> Result<(), String> {
    let Some(path) = args.first() else { return Err("dump needs a file".into()) };
    let f = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let entries = nx_container::dump_files(&mut BufReader::new(f)).map_err(|e| format!("{path}: {e}"))?;
    if entries.is_empty() {
        return Err(format!("{path}: not an NSP / NSZ / XCI / XCZ"));
    }
    for e in &entries {
        println!("{:>14}  {:>14}  {}", e.offset, e.size, e.name);
    }
    Ok(())
}

fn keys(args: &[String]) -> Result<(), String> {
    let Some(path) = args.first() else { return Err("keys needs a prod.keys file".into()) };
    let k = nx_keys::Keys::load(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    println!("{} keys, {} master keys, header_key {}", k.len(), k.master_keys(), if k.header_key().is_some() { "present" } else { "MISSING" });
    for n in &k.rejected {
        println!("not valid hex: {n}");
    }
    Ok(())
}

fn title(args: &[String]) -> Result<(), String> {
    let Some(path) = args.first() else { return Err("title needs a dump file".into()) };
    let keys = match args.get(1) {
        Some(k) => Some(nx_keys::Keys::load(Path::new(k)).map_err(|e| format!("{k}: {e}"))?),
        None => None,
    };
    let f = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    match nx_nca::title_of(&mut BufReader::new(f), Path::new(path), keys.as_ref()).map_err(|e| format!("{path}: {e}"))? {
        Some(t) => {
            println!("{:016X}", t.id);
            for o in t.others {
                eprintln!("[RED] another title in this file, not taken: {o:016X}");
            }
            Ok(())
        }
        None => Err(format!("{path}: no title ID found (no ticket, no readable meta NCA, no [title ID] in the name)")),
    }
}

fn extract(args: &[String]) -> Result<(), String> {
    let [keys, dump, out] = args else { return Err("extract needs <prod.keys> <dump> <out dir>".into()) };
    let keys = nx_keys::Keys::load(Path::new(keys)).map_err(|e| format!("{keys}: {e}"))?;
    let f = File::open(dump).map_err(|e| format!("{dump}: {e}"))?;
    let r = nx_extract::extract_base(BufReader::new(f), &keys, dump, Path::new(out)).map_err(|e| e.to_string())?;
    println!("title {:016x}: ExeFS {} files, RomFS {} files ({:.1} MB), control.nacp {}", r.title_id, r.exefs_files, r.romfs_files, r.romfs_bytes as f64 / 1048576.0, if r.nacp { "written" } else { "not found" });
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("games") => games(&args[1..]),
        Some("dump") => dump(&args[1..]),
        Some("keys") => keys(&args[1..]),
        Some("title") => title(&args[1..]),
        Some("extract") => extract(&args[1..]),
        _ => return usage(),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
