//! `rule` — see DESIGN.md. `rule.sh` (root of the harness) is the shim every caller names;
//! it sources `conf.sh` for its side effects and `exec`s into this binary by bare name on
//! the release PATH.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use rule::{
    list_body, parse_enact_args, slugify, word_count, word_limit_refusal, write_succeeded_line,
    CommitOutcome, EnactError, USAGE,
};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut home = ".".to_string();
    let mut rest: Vec<String> = Vec::new();
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        if a == "--home" {
            home = it.next().unwrap_or_else(|| ".".to_string());
        } else {
            rest.push(a);
        }
    }

    let mut args = rest.into_iter();
    let cmd = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();

    // ENACT/RETIRE/LIST/SHOW are the statute book itself and refuse a `$SPIRA_DB` that is
    // not a real store (never fall through to `bd`'s own auto-discovery of some other one).
    // RENDER-MEMORIES/SYSTEM-PROMPT-SPLIT (wave 4.35, sp-kelr2, row Q) are lib.sh's
    // `render_memories`/`system_prompt_split` shim doors: pure string transforms, or a read
    // the `SPIRA_MEMORIES_CMD` test seam bypasses `bd` for entirely, so they must not share
    // this gate — concierge.sh is their one production caller and always has a real `$SPIRA_DB`,
    // but test-render-memories.sh's seam cases deliberately never set one. SPIRA_DB is a
    // registered config key (spira/conf.d) — resolved only for the commands that need it, via
    // the one source of config, never a process-environment read.
    let needs_db = matches!(cmd.as_str(), "enact" | "retire" | "list" | "show");
    let db = if needs_db {
        match spira_config::process::cfg("SPIRA_DB") {
            Ok(v) => v,
            Err(e) => {
                eprintln!("rule: {e}");
                std::process::exit(1);
            }
        }
    } else {
        String::new()
    };
    if needs_db && !Path::new(&format!("{db}/.beads")).is_dir() {
        eprintln!("rule: {db} has no .beads — refusing to guess a database");
        std::process::exit(1);
    }

    let rc = match cmd.as_str() {
        "enact" => cmd_enact(&db, &home, &rest),
        "retire" => cmd_retire(&db, &home, &rest),
        "list" => cmd_list(&db),
        "show" => cmd_show(&db, &rest),
        "render-memories" => cmd_render_memories(&home, &rest),
        "system-prompt-split" => cmd_system_prompt_split(&rest),
        _ => {
            println!("{USAGE}");
            1
        }
    };
    std::process::exit(rc);
}

/// `bd -C <db> <args...>` — never `$SPIRA_BD` (DESIGN.md "Decisions": `rule.sh` didn't
/// either). Returns (exit code, stdout, stderr).
fn run_bd(db: &str, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("bd").arg("-C").arg(db).args(args).output();
    match out {
        Ok(o) => (
            o.status.code().unwrap_or(1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        ),
        Err(e) => (127, String::new(), format!("rule: could not run bd: {e}")),
    }
}

/// `bd memories --json`, distinguishing "the store could not be reached" (bd's own exit
/// status is non-zero) from "the statute book is genuinely empty" ({}), per sp-n93br.
fn memories_json(db: &str) -> Result<BTreeMap<String, serde_json::Value>, String> {
    let (code, stdout, stderr) = run_bd(db, &["memories", "--json"]);
    if code != 0 {
        let mut combined = stdout;
        combined.push_str(&stderr);
        return Err(format!(
            "rule: cannot reach the statute book at {db}: {combined}"
        ));
    }
    Ok(serde_json::from_str(&stdout).unwrap_or_default())
}

/// `SPIRA_MEMORIES_CACHE`, a registered config key (spira/conf.d) — the one source of config,
/// resolved once per process (wave 4.9, sp-k80sa). `rule.sh` used to `export` this across the
/// `exec` boundary because its derived default (`$SPIRA_RUN/memories-cache.json`) is
/// deliberately not in `spira_config::resolve::EXPORT_KEYS` — the same "read in-process,
/// never exported to a child" category `SPIRA_REPO_MAP`/`SPIRA_FAYTHS` carry — so a caller
/// that no longer re-exports it must resolve it itself instead.
fn memories_cache_path() -> Option<String> {
    spira_config::process::cfg("SPIRA_MEMORIES_CACHE").ok().filter(|p| !p.is_empty())
}

fn rm_memories_cache() {
    if let Some(p) = memories_cache_path() {
        let _ = std::fs::remove_file(p);
    }
}

/// The wiki regeneration hook: `$SPIRA_WIKI_HOOK`, else `<home>/law-synth.sh`. Prints its
/// own refusal (to stderr) and returns false when the hook is missing or not executable —
/// SYNTHESIS IS REQUIRED, NOT OPTIONAL (DESIGN.md), exactly as `rule.sh`'s own `synth()`.
const SYNTH_HOOK_NAME: &str = "law-synth.sh";

fn synth(home: &str) -> bool {
    // SPIRA_WIKI_HOOK is a registered config key (spira/conf.d) whose own declared default is
    // the empty string: empty IS the configured meaning "no override, use rule.sh's built-in
    // hook" (see spira/conf.d/SPIRA_WIKI_HOOK) — this is not a crate-local fallback, it is the
    // config's own documented value for "unset".
    let hook = spira_config::process::cfg("SPIRA_WIKI_HOOK")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| std::path::Path::new(home).join(SYNTH_HOOK_NAME).to_string_lossy().into_owned());
    let executable = std::fs::metadata(&hook)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if !executable {
        eprintln!("rule: hook '{hook}' is not executable — statute NOT regenerated in wiki.");
        return false;
    }
    Command::new(&hook)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Commits the `SPIRA_STATUTE_PAGE` page through `<home>/wiki-commit.sh`, immediately after
/// `synth()` regenerates it, named only under `law: <verb> <key>` (sp-4fl2e). Best-effort:
/// no `$SPIRA_WIKI` checkout, or nothing to stage, are not failures.
fn commit_common_law(home: &str, verb: &str, key: &str) -> CommitOutcome {
    // SPIRA_WIKI/SPIRA_STATUTE_PAGE are registered config keys (spira/conf.d) — the one
    // source of config; no second, process-environment read behind it.
    let wiki = match spira_config::process::cfg("SPIRA_WIKI") {
        Ok(w) if !w.is_empty() && Path::new(&w).join(".git").is_dir() => w,
        _ => return CommitOutcome::Skipped,
    };
    let page = match spira_config::process::cfg("SPIRA_STATUTE_PAGE").ok().filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => {
            eprintln!("rule: SPIRA_STATUTE_PAGE is not resolvable from spira_config — wiki page NOT committed.");
            return CommitOutcome::Failed;
        }
    };
    let script = format!("{home}/wiki-commit.sh");
    let mut child = match Command::new("bash").envs(spira_config::release_env::child_path_env_for_process())
        .arg(&script)
        .arg(&wiki)
        .arg(format!("law: {verb} {key}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return CommitOutcome::Failed,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{page}");
    }
    match child.wait() {
        Ok(status) if status.success() => CommitOutcome::Committed,
        _ => CommitOutcome::Failed,
    }
}

/// The two-message tail every `enact`/`retire` call prints after a successful write: the
/// synth+commit outcome, on success, or the "database write succeeded, wiki page did NOT
/// regenerate" refusal (exit 1) when the hook itself failed or was refused.
fn finish_write(
    home: &str,
    verb: &str,
    key: &str,
    live_line: &str,
    write_action: &str,
    write_succeeded_line: &str,
) -> i32 {
    if synth(home) {
        println!();
        println!("{live_line}");
        let outcome = commit_common_law(home, verb, key);
        let msg = outcome.message(verb, key);
        match outcome {
            CommitOutcome::Failed => eprintln!("{msg}"),
            _ => println!("{msg}"),
        }
        0
    } else {
        eprintln!();
        eprintln!("{write_succeeded_line}");
        eprintln!(
            "The wiki page was NOT regenerated. Fix the hook and re-run rule.sh {write_action}."
        );
        1
    }
}

fn cmd_enact(db: &str, home: &str, rest: &[String]) -> i32 {
    if rest.is_empty() {
        println!("{USAGE}");
        return 1;
    }
    let key = slugify(&rest[0]);
    let (dry_run, text) = match parse_enact_args(&rest[1..]) {
        Ok(a) => (a.dry_run, a.text),
        Err(EnactError::Usage) => {
            println!("{USAGE}");
            return 1;
        }
        Err(EnactError::Message(m)) => {
            eprintln!("{m}");
            return 1;
        }
    };

    let words = word_count(&text);
    if words > 130 {
        eprintln!("{}", word_limit_refusal(words));
        return 1;
    }

    let (recall_code, recall_out, _) = run_bd(db, &["recall", &key]);
    if recall_code == 0 {
        println!("rule: '{key}' already exists — this enact overwrites it.");
        println!("--- current ---");
        println!("{recall_out}");
        println!("--- new ---");
        println!("{text}");
        println!("---");
    }

    if dry_run {
        println!("DRY RUN: would enact {key} ({words} words). Nothing written.");
        return 0;
    }

    let (code, _, stderr) = run_bd(db, &["remember", "--key", &key, &text]);
    if code != 0 {
        eprintln!("rule: failed to write {key} to the statute book at {db}: {stderr}");
        return 1;
    }
    rm_memories_cache();
    println!("enacted {key} ({words} words)");

    finish_write(
        home,
        "enact",
        &key,
        "Statute is live in every agent session at its next summon.",
        "enact",
        write_succeeded_line("enact"),
    )
}

fn cmd_retire(db: &str, home: &str, rest: &[String]) -> i32 {
    if rest.len() != 1 {
        println!("{USAGE}");
        return 1;
    }
    let key = slugify(&rest[0]);

    let memories = match memories_json(db) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if !memories.contains_key(&key) {
        eprintln!("rule: no statute '{key}' in the statute book at {db}");
        return 1;
    }

    let (code, _, stderr) = run_bd(db, &["forget", &key]);
    if code != 0 {
        eprintln!("rule: failed to forget {key}: {stderr}");
        return 1;
    }
    println!("forgot {key}");
    rm_memories_cache();

    finish_write(
        home,
        "retire",
        &key,
        "Retired. Do not leave a retired statute standing with a correction attached —\nthat is the same defect as a correction banner on a stale page.",
        "retire",
        write_succeeded_line("retire"),
    )
}

fn cmd_list(db: &str) -> i32 {
    match memories_json(db) {
        Ok(m) => {
            print!("{}", list_body(&m));
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn cmd_show(db: &str, rest: &[String]) -> i32 {
    if rest.len() != 1 {
        println!("{USAGE}");
        return 1;
    }
    let key = slugify(&rest[0]);
    let (code, stdout, stderr) = run_bd(db, &["recall", &key]);
    if code != 0 {
        let msg = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        eprintln!("rule: {msg}");
        eprintln!("rule: `rule.sh list` shows what is in force");
        return 1;
    }
    println!("{stdout}");
    0
}

// =========================================================================================
// render-memories / system-prompt-split (wave 4.35, sp-kelr2, row Q) — lib.sh's
// `render_memories`/`system_prompt_split` shim doors. The tiering/split logic itself is
// `rule::memories`; everything here is the I/O the bash+python did inline: the cache, the
// `SPIRA_MEMORIES_CMD` test seam, and the real `bdq memories --json` read.
// =========================================================================================

fn cmd_render_memories(home: &str, rest: &[String]) -> i32 {
    let prefixes = match rest.first().map(String::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "law-".to_string(),
    };
    // The bash's `int($budget)` crashes uncaught on a non-numeric budget — its traceback is
    // swallowed by the python call's own `2>/dev/null`, so the whole function renders
    // empty. Matched rather than "fixed": nothing production ever passes a non-numeric one.
    let budget: usize = match rest.get(1).map(String::as_str) {
        None | Some("") => 120_000,
        Some(s) => match s.parse() {
            Ok(n) => n,
            Err(_) => return 0,
        },
    };
    // SPIRA_STATUTE_CORE is a registered config key (spira/conf.d) — the one source of
    // config; a resolution failure refuses rather than silently rendering with no core set.
    let core_csv = match rest.get(2).map(String::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => match spira_config::process::cfg("SPIRA_STATUTE_CORE") {
            Ok(v) => v,
            Err(e) => {
                eprintln!("rule: {e}");
                return 1;
            }
        },
    };
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let harness = match spira_config::resolve::resolve_key(&env, std::path::Path::new(home), "SPIRA_REPO") {
        Ok(h) => h,
        Err(e) => {
            eprintln!("rule: {e}");
            return 1;
        }
    };

    let mem_json = match memories_json_cached() {
        Ok(j) => j,
        Err(e) => {
            eprintln!("rule: {e}");
            return 1;
        }
    };
    println!("{}", rule::memories::render(&mem_json, &prefixes, budget, &core_csv, &harness));
    0
}

fn epoch_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64), // t is before the epoch
    }
}

/// The cache-or-fetch read `render_memories` did inline: a fresh cache hit (file exists,
/// `now - mtime < age`) short-circuits the fetch entirely; anything else — missing, stale,
/// or present but empty once read — falls through to [`fetch_memories_json`], and a
/// non-empty result only is written back.
fn memories_json_cached() -> Result<String, String> {
    let cache = memories_cache_path();
    // SPIRA_MEMORIES_CACHE_AGE is a registered config key (spira/conf.d) — the one source of
    // config; no crate-local default on it.
    let age: i64 = spira_config::process::cfg_parse("SPIRA_MEMORIES_CACHE_AGE")?;

    let mut mem_json = String::new();
    if let Some(path) = cache.as_deref() {
        if let Ok(meta) = std::fs::metadata(path) {
            let mtime = meta.modified().map(epoch_secs).unwrap_or(0);
            if epoch_secs(SystemTime::now()) - mtime < age {
                mem_json = std::fs::read_to_string(path).unwrap_or_default().trim_end_matches('\n').to_string();
            }
        }
    }
    if mem_json.is_empty() {
        mem_json = fetch_memories_json();
        if let Some(path) = cache.as_deref() {
            if !mem_json.is_empty() {
                if let Some(parent) = Path::new(path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path, format!("{mem_json}\n"));
            }
        }
    }
    Ok(mem_json)
}

/// `SPIRA_MEMORIES_CMD` stands in for the live read in every test but one — set, it runs
/// verbatim through a shell and `bd`/`bdq` is never touched. Unset, this is `bdjson
/// memories` = `bdq memories --json 2>/dev/null | json_only`.
fn fetch_memories_json() -> String {
    if let Ok(cmd) = std::env::var("SPIRA_MEMORIES_CMD") {
        if !cmd.is_empty() {
            return Command::new("bash").envs(spira_config::release_env::child_path_env_for_process())
                .arg("-c")
                .arg(&cmd)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
                .unwrap_or_default();
        }
    }
    Command::new("bdq")
        .args(["memories", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| json_only(&String::from_utf8_lossy(&o.stdout)).trim_end_matches('\n').to_string())
        .unwrap_or_default()
}

/// `json_only`: keep the first line (and everything after) that starts, at column 1, with
/// `[` or `{` — drops a warning banner `bd --json` printed on stdout first; no match at all
/// is empty. Mirrors `bead::bdq::json_only` (not depended on: one small stdin filter is not
/// worth a crate dependency here).
fn json_only(input: &str) -> &str {
    let mut offset = 0usize;
    for line in input.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        if body.starts_with('[') || body.starts_with('{') {
            return &input[offset..];
        }
        offset += line.len();
    }
    ""
}

fn cmd_system_prompt_split(rest: &[String]) -> i32 {
    if rest.len() != 4 {
        println!("{USAGE}");
        return 1;
    }
    let (sysfile, taskfile, statutes, prompt) = (&rest[0], &rest[1], &rest[2], &rest[3]);
    let (sys, task) = rule::memories::split(statutes, prompt);
    if std::fs::write(sysfile, sys).is_err() {
        eprintln!("rule: could not write {sysfile}");
        return 1;
    }
    if std::fs::write(taskfile, task).is_err() {
        eprintln!("rule: could not write {taskfile}");
        return 1;
    }
    0
}
