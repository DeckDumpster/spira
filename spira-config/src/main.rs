//! `spira-config` — validate, read, export and convert `spira.toml`.
//!
//!   spira-config validate [file]        exit 1 and name the TOML path on the first error
//!   spira-config get <dotted.path>      one value read out of the document
//!   spira-config export --sh            `[spira]` as quoted KEY=value lines
//!   spira-config convert ...            spira.conf + repo-map + *.fayth -> spira.toml
//!   spira-config schema                 the JSON Schema spira.toml is validated against
//!
//! `validate`, `get` and `export` read `spira.toml` from: the file argument if one is
//! given, else `$SPIRA_TOML`, else `./spira.toml`, else stdin — the same "explicit, then
//! configured, then derived" order `conf.sh` itself uses for `spira.conf`.

use std::env;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use spira_config::{convert, export_sh, get_path, json_schema, validate};

fn read_input(file: Option<&str>) -> Result<String, String> {
    match file {
        Some("-") => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| e.to_string())?;
            Ok(s)
        }
        Some(f) => fs::read_to_string(f).map_err(|e| format!("{f}: {e}")),
        None => {
            if let Ok(p) = env::var("SPIRA_TOML") {
                return fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"));
            }
            if Path::new("spira.toml").exists() {
                return fs::read_to_string("spira.toml").map_err(|e| e.to_string());
            }
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| e.to_string())?;
            Ok(s)
        }
    }
}

fn cmd_validate(file: Option<&str>) -> ExitCode {
    let text = match read_input(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("spira-config: {e}");
            return ExitCode::FAILURE;
        }
    };
    match validate(&text) {
        Ok(_) => {
            println!("ok");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spira-config: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_get(path: &str, file: Option<&str>) -> ExitCode {
    let text = match read_input(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("spira-config: {e}");
            return ExitCode::FAILURE;
        }
    };
    let doc = match validate(&text) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("spira-config: {e}");
            return ExitCode::FAILURE;
        }
    };
    match get_path(&doc, path) {
        Some(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        None => ExitCode::FAILURE,
    }
}

fn cmd_export_sh(file: Option<&str>) -> ExitCode {
    let text = match read_input(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("spira-config: {e}");
            return ExitCode::FAILURE;
        }
    };
    match validate(&text) {
        Ok(doc) => {
            print!("{}", export_sh(&doc));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spira-config: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_schema() -> ExitCode {
    match serde_json::to_string_pretty(&json_schema()) {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spira-config: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_convert(args: &[String]) -> ExitCode {
    let mut conf_path = None;
    let mut repo_map_path = None;
    let mut fayth_paths: Vec<String> = Vec::new();
    let mut out_path = None;
    let mut home = env::var("HOME").unwrap_or_default();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--conf" => {
                conf_path = args.get(i + 1).cloned();
                i += 2;
            }
            "--repo-map" => {
                repo_map_path = args.get(i + 1).cloned();
                i += 2;
            }
            "--fayth" => {
                if let Some(p) = args.get(i + 1) {
                    fayth_paths.push(p.clone());
                }
                i += 2;
            }
            "--out" => {
                out_path = args.get(i + 1).cloned();
                i += 2;
            }
            "--home" => {
                if let Some(h) = args.get(i + 1) {
                    home = h.clone();
                }
                i += 2;
            }
            other => {
                eprintln!("spira-config convert: unknown argument {other:?}");
                return ExitCode::FAILURE;
            }
        }
    }

    let conf_text = conf_path.as_deref().map(fs::read_to_string).transpose();
    let conf_text = match conf_text {
        Ok(t) => t.unwrap_or_default(),
        Err(e) => {
            eprintln!("spira-config convert: {e}");
            return ExitCode::FAILURE;
        }
    };
    let repo_map_text = repo_map_path.as_deref().map(fs::read_to_string).transpose();
    let repo_map_text = match repo_map_text {
        Ok(t) => t.unwrap_or_default(),
        Err(e) => {
            eprintln!("spira-config convert: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut fayth_texts: Vec<(String, String)> = Vec::new();
    for p in &fayth_paths {
        match fs::read_to_string(p) {
            Ok(t) => fayth_texts.push((p.clone(), t)),
            Err(e) => {
                eprintln!("spira-config convert: {p}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let fayth_refs: Vec<(&str, &str)> = fayth_texts
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();

    let (doc, warnings) = convert::convert(&conf_text, &home, &repo_map_text, &fayth_refs);
    for w in &warnings.0 {
        eprintln!("spira-config convert: {w}");
    }
    let out = match toml::to_string_pretty(&doc) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("spira-config convert: {e}");
            return ExitCode::FAILURE;
        }
    };
    match out_path {
        Some(p) => {
            if let Err(e) = fs::write(&p, out) {
                eprintln!("spira-config convert: {p}: {e}");
                return ExitCode::FAILURE;
            }
        }
        None => print!("{out}"),
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("validate") => cmd_validate(args.get(1).map(String::as_str)),
        Some("get") => match args.get(1) {
            Some(path) => cmd_get(path, args.get(2).map(String::as_str)),
            None => {
                eprintln!("usage: spira-config get <path> [file]");
                ExitCode::FAILURE
            }
        },
        Some("export") => {
            if args.get(1).map(String::as_str) == Some("--sh") {
                cmd_export_sh(args.get(2).map(String::as_str))
            } else {
                eprintln!("usage: spira-config export --sh [file]");
                ExitCode::FAILURE
            }
        }
        Some("convert") => cmd_convert(&args[1..]),
        Some("schema") => cmd_schema(),
        _ => {
            eprintln!(
                "usage: spira-config <validate|get|export|convert|schema> ...\n\
                 \n\
                 \x20 validate [file]\n\
                 \x20 get <dotted.path> [file]\n\
                 \x20 export --sh [file]\n\
                 \x20 convert --conf F --repo-map F [--fayth F]... [--home DIR] [--out F]\n\
                 \x20 schema"
            );
            ExitCode::FAILURE
        }
    }
}
