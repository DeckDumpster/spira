//! `bdq` — replaces `spira/lib.sh`'s `bdq` function (plus `ghq`/`bdjson`/`json_only`/
//! `json_count`) with a compiled binary every caller already types by that bare name
//! (sp-w3h16, wave 4.14). `spira/lib.sh` keeps one-line shims onto this binary — see
//! `bead/DESIGN.md` "bdq" for the exec-boundary env threading those shims carry, and
//! `bead::bdq` for the pure decision logic this binary drives.
//!
//! Three internal subcommands exist ONLY so the bash shims for `_bdq_check_repo_label`/
//! `_bdq_check_destructive`/`_bdq_check_schema_delete`/`json_only`/`json_count`/`ghq` — each
//! called directly by name in test suites and other scripts, not only from inside `bdq` —
//! have something to exec into. The `__` prefix cannot collide with a real `bd` verb.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bead::bdq::{
    check_destructive, check_repo_label, check_schema_delete, czar_fence_class, is_create, json_count, json_only,
    should_retry,
};
use spira_config::repos::Registry;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let rc = match argv.first().map(String::as_str) {
        Some("__fence") => cmd_fence(&argv[1..]),
        Some("__json_only") => cmd_json_only(),
        Some("__json_count") => cmd_json_count(),
        Some("__ghq") => cmd_ghq(&argv[1..]),
        Some("__latency") => cmd_latency(),
        _ => cmd_bdq(&argv),
    };
    std::process::exit(rc);
}

fn cmd_latency() -> i32 {
    let path = match env_nonempty("SPIRA_RUN") {
        Some(run) => format!("{run}/bdq/latency.log"),
        None => {
            eprintln!("bdq __latency: SPIRA_RUN is unset");
            return 1;
        }
    };
    match std::fs::read_to_string(&path) {
        Ok(log) => {
            print!("{}", bead::latency::render(&bead::latency::rollup(&log)));
            0
        }
        Err(e) => {
            eprintln!("bdq __latency: cannot read {path}: {e}");
            1
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// The repo registry in force for THIS process. The lib.sh `bdq()` shim threads
/// `SPIRA_REPO_MAP`/`SPIRA_HOME`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED`/`SPIRA_HOME_REPO`
/// through explicitly on every call it makes, but a bare invocation (no shim in front of
/// it — a human at a terminal, or a caller that execs this binary directly) has none of
/// them, since conf.sh exports none. `spira_config::repos::Registry::from_env` (sp-k6lku)
/// is the one production door onto a registry: it resolves the four in-process,
/// in-process the same way conf.sh does, whenever the environment it is given lacks them
/// — a key the shim DID thread through still wins, since `from_env` never overwrites a
/// key already present. `bead::bdq::registry_from_env` (the crate's own, lower-level
/// helper) stays reserved for this crate's own deterministic tests, which want an exact,
/// unresolved env map and no filesystem resolution at all.
fn build_registry() -> Registry {
    let env_map: BTreeMap<String, String> = std::env::vars().collect();
    let home = PathBuf::from(env_map.get("SPIRA_HOME").cloned().unwrap_or_default());
    spira_config::repos::Registry::from_env(env_map, &home)
}

// =========================================================================================
// __fence <repo-label|destructive|schema-delete> <bdq-create-argv...>
// =========================================================================================

fn cmd_fence(args: &[String]) -> i32 {
    let (kind, rest) = match args.split_first() {
        Some((k, r)) => (k.as_str(), r.to_vec()),
        None => {
            eprintln!("bdq: __fence requires a check name (repo-label|destructive|schema-delete)");
            return 2;
        }
    };
    match kind {
        "repo-label" => {
            let reg = build_registry();
            match check_repo_label(&rest, &reg) {
                Some(msg) => {
                    eprint!("{msg}");
                    1
                }
                None => 0,
            }
        }
        "destructive" => match env_nonempty("SPIRA_ASK_LABEL") {
            Some(ask_label) => match check_destructive(&rest, &ask_label) {
                Some(msg) => {
                    eprint!("{msg}");
                    1
                }
                None => 0,
            },
            None => {
                eprintln!("bash: SPIRA_ASK_LABEL: SPIRA_ASK_LABEL is unset — source conf.sh");
                1
            }
        },
        "schema-delete" => match check_schema_delete(&rest) {
            Some(msg) => {
                eprint!("{msg}");
                1
            }
            None => 0,
        },
        other => {
            eprintln!("bdq: unknown fence '{other}'");
            2
        }
    }
}

// =========================================================================================
// __json_only / __json_count — generic stdin filters, not bd-specific, but moved with the
// family they were bundled with in lib.sh (wave4-decomposition.md row B).
// =========================================================================================

fn cmd_json_only() -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 1;
    }
    let out = json_only(&input);
    let _ = std::io::stdout().write_all(out.as_bytes());
    0
}

fn cmd_json_count() -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    println!("{}", json_count(&input));
    0
}

// =========================================================================================
// __ghq — `timeout "${GH_TIMEOUT:-120}" "${SPIRA_GH:-gh}" "$@"`.
// =========================================================================================

fn cmd_ghq(args: &[String]) -> i32 {
    let gh_timeout = env_or("GH_TIMEOUT", "120");
    let gh_bin = env_or("SPIRA_GH", "gh");
    Command::new("timeout")
        .arg(&gh_timeout)
        .arg(&gh_bin)
        .args(args)
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or(127)
}

// =========================================================================================
// bdq's own main pass-through.
// =========================================================================================

fn date_now_utc_nanos() -> String {
    Command::new("date")
        .arg("-u")
        .arg("+%Y-%m-%dT%H:%M:%S.%NZ")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default()
}

/// One `timeout <secs> <bd> -C <db> <args>` attempt: stdout inherited straight through
/// (matching bash's unredirected call — a retried attempt can duplicate stdout a prior
/// failed attempt already printed, same latent behaviour the bash original has), stderr
/// captured so the retry loop can inspect it before replaying it once at the end.
fn run_bd_once(timeout_s: &str, bd_bin: &str, db: &str, args: &[String]) -> (i32, String) {
    let mut cmd = Command::new("timeout");
    cmd.arg(timeout_s).arg(bd_bin).arg("-C").arg(db).args(args);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return (127, String::new()),
    };
    let mut buf = Vec::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_end(&mut buf);
    }
    let rc = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(127);
    (rc, String::from_utf8_lossy(&buf).into_owned())
}

fn cmd_bdq(args: &[String]) -> i32 {
    // The three create-time fences — short-circuit on the first refusal, exactly like the
    // bash `_bdq_check_x "$@" || return 1` chain (a later check never runs once an earlier
    // one refuses).
    if is_create(args) {
        let reg = build_registry();
        if let Some(msg) = check_repo_label(args, &reg) {
            eprint!("{msg}");
            return 1;
        }
        let ask_label = match env_nonempty("SPIRA_ASK_LABEL") {
            Some(v) => v,
            None => {
                eprintln!("bash: SPIRA_ASK_LABEL: SPIRA_ASK_LABEL is unset — source conf.sh");
                return 1;
            }
        };
        if let Some(msg) = check_destructive(args, &ask_label) {
            eprint!("{msg}");
            return 1;
        }
        if let Some(msg) = check_schema_delete(args) {
            eprint!("{msg}");
            return 1;
        }
    }

    // The czar fence: a live SPIRA_FAYTH=czar session with a class mutating the queue
    // (reopen always; update/close unless it is the trigger bead itself) must pass
    // czar-fence.sh first. czar-fence.sh's own exit code is NOT propagated — bdq always
    // normalizes a refusal here to 1, matching the bash's bare `|| return 1`.
    let fayth = env_or("SPIRA_FAYTH", "");
    let czar_class = env_or("SPIRA_CZAR_CLASS", "");
    let czar_trigger = env_or("SPIRA_CZAR_TRIGGER_BEAD", "");
    if let Some(class) = czar_fence_class(&fayth, &czar_class, &czar_trigger, args) {
        let ok = Command::new("czar-fence.sh").arg(class).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            return 1;
        }
    }

    // SPIRA_BDJSON_FIXTURE routes every call to bdsim.py instead of the real bd — stdio
    // fully inherited, exit code passed through unchanged.
    if let Some(fixture) = env_nonempty("SPIRA_BDJSON_FIXTURE") {
        return Command::new("bdsim.py")
            .arg(&fixture)
            .args(args)
            .status()
            .map(|s| s.code().unwrap_or(1))
            .unwrap_or(127);
    }

    // Refuse rather than fall through to bd's own auto-discovery (sp-agdzk/sp-25b7s).
    let db = match env_nonempty("SPIRA_DB") {
        Some(d) => d,
        None => {
            eprintln!("bdq: refusing - SPIRA_DB is empty/unset (would fall through to bd auto-discovery)");
            return 1;
        }
    };

    let sop_trace_on = env_or("SOP_APPLIED_TRACE", "") == "1";
    let trace_file: Option<String> = if sop_trace_on {
        Some(env_nonempty("SOP_APPLIED_TRACE_FILE").unwrap_or_else(|| {
            let run = env_nonempty("SPIRA_RUN").unwrap_or_else(|| "/tmp".to_string());
            format!("{run}/sop/trace.log")
        }))
    } else {
        None
    };
    if let Some(tf) = trace_file.as_deref() {
        if let Some(parent) = Path::new(tf).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let t0 = trace_file.as_ref().map(|_| date_now_utc_nanos());

    let bd_bin = env_or("SPIRA_BD", "bd");
    let timeout_s = env_or("BD_TIMEOUT", "180");
    let max_tries: u32 = env_nonempty("SPIRA_BDQ_CONN_RETRIES").and_then(|s| s.parse().ok()).unwrap_or(2);

    let mut try_n: u32 = 1;
    let (rc, stderr_buf) = loop {
        let (rc, err) = run_bd_once(&timeout_s, &bd_bin, &db, args);
        let has_invalid_connection = err.contains("invalid connection");
        if should_retry(rc, try_n, max_tries, has_invalid_connection) {
            try_n += 1;
            continue;
        }
        break (rc, err);
    };
    eprint!("{stderr_buf}");

    if let (Some(tf), Some(t0)) = (trace_file, t0) {
        let t1 = date_now_utc_nanos();
        let line = format!(
            "{} start={} end={} rc={} tries={} argv={}\n",
            std::process::id(),
            t0,
            t1,
            rc,
            try_n,
            args.join(" ")
        );
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&tf) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    rc
}
