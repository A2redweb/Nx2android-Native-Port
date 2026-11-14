//! `nxn`: the command-line face of the core, runnable on a PC or in Termux. The Android app calls the same crates.
//!
//!   nxn games <profiles dir> [--all] [--json] [--overlay <overlay_default.tsv>]   the games the porter knows
//!   nxn dump <file>                                                              the files inside an NSP / NSZ / XCI / XCZ
//!   nxn keys <prod.keys>                                                         which keys the file holds (names only)

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage:\n  nxn games <profiles dir> [--all] [--json] [--overlay <overlay_default.tsv>]\n  nxn dump <file>\n  nxn keys <prod.keys>");
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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("games") => games(&args[1..]),
        Some("dump") => dump(&args[1..]),
        Some("keys") => keys(&args[1..]),
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
