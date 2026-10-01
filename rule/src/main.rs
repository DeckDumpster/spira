//! `rule` — see DESIGN.md. `rule.sh` (root of the harness) is the shim every caller names;
//! it sources `conf.sh` for its side effects and `exec`s into this binary by bare name on
//! the release PATH.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

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

    let db = std::env::var("SPIRA_DB").unwrap_or_default();
    if !Path::new(&format!("{db}/.beads")).is_dir() {
        eprintln!("rule: {db} has no .beads — refusing to guess a database");
        std::process::exit(1);
    }

    let mut args = rest.into_iter();
    let cmd = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();

    let rc = match cmd.as_str() {
        "enact" => cmd_enact(&db, &home, &rest),
        "retire" => cmd_retire(&db, &home, &rest),
        "list" => cmd_list(&db),
        "show" => cmd_show(&db, &rest),
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

/// `SPIRA_MEMORIES_CACHE`, resolved in-process via `spira_config::resolve::resolve_for_process`
/// (wave 4.9, sp-k80sa). `rule.sh` used to `export` this across the `exec` boundary because
/// its derived default (`$SPIRA_RUN/memories-cache.json`) is deliberately not in
/// `spira_config::resolve::EXPORT_KEYS` — the same "read in-process, never exported to a
/// child" category `SPIRA_REPO_MAP`/`SPIRA_FAYTHS` carry — so a caller that no longer
/// re-exports it must resolve it itself instead.
fn memories_cache_path(home: &str) -> Option<String> {
    let home_path = std::path::Path::new(home);
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let repo = spira_config::resolve::derive_home_repo(home_path, &env);
    spira_config::resolve::resolve_for_process(home_path, &repo, &env)
        .ok()
        .and_then(|r| r.values.get("SPIRA_MEMORIES_CACHE").cloned())
        .filter(|p| !p.is_empty())
}

fn rm_memories_cache(home: &str) {
    if let Some(p) = memories_cache_path(home) {
        let _ = std::fs::remove_file(p);
    }
}

/// The wiki regeneration hook: `$SPIRA_WIKI_HOOK`, else `<home>/law-synth.sh`. Prints its
/// own refusal (to stderr) and returns false when the hook is missing or not executable —
/// SYNTHESIS IS REQUIRED, NOT OPTIONAL (DESIGN.md), exactly as `rule.sh`'s own `synth()`.
fn synth(home: &str) -> bool {
    let hook = std::env::var("SPIRA_WIKI_HOOK")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| format!("{home}/law-synth.sh"));
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

/// Commits `wiki/notes/common-law.md` through `<home>/wiki-commit.sh`, immediately after
/// `synth()` regenerates it, named only under `law: <verb> <key>` (sp-4fl2e). Best-effort:
/// no `$SPIRA_WIKI` checkout, or nothing to stage, are not failures.
fn commit_common_law(home: &str, verb: &str, key: &str) -> CommitOutcome {
    let wiki = match std::env::var("SPIRA_WIKI") {
        Ok(w) if !w.is_empty() && Path::new(&w).join(".git").is_dir() => w,
        _ => return CommitOutcome::Skipped,
    };
    let script = format!("{home}/wiki-commit.sh");
    let mut child = match Command::new("bash")
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
        let _ = stdin.write_all(b"wiki/notes/common-law.md\n");
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
    rm_memories_cache(home);
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
    rm_memories_cache(home);

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
