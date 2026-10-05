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
//!   spira-config env-bootstrap --sh     the PATH tail + SPIRA_BD resolution conf.sh evals
//!                                       after `resolve --sh-all` (sp-kfimz, "wave 4.6");
//!                                       reads PATH, HOME, SPIRA_PATH, SPIRA_BD from this
//!                                       process's own environment, same as `resolve` reads
//!                                       SPIRA_HOME/SPIRA_REPO from its own
//!   spira-config check-bd               the cached bd-schema preflight (sp-kfimz); reads
//!                                       SPIRA_BD, SPIRA_DB, SPIRA_RUN, SPIRA_DOCTOR,
//!                                       SPIRA_CONF_FILE; exit 1 means the caller must
//!                                       `exit 1` outright, never just `return`
//!   spira-config convert ...            spira.conf + repo-map + *.fayth -> spira.toml
//!   spira-config set <path> <v> <file>  write one value into <file> in place
//!   spira-config unset <path> <file>    remove one value from <file> in place
//!   spira-config writeback <candidate>  the write target any REGENERATING caller resolves
//!                                       through first (wave 4.7, sp-ksrss) — `candidate`
//!                                       unchanged on the installed release or
//!                                       SPIRA_CONFIG_WRITE=1, else redirected under
//!                                       SPIRA_REPO, then XDG_CONFIG_HOME, then a scratch
//!                                       file, at the first of those that is writable;
//!                                       reads SPIRA_CONFIG_WRITE/SPIRA_HOME/SPIRA_PROD/
//!                                       SPIRA_REPO/HOME/XDG_CONFIG_HOME from this
//!                                       process's own environment, same as `resolve`
//!   spira-config schema                 the JSON Schema spira.toml is validated against
//!   spira-config path-tail              the box's `spira.path` tail, or a refusal naming why
//!   spira-config migrate <file>         one-time: a pre-k6m1m goal implies id_prefix (sp-oppza)
//!   spira-config fayth ...              the chamber registry (wave 4.22, sp-r5zd2) — names,
//!                                       get, roster, task, lane, partitions, for-labels, model
//!   spira-config unit <base> [kind]      `spira_unit` (wave 4.10, sp-wqj3o): the unit name
//!                                       this installation has loaded, or '?' when neither
//!                                       the instance-qualified nor the plain form is known
//!                                       to systemd; reads SPIRA_INSTANCE, SPIRA_SYSTEMCTL
//!   spira-config unit --watch <name>    `watch_unit_name`: pure string formatting, no
//!                                       systemctl call; reads SPIRA_INSTANCE (default prod)
//!   spira-config deps list [tier]        `spira_deps_list` (wave 4.10, sp-wqj3o): deps.toml's
//!                                       declared program names, optionally filtered
//!   spira-config deps tier|purpose|absent <bin>
//!                                       `spira_bin_tier`/`_purpose`/`_absent`: one declared
//!                                       field, or its fallback for an undeclared name
//!   spira-config deps require <bin>...  `spira_require`: exit 1 naming each bin not on PATH
//!                                       plus its purpose; `deps list`/`tier`/`purpose`/
//!                                       `absent`/`require` all read SPIRA_HOME/deps.toml
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

use spira_config::chamber;
use spira_config::locate::locate;
use spira_config::repos::{Column, Registry};
use spira_config::resolve::{resolve, ResolveError, ResolveInput, EXPORT_KEYS};
use spira_config::{
    atomic_write_commit, atomic_write_start, backup_existing, convert, discover, export_sh,
    get_path, json_schema, load, migrate_goal_to_id_prefix_in_file, set_path, shrink_reason,
    tail_refusals, unset_path, validate, validate_strict, write_atomic, LocateOutcome, SpiraToml,
};

/// The message `locate`'s CLI surface and `read_input`'s no-file-argument path both print on
/// refusal — one wording, so a diagnostic and an actual read failure never disagree about
/// what was tried.
fn describe_locate_failure(outcome: &LocateOutcome) -> String {
    match outcome {
        LocateOutcome::Found(p) => unreachable!("describe_locate_failure called on Found({p:?})"),
        LocateOutcome::NotFound { tried } if tried.is_empty() => "SPIRA_TOML is not set — it names the one spira.toml".to_string(),
        LocateOutcome::NotFound { tried } => format!(
            "SPIRA_TOML names {}, which is not a file",
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

/// `link-model-bin <release>` — [`spira_config::release_env::link_model_bin`], the function
/// `release build` calls, for a release layout staged anywhere else (sp-jq4wq): testenv's
/// stage script and the suite library run the tree under test's OWN spira-config, so the
/// staged `model-bin/` is that tree's `MODEL_BINS`, never a list baked into the installed
/// testenv. Prints each name linked.
fn cmd_link_model_bin(release: &str) -> ExitCode {
    match spira_config::release_env::link_model_bin(Path::new(release)) {
        Ok(names) => {
            for n in names {
                println!("{n}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spira-config link-model-bin: {e}");
            ExitCode::FAILURE
        }
    }
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

/// `env-bootstrap --sh` (sp-kfimz, "wave 4.6") — `conf.sh`'s own `eval` target, run right
/// after `resolve --sh-all`: the PATH tail ([`spira_config::env_bootstrap::path_tail`]) and
/// `SPIRA_BD` resolution ([`spira_config::env_bootstrap::resolve_bd`]). `PATH`, `HOME` and
/// `SPIRA_PATH` are read from this process's own environment — `conf.sh` passes them
/// explicitly on the call (`PATH="$PATH" HOME="$HOME" SPIRA_PATH="${SPIRA_PATH:-}" ...`)
/// because neither is `export`ed yet at the point this runs; `resolve --sh-all`'s own output
/// is still a plain, un-exported shell assignment until `conf.sh`'s later, explicit export
/// list. `SPIRA_BD` is read the same way, so a value the environment or `spira.toml` already
/// gave `conf.sh` survives untouched (env/toml outrank derivation, same precedence as every
/// other key in this file).
fn cmd_env_bootstrap_sh() -> ExitCode {
    let current_path = env::var("PATH").unwrap_or_default();
    let home = env::var("HOME").unwrap_or_default();
    let spira_path = env::var("SPIRA_PATH").unwrap_or_default();
    let existing_bd = env::var("SPIRA_BD").unwrap_or_default();
    print!(
        "{}",
        spira_config::env_bootstrap::bootstrap_sh(&current_path, &spira_path, &home, &existing_bd)
    );
    ExitCode::SUCCESS
}

/// `check-bd` (sp-kfimz, "wave 4.6") — the cached bd-schema preflight
/// ([`spira_config::env_bootstrap::check_bd_schema`]), as a CLI door for `conf.sh`'s own
/// `spira-config check-bd || exit 1`. `SPIRA_BD`, `SPIRA_DB`, `SPIRA_RUN`, `SPIRA_DOCTOR`
/// and `SPIRA_CONF_FILE` are read from the environment, passed explicitly by `conf.sh` for
/// the same not-yet-exported reason `cmd_env_bootstrap_sh` is. Exit code is the whole
/// contract: 0 means the caller continues (including every tolerated or skipped case — a
/// missing store, a cache hit, `SPIRA_DOCTOR` tolerating a failure it will itself re-check),
/// non-zero means `conf.sh` must `exit 1` outright. Every diagnostic line is printed here,
/// to stderr, so `conf.sh` itself prints nothing further.
fn cmd_check_bd() -> ExitCode {
    let bd = env::var("SPIRA_BD").unwrap_or_else(|_| "bd".to_string());
    let db = env::var("SPIRA_DB").unwrap_or_default();
    let run = env::var("SPIRA_RUN").unwrap_or_default();
    let doctor = env::var("SPIRA_DOCTOR").map(|v| !v.is_empty()).unwrap_or(false);
    let conf_file = env::var("SPIRA_CONF_FILE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "spira.conf".to_string());
    let result = spira_config::env_bootstrap::check_bd_schema(&bd, &db, &run, doctor, &conf_file);
    for m in &result.messages {
        eprintln!("{m}");
    }
    if result.refuse {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `unit <base> [kind]` / `unit --watch <name>` (sp-wqj3o, "wave 4.10") — `conf.sh`'s own
/// `spira_unit`/`watch_unit_name`, now one-line shims onto this. `SPIRA_INSTANCE` and
/// `SPIRA_SYSTEMCTL` are read from the environment, same not-yet-exported reason as every
/// other `cmd_*` function here that reads a per-copy fact `conf.sh` passes explicitly.
fn cmd_unit(args: &[String]) -> ExitCode {
    let instance = env::var("SPIRA_INSTANCE").unwrap_or_default();
    if args.first().map(String::as_str) == Some("--watch") {
        return match args.get(1) {
            Some(name) => {
                println!("{}", spira_config::unit::watch_unit_name(name, &instance));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("usage: spira-config unit --watch <name>");
                ExitCode::FAILURE
            }
        };
    }
    let Some(base) = args.first() else {
        eprintln!("usage: spira-config unit <base> [service|timer]");
        return ExitCode::FAILURE;
    };
    let kind = args.get(1).map(String::as_str).unwrap_or("service");
    let systemctl = env::var("SPIRA_SYSTEMCTL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "systemctl".to_string());
    println!("{}", spira_config::unit::resolve_unit(base, kind, &instance, &systemctl));
    ExitCode::SUCCESS
}

/// `deps.toml`, resolved the same per-copy way every other `cmd_*` function here reads
/// `SPIRA_HOME`: the caller's own fact, passed explicitly rather than assumed ambient.
fn deps_path() -> PathBuf {
    PathBuf::from(env::var("SPIRA_HOME").unwrap_or_default()).join("deps.toml")
}

/// `deps list|tier|purpose|absent|require ...` (sp-wqj3o, "wave 4.10") — `conf.sh`'s own
/// `spira_deps_list`/`spira_bin_tier`/`spira_bin_purpose`/`spira_bin_absent`/`spira_require`,
/// now reading `deps.toml` in-process ([`spira_config::deps`]) instead of a `python3
/// tomllib` subshell `conf.sh` ran unconditionally at every source of itself.
fn cmd_deps(args: &[String]) -> ExitCode {
    let deps = spira_config::deps::load(&deps_path());
    match args.first().map(String::as_str) {
        Some("list") => {
            for name in spira_config::deps::list(&deps, args.get(1).map(String::as_str)) {
                println!("{name}");
            }
            ExitCode::SUCCESS
        }
        Some("tier") => match args.get(1) {
            Some(bin) => {
                println!("{}", spira_config::deps::tier_of(&deps, bin));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("usage: spira-config deps tier <bin>");
                ExitCode::FAILURE
            }
        },
        Some("purpose") => match args.get(1) {
            Some(bin) => {
                println!("{}", spira_config::deps::purpose_of(&deps, bin));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("usage: spira-config deps purpose <bin>");
                ExitCode::FAILURE
            }
        },
        Some("absent") => match args.get(1) {
            Some(bin) => {
                println!("{}", spira_config::deps::absent_of(&deps, bin));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("usage: spira-config deps absent <bin>");
                ExitCode::FAILURE
            }
        },
        Some("require") if args.len() > 1 => {
            let bins: Vec<&str> = args[1..].iter().map(String::as_str).collect();
            let path = env::var("PATH").unwrap_or_default();
            let conf_file = env::var("SPIRA_CONF_FILE")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "spira.conf".to_string());
            match spira_config::deps::require(&deps, &bins, &path, &conf_file) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    eprint!("{msg}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("usage: spira-config deps <list [tier]|tier <bin>|purpose <bin>|absent <bin>|require <bin>...>");
            ExitCode::FAILURE
        }
    }
}

/// The repo registry in force for THIS process (sp-37rmg, "wave 4.11"; `Registry::from_env`
/// since sp-k6lku). `SPIRA_HOME` is read straight out of the environment — the caller's own
/// per-copy fact, same as `cmd_resolve_sh`'s own `SPIRA_HOME`/`SPIRA_REPO` — but
/// `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED` are NOT assumed
/// present: a shim or a bash sourcer that already resolved conf.sh threads them through
/// explicitly and `from_env` leaves them untouched, but this binary is also the one a bare
/// shell or a unit calls directly (`spira-config repo root <name>` typed by hand, or by a
/// caller that never sourced conf.sh at all) — exactly the case conf.sh's own
/// never-exports-them design breaks without an in-process resolve (sp-z3eyk). A
/// `SPIRA_REPO_MAP` that resolves to nothing, or to an unreadable path, is still "no map"
/// (`Registry::map_present() == false`), matching `[ -f "$SPIRA_REPO_MAP" ]`'s own
/// existence-only test.
fn repo_registry() -> Registry {
    let env_map: BTreeMap<String, String> = env::vars().collect();
    let home = PathBuf::from(env_map.get("SPIRA_HOME").cloned().unwrap_or_default());
    Registry::from_env(env_map, &home)
}

fn parse_column(s: &str) -> Option<Column> {
    match s {
        "path" => Some(Column::Path),
        "land" => Some(Column::Land),
        "base" => Some(Column::Base),
        "format" => Some(Column::Format),
        "gate" => Some(Column::Gate),
        "lanes" => Some(Column::Lanes),
        _ => None,
    }
}

/// `spira_containment_check`/`_spira_remote_is_real` (sp-eekjm ported the logic into
/// [`spira_config::containment`]; this is the CLI door onto it lib.sh's own shim calls,
/// sp-37rmg) — `SPIRA_INSTANCE`, `SPIRA_WORKSPACES` and `SPIRA_REPO_MAP` read from the
/// environment exactly as the bash original reads its own globals. Prints every violation,
/// then the same halting line lib.sh printed, and exits 1 — a non-prod instance naming a
/// checkout outside its workspaces root, or with a real remote, must halt the whole process
/// that sourced it, not just this one check.
fn cmd_repo_containment_check() -> ExitCode {
    let instance = env::var("SPIRA_INSTANCE").unwrap_or_default();
    let workspaces = env::var("SPIRA_WORKSPACES").unwrap_or_default();
    let map_text = env::var("SPIRA_REPO_MAP")
        .ok()
        .filter(|p| !p.is_empty())
        .and_then(|p| spira_config::containment::read_repo_map(Path::new(&p)))
        .or_else(|| {
            let doc = spira_config::locate::locate(None).found().and_then(|p| spira_config::load(&p).ok())?;
            Some(
                spira_config::repos::rows_from_toml(&doc)
                    .iter()
                    .map(|r| format!("{} | {} | {} | {} | {} | {} | {}\n", r.name, r.path, r.land, r.base, r.format, r.gate, r.lanes))
                    .collect::<String>(),
            )
        });
    match spira_config::containment::check(&instance, &workspaces, map_text.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(violations) => {
            for v in &violations {
                eprintln!("{v}");
            }
            eprintln!("spira: containment check failed for instance {instance} — halting");
            ExitCode::FAILURE
        }
    }
}

/// `repo <field|names|all|home-repo|root|land|land-queued|gate|format|base|name-at|same|
/// containment-check|landref|ref-remote|ref-branch|qualify-base-ref|landrefs|publish-forge>
/// ...` — the CLI door onto [`spira_config::repos`] for the bash scripts that still call
/// lib.sh's `repo_field`/`repo_root`/`spira_landref`/... by name (now one-line shims onto
/// this), and for any Rust crate that has not yet been switched to call the library
/// in-process (wave4-decomposition.md row 13). `field`/`gate`/`format`/`base` refuse (exit 1,
/// no output) when `SPIRA_REPO_MAP` itself is absent — matching `repo_field`'s own
/// `[ -f "$SPIRA_REPO_MAP" ] || return 1` — while `land`/`land-queued`/`names`/`all`/
/// `home-repo` never refuse, matching their bash originals exactly (see `repos.rs`'s own doc
/// on each). The base-ref verbs (sp-o88bx, "wave 4.12") follow their own lib.sh originals'
/// refusal shapes one-for-one, also documented on their `spira_config::repos` functions.
fn cmd_repo(args: &[String]) -> ExitCode {
    let reg = repo_registry();
    match args.first().map(String::as_str) {
        Some("field") => match (args.get(1), args.get(2).map(String::as_str).and_then(parse_column)) {
            (Some(name), Some(col)) => match reg.field(name, col) {
                Some(v) => {
                    println!("{v}");
                    ExitCode::SUCCESS
                }
                None => ExitCode::FAILURE,
            },
            _ => {
                eprintln!("usage: spira-config repo field <name> <path|land|base|format|gate|lanes>");
                ExitCode::FAILURE
            }
        },
        Some("names") => {
            for n in reg.names() {
                println!("{n}");
            }
            ExitCode::SUCCESS
        }
        Some("all") => {
            for n in reg.all() {
                println!("{n}");
            }
            ExitCode::SUCCESS
        }
        Some("home-repo") => {
            println!("{}", reg.home_repo());
            ExitCode::SUCCESS
        }
        Some("root") => match args.get(1) {
            Some(name) => match reg.root(name) {
                Some(p) => {
                    println!("{p}");
                    ExitCode::SUCCESS
                }
                None => ExitCode::FAILURE,
            },
            None => {
                eprintln!("usage: spira-config repo root <name>");
                ExitCode::FAILURE
            }
        },
        Some("land") => match args.get(1) {
            Some(name) => {
                println!("{}", reg.land(name));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("usage: spira-config repo land <name>");
                ExitCode::FAILURE
            }
        },
        Some("land-queued") => match args.get(1) {
            Some(name) => {
                if reg.land_queued(name) {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            None => {
                eprintln!("usage: spira-config repo land-queued <name>");
                ExitCode::FAILURE
            }
        },
        Some("gate") => match args.get(1) {
            Some(name) => match reg.gate(name) {
                Some(v) => {
                    println!("{v}");
                    ExitCode::SUCCESS
                }
                None => ExitCode::FAILURE,
            },
            None => {
                eprintln!("usage: spira-config repo gate <name>");
                ExitCode::FAILURE
            }
        },
        Some("format") => match args.get(1) {
            Some(name) => match reg.format(name) {
                Some(v) => {
                    println!("{v}");
                    ExitCode::SUCCESS
                }
                None => ExitCode::FAILURE,
            },
            None => {
                eprintln!("usage: spira-config repo format <name>");
                ExitCode::FAILURE
            }
        },
        Some("base") => match args.get(1) {
            Some(name) => match reg.base(name) {
                Some(v) => {
                    println!("{v}");
                    ExitCode::SUCCESS
                }
                None => ExitCode::FAILURE,
            },
            None => {
                eprintln!("usage: spira-config repo base <name>");
                ExitCode::FAILURE
            }
        },
        Some("name-at") => match args.get(1) {
            Some(path) => match reg.name_at(path) {
                Some(n) => {
                    println!("{n}");
                    ExitCode::SUCCESS
                }
                None => ExitCode::FAILURE,
            },
            None => {
                eprintln!("usage: spira-config repo name-at <path>");
                ExitCode::FAILURE
            }
        },
        Some("same") => match (args.get(1), args.get(2)) {
            (Some(a), Some(b)) => {
                if spira_config::repos::same_repo(a, b) {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            _ => {
                eprintln!("usage: spira-config repo same <a> <b>");
                ExitCode::FAILURE
            }
        },
        Some("containment-check") => cmd_repo_containment_check(),
        // Family W (sp-o88bx, "wave 4.12"): spira_landref/ref_remote/ref_branch/
        // qualify_base_ref/spira_landrefs/spira_publish_forge. Unlike the lookups above,
        // `ref-branch` and `qualify-base-ref` never consult the registry at all (they only
        // ever shell to git on the repo they're given), so lib.sh's shims for those two skip
        // `_spira_config_repo`'s env threading entirely — see lib.sh's own comment.
        Some("landref") => {
            let arg = args.get(1).map(String::as_str).unwrap_or("");
            match spira_config::repos::landref(&reg, arg) {
                Some(v) => {
                    println!("{v}");
                    ExitCode::SUCCESS
                }
                None => ExitCode::FAILURE,
            }
        }
        Some("ref-remote") => match args.get(1) {
            // An empty `repo` positional is the shim's "${2:-}" with nothing passed, which
            // means "no repo given" — same convention as `root`'s empty name defaulting to
            // the home repo elsewhere in this file, not a literal empty path to shell `git
            // -C` onto.
            Some(r) => match spira_config::repos::ref_remote(r, args.get(2).map(String::as_str).filter(|s| !s.is_empty())) {
                Some(v) => {
                    println!("{v}");
                    ExitCode::SUCCESS
                }
                None => ExitCode::FAILURE,
            },
            None => {
                eprintln!("usage: spira-config repo ref-remote <ref> [repo]");
                ExitCode::FAILURE
            }
        },
        Some("ref-branch") => match args.get(1) {
            Some(r) => {
                println!("{}", spira_config::repos::ref_branch(r));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("usage: spira-config repo ref-branch <ref>");
                ExitCode::FAILURE
            }
        },
        Some("qualify-base-ref") => match (args.get(1), args.get(2)) {
            (Some(r), Some(repo)) => {
                println!("{}", spira_config::repos::qualify_base_ref(r, repo));
                ExitCode::SUCCESS
            }
            _ => {
                eprintln!("usage: spira-config repo qualify-base-ref <ref> <repo>");
                ExitCode::FAILURE
            }
        },
        Some("landrefs") => match args.get(1) {
            Some(repo) => match spira_config::repos::landrefs(&reg, repo) {
                Some((base, None)) => {
                    println!("{base}");
                    ExitCode::SUCCESS
                }
                Some((base, Some(local))) => {
                    println!("{base} {local}");
                    ExitCode::SUCCESS
                }
                None => ExitCode::FAILURE,
            },
            None => {
                eprintln!("usage: spira-config repo landrefs <repo>");
                ExitCode::FAILURE
            }
        },
        Some("publish-forge") => match args.get(1) {
            Some(name) => {
                let env_map: BTreeMap<String, String> = env::vars().collect();
                match spira_config::repos::publish_forge(&reg, name, &env_map) {
                    Some((remote, branch)) => {
                        println!("{remote} {branch}");
                        ExitCode::SUCCESS
                    }
                    None => ExitCode::FAILURE,
                }
            }
            None => {
                eprintln!("usage: spira-config repo publish-forge <name>");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!(
                "usage: spira-config repo <field|names|all|home-repo|root|land|land-queued|\n\
                 \x20            gate|format|base|name-at|same|containment-check|landref|\n\
                 \x20            ref-remote|ref-branch|qualify-base-ref|landrefs|publish-forge> ..."
            );
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

/// Writes `doc` to `file` for `set`/`unset`: temp file, validate the temp file's own
/// contents, back up whatever `file` currently holds, then rename — in that order, so a
/// crash at any point before the rename leaves `file` exactly as it was, and a doc that
/// fails to round-trip through validation is never renamed into place at all.
fn write_doc(file: &str, doc: &SpiraToml, verb: &str) -> ExitCode {
    let out = match toml::to_string_pretty(doc) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("spira-config {verb}: {file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let path = Path::new(file);
    let pending = match atomic_write_start(path, &out) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("spira-config {verb}: {file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = validate(&out) {
        eprintln!("spira-config {verb}: {file}: refusing to write a document that fails to validate: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = backup_existing(path) {
        eprintln!("spira-config {verb}: {file}: could not back up existing file: {e}");
        return ExitCode::FAILURE;
    }
    match atomic_write_commit(pending) {
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

/// `writeback <candidate>` (conf.sh's `spira_config_writeback`; wave 4.7, sp-ksrss) — reads
/// `SPIRA_CONFIG_WRITE`, `SPIRA_HOME`, `SPIRA_PROD`, `SPIRA_REPO`, `HOME` and
/// `XDG_CONFIG_HOME` from this process's own environment (passed explicitly by `conf.sh`'s
/// one-line shim, same reason `cmd_resolve_sh`/`cmd_env_bootstrap_sh` do — several of these
/// are deliberately unexported per-copy facts) and prints [`spira_config::writeback::writeback`]'s
/// answer. Always succeeds: the whole point of the scratch-file fallback is that this never
/// has nothing to print.
fn cmd_writeback(candidate: &str) -> ExitCode {
    use spira_config::writeback::{writeback, WritebackInput};

    let config_write = env::var("SPIRA_CONFIG_WRITE").map(|v| v == "1").unwrap_or(false);
    let home = PathBuf::from(env::var("SPIRA_HOME").unwrap_or_default());
    let prod = env::var("SPIRA_PROD").ok().filter(|v| !v.is_empty()).map(PathBuf::from);
    let repo = PathBuf::from(env::var("SPIRA_REPO").unwrap_or_default());
    let xdg = env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var("HOME").unwrap_or_default()).join(".config"))
        .join("spira");

    let input = WritebackInput { config_write, home: &home, prod: prod.as_deref(), repo: &repo, xdg_spira_dir: xdg };
    println!("{}", writeback(Path::new(candidate), &input).display());
    ExitCode::SUCCESS
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

/// `$SPIRA_HOME`, or the refusal every `fayth` subcommand prints and fails on — the chamber
/// lives at `<home>/chamber`, and every lib.sh caller this binary now backs already has
/// `SPIRA_HOME` set by the time it calls one of these (conf.sh resolves it before lib.sh is
/// sourced), so an unset value here means something upstream broke, not "use a guess".
fn fayth_home() -> Result<PathBuf, String> {
    match env::var("SPIRA_HOME") {
        Ok(h) if !h.is_empty() => Ok(PathBuf::from(h)),
        _ => Err("SPIRA_HOME is not set".to_string()),
    }
}

/// `fayth ...` — the chamber registry (wave 4.22, sp-r5zd2): `spira-config fayth <verb>`
/// backs lib.sh's own `fayth_names`/`spira_fayths`/`spira_task_fayths`/`spira_lane_fayths`/
/// `fayth_get`/`fayth_partitions`/`fayths_for_labels`/`persona_model`, each now a one-line
/// shim onto one of these verbs.
fn cmd_fayth(args: &[String]) -> ExitCode {
    let home = match fayth_home() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("spira-config fayth: {e}");
            return ExitCode::FAILURE;
        }
    };
    let roster_override = env::var("SPIRA_FAYTHS").ok();
    let roster_override = roster_override.as_deref();
    match args.first().map(String::as_str) {
        Some("names") => {
            for n in chamber::fayth_names(&home) {
                println!("{n}");
            }
            ExitCode::SUCCESS
        }
        Some("get") => match (args.get(1), args.get(2)) {
            (Some(fayth), Some(var)) => {
                let def = args.get(3).map(String::as_str).unwrap_or("");
                print!("{}", chamber::fayth_get(&home, fayth, var, def));
                ExitCode::SUCCESS
            }
            _ => {
                eprintln!("usage: spira-config fayth get <fayth> <var> [default]");
                ExitCode::FAILURE
            }
        },
        Some("roster") => {
            print!("{}", chamber::spira_fayths(&home, roster_override));
            ExitCode::SUCCESS
        }
        Some("task") => {
            print!("{}", chamber::spira_task_fayths(&home, roster_override));
            ExitCode::SUCCESS
        }
        Some("lane") => {
            print!("{}", chamber::spira_lane_fayths(&home, roster_override));
            ExitCode::SUCCESS
        }
        Some("partitions") => {
            for (labels, exclude) in chamber::fayth_partitions(&home, roster_override) {
                println!("{labels}\t{exclude}");
            }
            ExitCode::SUCCESS
        }
        Some("for-labels") => match args.get(1) {
            Some(labels) => {
                for f in chamber::fayths_for_labels(&home, labels) {
                    println!("{f}");
                }
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("usage: spira-config fayth for-labels <labels>");
                ExitCode::FAILURE
            }
        },
        Some("model") => match args.get(1) {
            Some(fayth) => {
                let def = args.get(2).map(String::as_str);
                print!("{}", chamber::persona_model(fayth, def));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("usage: spira-config fayth model <fayth> [default]");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!(
                "usage: spira-config fayth <names|get|roster|task|lane|partitions|for-labels|model> ...\n\
                 \n\
                 \x20 names                           every persona in the chamber, one per line\n\
                 \x20 get <fayth> <var> [default]      one field of a fayth\n\
                 \x20 roster                           spira_fayths: the active roster, in priority order\n\
                 \x20 task                             spira_task_fayths: the pool's summon set\n\
                 \x20 lane                              spira_lane_fayths: the declared lane fayths\n\
                 \x20 partitions                       fayth_partitions: labels\\texclude-labels, one per line\n\
                 \x20 for-labels <labels>              fayths_for_labels: personas whose partition IS <labels>\n\
                 \x20 model <fayth> [default]          persona_model: the model this persona launches under"
            );
            ExitCode::FAILURE
        }
    }
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
        Some("env-bootstrap") => {
            if args.get(1).map(String::as_str) == Some("--sh") {
                cmd_env_bootstrap_sh()
            } else {
                eprintln!("usage: spira-config env-bootstrap --sh");
                ExitCode::FAILURE
            }
        }
        Some("check-bd") => cmd_check_bd(),
        Some("convert") => cmd_convert(&args[1..]),
        Some("repo") => cmd_repo(&args[1..]),
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
        Some("writeback") => match args.get(1) {
            Some(candidate) => cmd_writeback(candidate),
            None => {
                eprintln!("usage: spira-config writeback <candidate>");
                ExitCode::FAILURE
            }
        },
        Some("link-model-bin") => match args.get(1) {
            Some(release) => cmd_link_model_bin(release),
            None => {
                eprintln!("usage: spira-config link-model-bin <release>");
                ExitCode::FAILURE
            }
        },
        Some("schema") => cmd_schema(),
        Some("path-tail") => cmd_path_tail(),
        Some("fayth") => cmd_fayth(&args[1..]),
        Some("unit") => cmd_unit(&args[1..]),
        Some("deps") => cmd_deps(&args[1..]),
        Some("migrate") => match args.get(1) {
            Some(file) => cmd_migrate(file),
            None => {
                eprintln!("usage: spira-config migrate <file>");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!(
                "usage: spira-config <validate|get|export|locate|resolve|env-bootstrap|check-bd|\n\
                 \x20       convert|set|unset|writeback|schema|path-tail|fayth|unit|deps|\n\
                 \x20       migrate|repo> ...\n\
                 \n\
                 \x20 validate [file]\n\
                 \x20 get <dotted.path> [file]\n\
                 \x20 export --sh [file]\n\
                 \x20 locate\n\
                 \x20 resolve --sh|--sh-all [--conf-d DIR] [file]\n\
                 \x20 env-bootstrap --sh\n\
                 \x20 check-bd\n\
                 \x20 convert --conf F --repo-map F [--fayth F]... [--home DIR] [--out F]\n\
                 \x20         [--force-shrink]\n\
                 \x20 set <dotted.path> <value> <file>\n\
                 \x20 unset <dotted.path> <file>\n\
                 \x20 writeback <candidate>\n\
                 \x20 link-model-bin <release>\n\
                 \x20 schema\n\
                 \x20 path-tail\n\
                 \x20 fayth <names|get|roster|task|lane|partitions|for-labels|model> ...\n\
                 \x20 unit <base> [service|timer]\n\
                 \x20 unit --watch <name>\n\
                 \x20 deps <list [tier]|tier <bin>|purpose <bin>|absent <bin>|require <bin>...>\n\
                 \x20 migrate <file>\n\
                 \x20 repo <field|names|all|home-repo|root|land|land-queued|gate|format|base|\n\
                 \x20      name-at|same|containment-check> ..."
            );
            ExitCode::FAILURE
        }
    }
}
