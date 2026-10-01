//! `spira-config` — validate, read, export and convert `spira.toml`.
//!
//!   spira-config validate [file]        exit 1 and name the TOML path on the first error;
//!                                       also refuses a [spira] with no id_prefix (sp-k6m1m)
//!   spira-config get <dotted.path>      one value read out of the document
//!   spira-config export --sh            `[spira]` as quoted KEY=value lines
//!   spira-config locate                 the spira.toml path in force, or a refusal naming why
//!   spira-config resolve --sh [file]    env > toml > derived defaults, as KEY='value' lines
//!                                       for the typed export set (sp-eekjm; SPIRA_HOME and
//!                                       SPIRA_REPO come from this process's own environment)
//!   spira-config resolve --sh-all [file]  the same resolution, but EVERY resolved key —
//!                                       conf.sh's own `eval` target (sp-ubcgo), which needs
//!                                       SPIRA_REPO_MAP/SPIRA_FAYTHS/SPIRA_MAX_AEONS and every
//!                                       other registry key too, not only the typed export set
//!   ... --conf-d DIR                    read the registry from DIR instead of
//!                                       SPIRA_HOME/conf.d — conf.sh's own fix for its
//!                                       symlink fence, where SPIRA_HOME resolves to a test
//!                                       fixture but conf.d/ stays beside conf.sh's real file
//!   spira-config convert ...            spira.conf + repo-map + *.fayth -> spira.toml
//!   spira-config set <path> <v> <file>  write one value into <file> in place
//!   spira-config unset <path> <file>    remove one value from <file> in place
//!   spira-config schema                 the JSON Schema spira.toml is validated against
//!   spira-config path-tail              the box's `spira.path` tail, or a refusal naming why
//!   spira-config migrate <file>         one-time: a pre-k6m1m goal implies id_prefix (sp-oppza)
//!
//! `validate`, `get` and `export` read `spira.toml` from: the file argument if given (`-` for
//! stdin, named on purpose), else the same search `locate` reports — `$SPIRA_TOML` (exclusive:
//! a pinned path that is not a file means no config, not "keep looking"), else
//! `${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml`, else `/etc/spira/spira.toml` (no
//! `$SPIRA_REPO` tier — sp-9hwim removed it from `conf.sh`'s matching bash search; see
//! `DESIGN-locate.md`). With no file argument and nothing resolvable, these refuse by name
//! (`NotFound`/`LegacyOnly`) instead of the prior fallback guesses — a `./spira.toml` that
//! happened to be in the current directory, or blocking forever on stdin — fail-closed, sp-hconl.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use spira_config::locate::locate;
use spira_config::resolve::{resolve, ResolveError, ResolveInput, EXPORT_KEYS};
use spira_config::{
    convert, discover, export_sh, get_path, json_schema, load,
    migrate_goal_to_id_prefix_in_file, set_path, shrink_reason, tail_refusals, unset_path,
    validate, validate_strict, write_atomic, LocateOutcome, SpiraToml,
};

/// The message `locate`'s CLI surface and `read_input`'s no-file-argument path both print on
/// refusal — one wording, so a diagnostic and an actual read failure never disagree about
/// what was tried.
fn describe_locate_failure(outcome: &LocateOutcome) -> String {
    match outcome {
        LocateOutcome::Found(p) => unreachable!("describe_locate_failure called on Found({p:?})"),
        LocateOutcome::NotFound { tried } => format!(
            "no spira.toml found; tried: {}",
            tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        ),
        LocateOutcome::LegacyOnly { conf, tried } => format!(
            "{} exists but no spira.toml — run `spira-config convert` first; tried: {}",
            conf.display(),
            tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn cmd_locate() -> ExitCode {
    match locate(None) {
        LocateOutcome::Found(p) => {
            println!("{}", p.display());
            ExitCode::SUCCESS
        }
        outcome @ LocateOutcome::NotFound { .. } => {
            eprintln!("spira-config locate: {}", describe_locate_failure(&outcome));
            ExitCode::from(1)
        }
        outcome @ LocateOutcome::LegacyOnly { .. } => {
            eprintln!("spira-config locate: {}", describe_locate_failure(&outcome));
            ExitCode::from(2)
        }
    }
}

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
        // FAIL CLOSED (sp-hconl): no file argument means "resolve it the same way conf.sh
        // does", never "guess the current directory" or "block on stdin" — the two things
        // this branch did before. An unresolvable config names every path it tried instead
        // of guessing.
        None => match locate(None) {
            LocateOutcome::Found(p) => {
                fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))
            }
            outcome => Err(describe_locate_failure(&outcome)),
        },
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

/// `migrate <file>` — sp-oppza's one-time upgrade migration, run by the installer
/// (pre-activate, doctor) before `validate`: a box whose config predates sp-k6m1m (goal set,
/// no id_prefix) is repaired in place instead of failing activation. Idempotent: a file
/// already migrated, or one with nothing to migrate, is untouched and prints nothing.
fn cmd_migrate(file: &str) -> ExitCode {
    match migrate_goal_to_id_prefix_in_file(Path::new(file)) {
        Ok(Some(msg)) => {
            println!("spira-config migrate: {file}: {msg}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spira-config migrate: {e}");
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
/// release or a checkout — the one place this check runs. This CLI form remains for bash
/// callers; the `install` crate (sp-31dm0, which retired `render.py`, `install.sh` and
/// `unit-ensure.sh`) calls [`tail_refusals`] and [`discover`]/[`load`] directly as a library
/// instead of spawning this binary.
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

/// `resolve --sh [file]` / `resolve --sh-all [file]` — `spira_config::resolve()`'s result as
/// `KEY='value'` lines (sp-eekjm, "wave 4.4: spira-config resolve"; the `--sh-all` form is
/// sp-ubcgo, "wave 4.5: conf.sh becomes an eval of resolve"). `SPIRA_HOME`/`SPIRA_REPO` come
/// from THIS process's own environment, never self-located — the one caller that will
/// matter, `conf.sh`'s own `eval "$(spira-config resolve --sh-all)"`, has already derived
/// both by the time it gets here, exactly as it derives them today. `file`, if given, pins
/// the `spira.toml` to read the same way `validate`/`get`/`export` already do; with no
/// argument, it is located the same way `spira-config locate` reports (no
/// `$SPIRA_TOML`/`$SPIRA_CONF` pin here means "no config file", not a search failure).
///
/// `all`: false prints only [`EXPORT_KEYS`] (the set any caller may safely re-export to a
/// child process); true prints every resolved key, `SPIRA_REPO_MAP`/`SPIRA_FAYTHS`/
/// `SPIRA_MAX_AEONS` included — `conf.sh` is sourced, not exec'd, so reading those in-process
/// here is the same thing bash's own `spira_conf_defaults` always did; `conf.sh` decides
/// separately, in bash, which names to then `export` to ITS children.
///
/// `conf_d_override`: the registry directory to read, when the caller's own `SPIRA_HOME`
/// is NOT where `conf.d/` actually lives. `conf.sh`'s symlink fence is exactly this case —
/// dozens of suites `ln -s ".../conf.sh" "$FIXTURE/spira/conf.sh"` so `SPIRA_HOME` resolves
/// to the fixture, deliberately, while `conf.d/` and `conf-gen.sh` stay beside conf.sh's
/// REAL file (see conf.sh's own `_spira_conf_gen_ensure`, "RESOLVES THE SYMLINK, unlike
/// SPIRA_HOME's own derivation"). `home.join("conf.d")` alone would look for a registry
/// inside the fixture, find nothing, and `resolve` would silently compute only its ~30
/// hand-ported keys — every one of the ~270 registry-backed keys gone without an error
/// (sp-ubcgo: caught by test-conf-toml.sh's T1 before this override existed). `None` keeps
/// the plain `home.join("conf.d")` default, for callers whose `SPIRA_HOME` is not a symlink
/// target at all (every Rust crate that calls `resolve()` as a library already passes its
/// own `conf_d`, so this override is a CLI-only concern).
fn cmd_resolve_sh(all: bool, file: Option<&str>, conf_d_override: Option<&str>) -> ExitCode {
    let home = match env::var("SPIRA_HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => {
            eprintln!("spira-config resolve: SPIRA_HOME is not set — this must be derived by the caller (conf.sh), never self-located");
            return ExitCode::FAILURE;
        }
    };
    let repo = match env::var("SPIRA_REPO") {
        Ok(r) if !r.is_empty() => PathBuf::from(r),
        _ => {
            eprintln!("spira-config resolve: SPIRA_REPO is not set — this must be derived by the caller (conf.sh), never self-located");
            return ExitCode::FAILURE;
        }
    };

    let toml_path = match file {
        Some(f) => Some(PathBuf::from(f)),
        None => locate(None).found(),
    };
    let doc = match toml_path {
        Some(p) => match fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display())).and_then(|t| validate(&t)) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("spira-config resolve: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    let conf_d = conf_d_override.map(PathBuf::from).unwrap_or_else(|| home.join("conf.d"));
    let env_map: BTreeMap<String, String> = env::vars().collect();
    match resolve(ResolveInput { env: &env_map, home: &home, repo: &repo, toml: doc.as_ref(), conf_d: &conf_d }) {
        Ok(resolved) => {
            for w in &resolved.warnings {
                eprintln!("{w}");
            }
            if all {
                print!("{}", resolved.to_sh_all());
            } else {
                print!("{}", resolved.to_sh(EXPORT_KEYS));
            }
            ExitCode::SUCCESS
        }
        Err(ResolveError::Containment(violations)) => {
            for v in &violations {
                eprintln!("{v}");
            }
            eprintln!(
                "spira: containment check failed for instance {} — halting",
                env::var("SPIRA_INSTANCE").unwrap_or_default()
            );
            ExitCode::FAILURE
        }
        Err(ResolveError::Registry(e)) => {
            eprintln!("spira-config resolve: {e}");
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
        Some("locate") => cmd_locate(),
        Some("resolve") => {
            let all = match args.get(1).map(String::as_str) {
                Some("--sh") => false,
                Some("--sh-all") => true,
                _ => {
                    eprintln!("usage: spira-config resolve --sh|--sh-all [--conf-d DIR] [file]");
                    return ExitCode::FAILURE;
                }
            };
            let mut conf_d: Option<String> = None;
            let mut file: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--conf-d" => {
                        conf_d = args.get(i + 1).cloned();
                        if conf_d.is_none() {
                            eprintln!("usage: spira-config resolve --sh|--sh-all [--conf-d DIR] [file]");
                            return ExitCode::FAILURE;
                        }
                        i += 2;
                    }
                    other => {
                        file = Some(other.to_string());
                        i += 1;
                    }
                }
            }
            cmd_resolve_sh(all, file.as_deref(), conf_d.as_deref())
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
        Some("migrate") => match args.get(1) {
            Some(file) => cmd_migrate(file),
            None => {
                eprintln!("usage: spira-config migrate <file>");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!(
                "usage: spira-config <validate|get|export|locate|resolve|convert|set|unset|schema|path-tail|migrate> ...\n\
                 \n\
                 \x20 validate [file]\n\
                 \x20 get <dotted.path> [file]\n\
                 \x20 export --sh [file]\n\
                 \x20 locate\n\
                 \x20 resolve --sh|--sh-all [--conf-d DIR] [file]\n\
                 \x20 convert --conf F --repo-map F [--fayth F]... [--home DIR] [--out F]\n\
                 \x20         [--force-shrink]\n\
                 \x20 set <dotted.path> <value> <file>\n\
                 \x20 unset <dotted.path> <file>\n\
                 \x20 schema\n\
                 \x20 path-tail\n\
                 \x20 migrate <file>"
            );
            ExitCode::FAILURE
        }
    }
}
