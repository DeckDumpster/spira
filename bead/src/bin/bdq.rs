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

use bead::claimdesc;
use bead::bdq::{
    check_destructive, check_repo_label, check_schema_delete, creates_closed, czar_fence_class, is_create, json_count, json_only,
    should_retry, retryable, backoff_ms,
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
    let path = match spira_config::process::cfg("SPIRA_RUN") {
        Ok(run) if !run.is_empty() => format!("{run}/bdq/latency.log"),
        Ok(_) => {
            eprintln!("bdq __latency: SPIRA_RUN resolved empty");
            return 1;
        }
        Err(e) => {
            eprintln!("bdq __latency: {e}");
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
        "destructive" => match spira_config::process::cfg("SPIRA_ASK_LABEL") {
            Ok(ask_label) => match check_destructive(&rest, &ask_label) {
                Some(msg) => {
                    eprint!("{msg}");
                    1
                }
                None => 0,
            },
            Err(e) => {
                eprintln!("bdq: {e}");
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
    // GH_TIMEOUT is not a registered config key (spira/conf.d has no entry) — left as a
    // plain env read with its existing default.
    let gh_timeout = env_or("GH_TIMEOUT", "120");
    // SPIRA_GH's own registered default is empty — empty is this key's documented sentinel
    // for "the system gh" (spira/conf.d/SPIRA_GH), not a missing-value fallback, so mapping
    // it to "gh" here is the key's own semantics, not a second default on top of cfg's.
    let gh_bin = match spira_config::process::cfg("SPIRA_GH") {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => "gh".to_string(),
        Err(e) => {
            eprintln!("bdq: {e}");
            return 1;
        }
    };
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
    spira_config::bounded::bounded("date")
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
fn run_bd_once(timeout_s: &str, bd_bin: &str, db: &str, args: &[String], capture_stdout: bool) -> (i32, String, Vec<u8>) {
    let mut cmd = Command::new("timeout");
    cmd.arg(timeout_s).arg(bd_bin).arg("-C").arg(db).args(args);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(if capture_stdout { Stdio::piped() } else { Stdio::inherit() });
    cmd.stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return (127, String::new(), Vec::new()),
    };
    let mut out_buf = Vec::new();
    let reader = child.stdout.take().map(|mut o| std::thread::spawn(move || {
        let _ = o.read_to_end(&mut out_buf);
        out_buf
    }));
    let mut buf = Vec::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_end(&mut buf);
    }
    let rc = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(127);
    let out_buf = reader.and_then(|h| h.join().ok()).unwrap_or_default();
    (rc, String::from_utf8_lossy(&buf).into_owned(), out_buf)
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
        let ask_label = match spira_config::process::cfg("SPIRA_ASK_LABEL") {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bdq: {e}");
                return 1;
            }
        };
        if let Some(msg) = check_destructive(args, &ask_label) {
            eprint!("{msg}");
            return 1;
        }
        if let Some(msg) = bead::bdq::check_ask_shape(args, &ask_label) {
            eprint!("{msg}");
            return 1;
        }
        if let Some(msg) = check_schema_delete(args) {
            eprint!("{msg}");
            return 1;
        }
    }

    if matches!(args.first().map(String::as_str), Some("label" | "update")) {
        match spira_config::process::cfg("SPIRA_ASK_LABEL") {
            Ok(ask) => {
                if let Some(msg) = bead::bdq::check_ask_label_write(args, &ask) {
                    eprint!("{msg}");
                    return 1;
                }
            }
            Err(e) => {
                eprintln!("bdq: {e}");
                return 1;
            }
        }
    }

    // A bead's state is the lifecycle row's (sp-6oimlm): no caller moves bd's status through
    // bdq. The override is named and logged, never silent.
    if let Some(door) = bead::bdq::check_state_verb(args) {
        match env_nonempty("SPIRA_BDQ_STATE_WRITE") {
            Some(why) => eprintln!("bdq: STATE WRITE OVERRIDE ({}) — bd's status moved outside the lifecycle machine: {why}", args.join(" ")),
            None => {
                eprintln!(
                    "spira: bdq refuses `{}` — a bead's state is its lifecycle row's, and bd's status follows it, never the reverse.\n\
                     Exit: {door}. To write bd's status anyway, set SPIRA_BDQ_STATE_WRITE=<reason> (logged).",
                    args.first().map(String::as_str).unwrap_or("")
                );
                return 1;
            }
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
    // SPIRA_DB is registered but deliberately carries no conf.d default (an explicitly
    // empty SPIRA_DB means "no database for this run" and must survive, per spira/conf.d/
    // SPIRA_DB's own doc) — so a resolved-but-empty value is this refusal, not a
    // resolution failure.
    let db = match spira_config::process::cfg("SPIRA_DB") {
        Ok(d) if !d.is_empty() => d,
        Ok(_) => {
            eprintln!("bdq: refusing - SPIRA_DB is empty/unset (would fall through to bd auto-discovery)");
            return 1;
        }
        Err(e) => {
            eprintln!("bdq: {e}");
            return 1;
        }
    };

    // SOP_APPLIED_TRACE / SOP_APPLIED_TRACE_FILE are not SPIRA_*/COCKPIT_* names at all —
    // not registered config keys, left as plain env reads.
    let sop_trace_on = env_or("SOP_APPLIED_TRACE", "") == "1";
    let trace_file: Option<String> = if sop_trace_on {
        match env_nonempty("SOP_APPLIED_TRACE_FILE") {
            Some(tf) => Some(tf),
            // No default (per Ryan 2026-10-05: one source of config, decided): an
            // unresolvable SPIRA_RUN just means this opt-in debug trace does not get
            // written this call, never a guessed "/tmp" location.
            None => spira_config::process::cfg("SPIRA_RUN")
                .ok()
                .filter(|v| !v.is_empty())
                .map(|run| format!("{run}/sop/trace.log")),
        }
    } else {
        None
    };
    if let Some(tf) = trace_file.as_deref() {
        if let Some(parent) = Path::new(tf).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let t0 = trace_file.as_ref().map(|_| date_now_utc_nanos());

    // SPIRA_BD is registered but carries no conf.d default ("resolves empty unless set via
    // environment or the config file") — the real config file always sets it explicitly, so an
    // empty resolution here is treated the same as a resolution failure, not a literal
    // "bd" fallback.
    let bd_bin = match spira_config::process::cfg("SPIRA_BD") {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => {
            eprintln!("bdq: SPIRA_BD resolved empty — refusing rather than guessing a bd binary");
            return 1;
        }
        Err(e) => {
            eprintln!("bdq: {e}");
            return 1;
        }
    };
    // BD_TIMEOUT is not a registered config key (spira/conf.d has no entry) — left as a
    // plain env read with its existing default.
    let timeout_s = env_or("BD_TIMEOUT", "180");
    let mut call_args: Vec<String> = args.to_vec();
    let mut forced: Option<(String, claimdesc::LiveClaim, String)> = None;
    if args.first().map(String::as_str) == Some("update") {
        let u = claimdesc::parse_update_args(&args[1..]);
        call_args = std::iter::once("update".to_string()).chain(u.passthrough.iter().cloned()).collect();
        if let (true, Some(id)) = (u.touches_description, u.id.as_ref()) {
            // The claim is the lifecycle row's (sp-mve9i). A machine that cannot answer refuses
            // nothing, as a failed bd show refused nothing before it.
            let row = spira_config::lc_state::row(id).unwrap_or_else(|e| {
                eprintln!("bdq: lifecycle state unreadable for {id} ({e}); not checking for a live claim");
                None
            });
            if let Some(claim) = claimdesc::live_claim(row.as_ref(), now_epoch()) {
                match u.force {
                    None => {
                        eprint!("{}", claimdesc::refusal(id, &claim));
                        return 1;
                    }
                    Some(reason) => forced = Some((id.clone(), claim, reason)),
                }
            }
        }
    }
    let args = &call_args[..];
    let max_tries: u32 = env_nonempty("SPIRA_BDQ_CONN_RETRIES").and_then(|s| s.parse().ok()).unwrap_or(3);
    let backoff_base: u64 = env_nonempty("SPIRA_BDQ_CONN_BACKOFF_MS").and_then(|s| s.parse().ok()).unwrap_or(1000);
    let t_start = std::time::Instant::now();

    let lc_row = is_create(args) && !creates_closed(args);
    let mut try_n: u32 = 1;
    let (rc, stderr_buf, created_out) = loop {
        let (rc, err, out) = run_bd_once(&timeout_s, &bd_bin, &db, args, lc_row);
        if should_retry(rc, try_n, max_tries, retryable(args, &err)) {
            eprintln!("bdq: invalid connection (attempt {try_n}/{max_tries}); retrying");
            std::thread::sleep(std::time::Duration::from_millis(backoff_ms(backoff_base, try_n)));
            try_n += 1;
            continue;
        }
        break (rc, err, out);
    };
    eprint!("{stderr_buf}");
    if lc_row {
        let _ = std::io::stdout().write_all(&created_out);
        let _ = std::io::stdout().flush();
        if rc == 0 {
            if let Err(e) = spira_config::lifecycle_row::after_create("bdq", &String::from_utf8_lossy(&created_out)) {
                eprintln!("bdq: LIFECYCLE: row not written after create: {e}; the new bead is rowless and cannot be claimed");
            }
        }
    }
    if rc == 0 {
        let _ = spira_config::lifecycle_row::after_close("bdq", args);
    }
    if let Some(run) = spira_config::process::cfg("SPIRA_RUN").ok().filter(|v| !v.is_empty()) {
        let dir = format!("{run}/bdq");
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{dir}/latency.log")) {
            let _ = writeln!(f, "{} rc={} tries={} ms={} verb={}", date_now_utc_nanos(), rc, try_n, t_start.elapsed().as_millis(), args.iter().find(|a| !a.starts_with('-')).map(String::as_str).unwrap_or(""));
        }
    }

    if let (0, Some((id, claim, reason))) = (rc, forced) {
        acknowledge_forced_edit(&timeout_s, &bd_bin, &db, &id, &claim, &reason);
    }

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

fn now_epoch() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn util_json_only(raw: &str) -> String {
    json_only(raw).to_string()
}

/// stdout of one bd call, stderr dropped; empty on any failure.
fn bd_capture(timeout_s: &str, bd_bin: &str, db: &str, args: &[&str]) -> String {
    Command::new("timeout")
        .arg(timeout_s)
        .arg(bd_bin)
        .arg("-C")
        .arg(db)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// An overridden edit is its own acknowledgment: re-stamp the baseline so the close-time check
/// does not reopen a close nobody did anything wrong on, file the note, and tell the holder.
/// These are direct bd calls, not nested bdq calls — a metadata write is not a description edit.
fn acknowledge_forced_edit(timeout_s: &str, bd_bin: &str, db: &str, id: &str, claim: &claimdesc::LiveClaim, reason: &str) {
    let actor = env_nonempty("BEADS_ACTOR")
        .or_else(|| {
            Command::new("git")
                .args(["config", "user.name"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| env_nonempty("USER"))
        .unwrap_or_else(|| "unknown".to_string());
    if let Some(hash) = claimdesc::desc_hash(&util_json_only(&bd_capture(timeout_s, bd_bin, db, &["show", id, "--json"]))) {
        bd_capture(timeout_s, bd_bin, db, &["update", id, "--set-metadata", &format!("{}={hash}", claimdesc::HASH_KEY)]);
    }
    bd_capture(timeout_s, bd_bin, db, &["note", id, &claimdesc::override_note(&actor, claim, reason)]);
    // Best-effort notification: by this point `cfg` has already resolved successfully at
    // least once in this process (SPIRA_DB/SPIRA_BD above), so these two cannot newly
    // fail; `unwrap_or_default` only ever matters if SPIRA_RUN/SPIRA_MAIL themselves
    // resolve empty, which `notify_live_aeon` already treats as "nothing to notify".
    let run = spira_config::process::cfg("SPIRA_RUN").unwrap_or_default();
    let mail = spira_config::process::cfg("SPIRA_MAIL").unwrap_or_default();
    claimdesc::notify_live_aeon(id, &claimdesc::holder_message(&actor, reason), &run, &mail);
}
