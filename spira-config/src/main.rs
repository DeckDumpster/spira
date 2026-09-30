//! `spira-config` — validate, read, export and convert `spira.toml`.
//!
//!   spira-config validate [file]        exit 1 and name the TOML path on the first error;
//!                                       also refuses a [spira] with no id_prefix (sp-k6m1m)
//!   spira-config get <dotted.path>      one value read out of the document
//!   spira-config export --sh            `[spira]` as quoted KEY=value lines
//!   spira-config convert ...            spira.conf + repo-map + *.fayth -> spira.toml
//!   spira-config set <path> <v> <file>  write one value into <file> in place
//!   spira-config unset <path> <file>    remove one value from <file> in place
//!   spira-config schema                 the JSON Schema spira.toml is validated against
//!   spira-config path-tail              the box's `spira.path` tail, or a refusal naming why
//!
//! `validate`, `get` and `export` read `spira.toml` from: the file argument if one is
//! given, else `$SPIRA_TOML`, else `./spira.toml`, else stdin — the same "explicit, then
//! configured, then derived" order `conf.sh` itself uses for `spira.conf`.

use std::env;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use spira_config::{
    convert, discover, export_sh, get_path, json_schema, load, set_path, shrink_reason,
    tail_refusals, unset_path, validate, validate_strict, write_atomic, SpiraToml,
};

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
    // STRICT: the config in force must name its id prefix (sp-k6m1m). doctor and the
    // release's pre-activate run this, so a release is never activated over one without it.
    match validate_strict(&text) {
        Ok((_, warnings)) => {
            for w in &warnings {
                eprintln!("spira-config: warning: {w}");
            }
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

/// `path-tail` — the box's own PATH tail every launcher appends after the release and system
/// directories (sp-c7b85, brain `runtime-is-a-release-2026-09-29`): `$SPIRA_PATH` if it is set
/// (nonempty), else `[spira].path` from the config `discover` finds, else empty. Prints the
/// tail on stdout and exits 0, or refuses (naming the entry) when a segment resolves inside a
/// release or a checkout — the one place this check runs, so `render.py`, `install.sh` and
/// `unit-ensure.sh` (bash and Python, which carry no logic of their own) get it from here
/// rather than each growing its own copy.
fn cmd_path_tail() -> ExitCode {
    let tail = match env::var("SPIRA_PATH") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => match discover(None).map(|p| load(&p)).transpose() {
            Ok(doc) => doc.and_then(|d| d.spira).and_then(|s| s.path).unwrap_or_default(),
            Err(e) => {
                eprintln!("spira-config path-tail: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let refusals = tail_refusals(&tail);
    if !refusals.is_empty() {
        eprintln!("spira-config path-tail: {}", refusals.join("; "));
        return ExitCode::FAILURE;
    }
    println!("{tail}");
    ExitCode::SUCCESS
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

/// Reads and validates `file` for `set`/`unset`, or starts from an empty document when it
/// does not exist yet — the file argument is required for both, unlike `validate`/`get`/
/// `export`, because their callers (`deploy.sh`, `install.sh`, `aeons.sh`) already know
/// exactly which file is in force and mutate it in place.
fn read_doc_or_default(file: &str) -> Result<SpiraToml, String> {
    if !Path::new(file).exists() {
        return Ok(SpiraToml::default());
    }
    let text = fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    if text.trim().is_empty() {
        Ok(SpiraToml::default())
    } else {
        validate(&text)
    }
}

fn write_doc(file: &str, doc: &SpiraToml, verb: &str) -> ExitCode {
    let out = match toml::to_string_pretty(doc) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("spira-config {verb}: {file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match fs::write(file, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spira-config {verb}: {file}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `set <dotted.path> <value> <file>` — write one path's value into `file` in place.
fn cmd_set(path: &str, value: &str, file: &str) -> ExitCode {
    let doc = match read_doc_or_default(file) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("spira-config: {e}");
            return ExitCode::FAILURE;
        }
    };
    let new_doc = match set_path(&doc, path, value) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("spira-config set: {e}");
            return ExitCode::FAILURE;
        }
    };
    write_doc(file, &new_doc, "set")
}

/// `unset <dotted.path> <file>` — remove one path's value from `file` in place, so the key
/// stops appearing rather than being left behind as an empty string.
fn cmd_unset(path: &str, file: &str) -> ExitCode {
    let doc = match read_doc_or_default(file) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("spira-config: {e}");
            return ExitCode::FAILURE;
        }
    };
    let new_doc = match unset_path(&doc, path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("spira-config unset: {e}");
            return ExitCode::FAILURE;
        }
    };
    write_doc(file, &new_doc, "unset")
}

fn cmd_convert(args: &[String]) -> ExitCode {
    let mut conf_path = None;
    let mut repo_map_path = None;
    let mut fayth_paths: Vec<String> = Vec::new();
    let mut out_path = None;
    let mut home = env::var("HOME").unwrap_or_default();
    let mut force_shrink = false;

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
            "--force-shrink" => {
                force_shrink = true;
                i += 1;
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

    let (mut doc, warnings) = match convert::convert(&conf_text, &home, &repo_map_text, &fayth_refs)
    {
        Ok(r) => r,
        Err(errors) => {
            for e in &errors {
                eprintln!("spira-config convert: {e}");
            }
            return ExitCode::FAILURE;
        }
    };
    for w in &warnings.0 {
        eprintln!("spira-config convert: {w}");
    }
    // A CALLER THAT OMITS --conf ENTIRELY IS ASKING FOR A NARROWER REFRESH (conf.sh's
    // persona-only regenerate passes just --fayth), not "this box has no [spira]/[repo]
    // config" — read_conf against empty text returns only defaults, and writing that over an
    // --out file with real content would silently erase every scalar spira.toml key the
    // omitted input didn't re-supply (shrink_reason only counts [repo.*] tables and the
    // fayths list, so a scalar wipe like this passes it unnoticed). Fall back to whatever the
    // target already holds for [spira]/[repo] when there is no --conf to derive them from.
    //
    // ONLY WHEN --conf ITSELF IS ABSENT, not merely --repo-map: a caller that passes --conf
    // without --repo-map is doing a real (if incomplete) conversion, and shrink_reason's
    // refusal is the intended backstop for that shape (sp-zs04v.2) — preserving [repo.*] out
    // from under it here would silence the exact refusal that guard exists to raise.
    if conf_path.is_none() {
        if let Some(p) = &out_path {
            if let Ok(existing_text) = fs::read_to_string(p) {
                if let Ok(existing) = validate(&existing_text) {
                    doc.spira = existing.spira;
                    if repo_map_path.is_none() {
                        doc.repo = existing.repo;
                    }
                }
            }
        }
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
            if !force_shrink {
                if let Ok(existing_text) = fs::read_to_string(&p) {
                    if let Ok(existing) = validate(&existing_text) {
                        if let Some(reason) = shrink_reason(&existing, &doc) {
                            eprintln!(
                                "spira-config convert: refuses to shrink {p}: {reason} \
                                 (pass --force-shrink to write it anyway)"
                            );
                            return ExitCode::FAILURE;
                        }
                    }
                }
            }
            if let Err(e) = write_atomic(Path::new(&p), &out) {
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
        Some("set") => match (args.get(1), args.get(2), args.get(3)) {
            (Some(path), Some(value), Some(file)) => cmd_set(path, value, file),
            _ => {
                eprintln!("usage: spira-config set <dotted.path> <value> <file>");
                ExitCode::FAILURE
            }
        },
        Some("unset") => match (args.get(1), args.get(2)) {
            (Some(path), Some(file)) => cmd_unset(path, file),
            _ => {
                eprintln!("usage: spira-config unset <dotted.path> <file>");
                ExitCode::FAILURE
            }
        },
        Some("schema") => cmd_schema(),
        Some("path-tail") => cmd_path_tail(),
        _ => {
            eprintln!(
                "usage: spira-config <validate|get|export|convert|set|unset|schema|path-tail> ...\n\
                 \n\
                 \x20 validate [file]\n\
                 \x20 get <dotted.path> [file]\n\
                 \x20 export --sh [file]\n\
                 \x20 convert --conf F --repo-map F [--fayth F]... [--home DIR] [--out F]\n\
                 \x20         [--force-shrink]\n\
                 \x20 set <dotted.path> <value> <file>\n\
                 \x20 unset <dotted.path> <file>\n\
                 \x20 schema\n\
                 \x20 path-tail"
            );
            ExitCode::FAILURE
        }
    }
}
