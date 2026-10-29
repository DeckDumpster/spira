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
    code,
    audit::{audit, finding_title}, find_duplicate, is_in_force, list_body, parse_enact_args, slugify, word_count, word_limit_refusal,
    CommitOutcome, EnactError, ToolDecl, USAGE,
};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut home: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        if a == "--home" {
            home = it.next();
        } else {
            rest.push(a);
        }
    }

    let home = home.unwrap_or_else(|| {
        spira_config::resolve::locate_home_for_process()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|e| {
                eprintln!("rule: {e} — pass --home");
                std::process::exit(1)
            })
    });

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
    spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "spira-rule", &cmd);
    let needs_db = matches!(cmd.as_str(), "enact" | "retire" | "audit");
    let reads_book = matches!(cmd.as_str(), "list" | "show") || (cmd == "code" && !rest.iter().any(|a| a == "--texts"));
    let db = if needs_db {
        match spira_config::process::cfg("SPIRA_DB") {
            Ok(v) => v,
            Err(e) => {
                eprintln!("rule: {e}");
                spira_otel::oneshot::exit(1);
            }
        }
    } else if reads_book {
        spira_config::process::cfg("SPIRA_DB").unwrap_or_default()
    } else {
        String::new()
    };
    if needs_db && !Path::new(&format!("{db}/.beads")).is_dir() {
        eprintln!("rule: {db} has no .beads — refusing to guess a database");
        spira_otel::oneshot::exit(1);
    }

    let rc = match cmd.as_str() {
        "enact" => cmd_enact(&db, &home, &rest),
        "retire" => cmd_retire(&db, &home, &rest),
        "list" => cmd_list(&db),
        "show" => cmd_show(&db, &home, &rest),
        "audit" => cmd_audit(&db, &home, &rest),
        "code" => cmd_code(&db, &rest),
        "render-memories" => cmd_render_memories(&home, &rest),
        "system-prompt-split" => cmd_system_prompt_split(&rest),
        _ => {
            println!("{USAGE}");
            1
        }
    };
    if rc == 0 && matches!(cmd.as_str(), "enact" | "retire") {
        spira_otel::oneshot::event("spira_statutes_changed", 1, &[("change", if cmd == "enact" { "enacted" } else { "retired" })]);
    }
    spira_otel::oneshot::exit(rc);
}

/// `bd -C <db> <args...>` — never `$SPIRA_BD` (DESIGN.md "Decisions": `rule.sh` didn't
/// either). Returns (exit code, stdout, stderr).
fn run_bd(db: &str, args: &[&str]) -> (i32, String, String) {
    let out = spira_config::bounded::bounded("bd").arg("-C").arg(db).args(args).output();
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

fn statute_dir_from(dir: &str, run: &str) -> Option<std::path::PathBuf> {
    match (dir.is_empty(), run.is_empty()) {
        (false, _) => Some(dir.into()),
        (true, false) => Some(Path::new(run).join("statutes")),
        (true, true) => None,
    }
}

fn statute_dir() -> Result<std::path::PathBuf, String> {
    let dir = spira_config::process::cfg("SPIRA_STATUTE_DIR")?;
    let run = spira_config::process::cfg("SPIRA_RUN").unwrap_or_default();
    statute_dir_from(&dir, &run).ok_or_else(|| "SPIRA_STATUTE_DIR and SPIRA_RUN are both empty in spira_config".to_string())
}

/// Reads of the book come from the on-disk mirror. Only a book never mirrored falls back to
/// the store, and that read then fills the mirror so the next one does not need it.
fn read_book(db: &str) -> Result<BTreeMap<String, serde_json::Value>, String> {
    let dir = statute_dir()?;
    if let Some(book) = rule::mirror::read(&dir) {
        return Ok(book);
    }
    if !Path::new(&format!("{db}/.beads")).is_dir() {
        return Err(format!("rule: no statute mirror in {} and {db} has no .beads — refusing to guess a database", dir.display()));
    }
    let book = memories_json(db)?;
    let _ = rule::mirror::sync(&dir, &book);
    Ok(book)
}

/// Mirrors `book`; a failure is the caller's to roll back, because a statute in the store
/// that readers cannot see is not in force.
fn mirror_book(book: &BTreeMap<String, serde_json::Value>) -> Result<(), String> {
    rule::mirror::sync(&statute_dir()?, book).map_err(|e| format!("rule: statute mirror not written: {e}"))
}

/// `SPIRA_MEMORIES_CACHE`, a registered config key (spira/conf.d) — the one source of config,
/// resolved once per process (wave 4.9, sp-k80sa). `rule.sh` used to `export` this across the
/// `exec` boundary because its derived default (`$SPIRA_RUN/memories-cache.json`) is
/// deliberately not in `spira_config::resolve::EXPORT_KEYS` — the same "read in-process,
/// never exported to a child" category `SPIRA_REPO_MAP`/`SPIRA_RUNES` carry — so a caller
/// that no longer re-exports it must resolve it itself instead.
fn memories_cache_path() -> Option<String> {
    spira_config::process::cfg("SPIRA_MEMORIES_CACHE").ok().filter(|p| !p.is_empty())
}

fn rm_memories_cache() {
    if let Some(p) = memories_cache_path() {
        let _ = std::fs::remove_file(p);
    }
}

const SYNTH_HOOK_NAME: &str = "law-synth.sh";

/// The wiki regeneration hook: `$SPIRA_WIKI_HOOK`, else `<home>/law-synth.sh`. Refuses a hook
/// that is relative (it would resolve against the caller's cwd) or not an executable file.
/// Checked BEFORE any write so a statute is never in force without its page.
fn resolve_hook(home: &str) -> Result<String, String> {
    // Empty SPIRA_WIKI_HOOK is the registered meaning "no override" (spira/conf.d).
    let hook = spira_config::process::cfg("SPIRA_WIKI_HOOK")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| Path::new(home).join(SYNTH_HOOK_NAME).to_string_lossy().into_owned());
    if !Path::new(&hook).is_absolute() {
        return Err(format!("hook '{hook}' is not an absolute path"));
    }
    let executable = std::fs::metadata(&hook)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if !executable {
        return Err(format!("hook '{hook}' is not executable"));
    }
    Ok(hook)
}

fn refuse_hook(e: &str) -> i32 {
    eprintln!("rule: {e} — wiki page NOT regenerated; nothing written, the statute book is unchanged.");
    1
}

fn synth(hook: &str) -> bool {
    // batch-job: this runs whatever its caller names, as long as that takes
    Command::new(hook).status().map(|s| s.success()).unwrap_or(false)
}

/// Puts the book back as it was before a write whose page could not be regenerated:
/// the prior text, or no key at all.
fn rollback(db: &str, key: &str, prior: Option<&str>, before: &BTreeMap<String, serde_json::Value>) -> bool {
    let (code, _, _) = match prior {
        Some(text) => run_bd(db, &["remember", "--key", key, text]),
        None => run_bd(db, &["forget", key]),
    };
    rm_memories_cache();
    code == 0 && mirror_book(before).is_ok()
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
    // batch-job: runs a gate, build or forge script that takes as long as its work
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

/// The tail of every `enact`/`retire`: regenerate the page and commit it. A failed
/// regeneration rolls the book back to `prior`, so the write and the page succeed or fail as one.
#[allow(clippy::too_many_arguments)]
fn finish_write(
    db: &str,
    home: &str,
    hook: &str,
    verb: &str,
    key: &str,
    before: &BTreeMap<String, serde_json::Value>,
    after: &BTreeMap<String, serde_json::Value>,
    live_line: &str,
) -> i32 {
    let prior = before.get(key).and_then(|v| v.as_str());
    if let Err(e) = mirror_book(after) {
        eprintln!("{e}");
        if rollback(db, key, prior, before) {
            eprintln!("rule: {verb} of {key} rolled back, nothing written.");
        } else {
            eprintln!("rule: the rollback of {key} failed too — run `rule.sh {verb} {key}` again once the mirror is writable.");
        }
        return 1;
    }
    if synth(hook) {
        println!();
        println!("{live_line}");
        let outcome = commit_common_law(home, verb, key);
        let msg = outcome.message(verb, key);
        match outcome {
            CommitOutcome::Failed => eprintln!("{msg}"),
            _ => println!("{msg}"),
        }
        0
    } else if rollback(db, key, prior, before) {
        eprintln!("rule: hook '{hook}' failed — wiki page NOT regenerated; {verb} of {key} rolled back, nothing written.");
        1
    } else {
        eprintln!("rule: hook '{hook}' failed AND the rollback of {key} failed.");
        eprintln!("The book and the wiki page now disagree. Re-run rule.sh {verb} {key} once the hook works.");
        1
    }
}

fn cmd_enact(db: &str, home: &str, rest: &[String]) -> i32 {
    if rest.is_empty() {
        println!("{USAGE}");
        return 1;
    }
    let key = slugify(&rest[0]);
    let (dry_run, text, tool, part) = match parse_enact_args(&rest[1..]) {
        Ok(a) => (a.dry_run, a.text, a.tool, a.part),
        Err(EnactError::Usage) => {
            println!("{USAGE}");
            return 1;
        }
        Err(EnactError::Message(m)) => {
            eprintln!("{m}");
            return 1;
        }
    };

    let repo = match locate_repo(home) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let toc = match read_toc_at(&repo) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let Some(part) = part else {
        eprintln!("rule: refusing — a statute is enacted into a Part of the code. Pass --part <n>; the Parts are:");
        eprint!("{}", code::parts_list(&toc));
        return 1;
    };
    let toc_after = match code::append(&toc, part, &key) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("rule: refusing — {e}. The Parts are:");
            eprint!("{}", code::parts_list(&toc));
            return 1;
        }
    };

    let words = word_count(&text);
    if words > 130 {
        eprintln!("{}", word_limit_refusal(words));
        return 1;
    }

    if let Some(m) = rule::vocabulary_refusal(&key, &text) {
        eprintln!("{m}");
        spira_otel::oneshot::event("spira_statute_refused", 1, &[("reason", "retired-vocabulary")]);
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

    let hook = match resolve_hook(home) {
        Ok(h) => h,
        Err(e) => return refuse_hook(&e),
    };

    if dry_run {
        println!("DRY RUN: would enact {key} ({words} words). Nothing written.");
        return 0;
    }

    let before = match memories_json(db) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if let Some(existing) = find_duplicate(&before, &key, &text) {
        eprintln!("rule: this text is already in force as {existing} — refused, nothing written. Retire or amend {existing} instead.");
        return 1;
    }
    let (code, _, stderr) = run_bd(db, &["remember", "--key", &key, &text]);
    if code != 0 {
        let landed = memories_json(db).map(|m| is_in_force(&m, &key, &text));
        match landed {
            Ok(false) => {
                eprintln!("rule: failed to write {key} to the statute book at {db}: {stderr}");
                eprintln!("rule: verified against the book — nothing written, {key} is not in force.");
                return 1;
            }
            Ok(true) => {
                eprintln!("rule: bd reported an error but {key} IS in force (verified against the book): {stderr}");
            }
            Err(e) => {
                eprintln!("rule: bd reported an error writing {key} and the book cannot be read back to say whether it landed: {stderr}");
                eprintln!("{e}");
                eprintln!("rule: run `rule.sh show {key}` before retrying; do not enact it again under another slug.");
                return 1;
            }
        }
    }
    rm_memories_cache();
    println!("enacted {key} ({words} words)");
    println!("{}", tool_line(&tool));

    let mut after = before.clone();
    after.insert(key.clone(), serde_json::Value::String(text.clone()));
    let rc = finish_write(db, home, &hook, "enact", &key, &before, &after, "Statute is live in every agent session at its next call.");
    if rc != 0 {
        return rc;
    }
    let pointers = match read_pointers_for_write(&repo) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            eprintln!("rule: {key} IS in force but law/ was not submitted — fix the cause and run `rule.sh code` from a worktree.");
            return 1;
        }
    };
    match land_law(home, &repo, "enact", &key, &toc_after, &pointers, &strings(&after)) {
        Ok((branch, bead)) => {
            let n = toc_after.iter().find(|p| p.number == part).and_then(|p| p.slugs.iter().position(|s| *s == key)).map_or(0, |i| i + 1);
            println!("law/: {key} is §{part}.{n}; committed on {branch}, submitted under {bead}.");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            eprintln!("rule: {key} IS in force but law/ was not submitted — fix the cause and run `rule.sh code` from a worktree.");
            1
        }
    }
}

fn land_law(home: &str, repo: &(String, Option<String>), verb: &str, key: &str, toc: &[code::Part], pointers: &code::Pointers, book: &BTreeMap<String, String>) -> Result<(String, String), String> {
    let wt_root = spira_config::process::cfg("SPIRA_RUN").map(|r| Path::new(&r).join("worktree"))?;
    let reg = spira_config::repos::Registry::from_env_checkout(std::env::vars().collect(), Path::new(home));
    let (id, branch) = code::land_law(&code::LawRepo { path: Path::new(&repo.0), base: repo.1.as_deref(), name: reg.home_repo() }, &wt_root, verb, key, toc, pointers, book)?;
    Ok((branch, id))
}

fn law_dir() -> Result<std::path::PathBuf, String> {
    let out = spira_config::bounded::bounded("git").args(["rev-parse", "--show-toplevel"]).output().map_err(|e| format!("rule: git: {e}"))?;
    if !out.status.success() {
        return Err("rule: not inside a git checkout — law/ is a tracked tree, run this from your worktree".to_string());
    }
    Ok(Path::new(String::from_utf8_lossy(&out.stdout).trim()).join("law"))
}

/// The home repository's checkout and the ref its `law/` is cut from — found through the
/// repo map, never through the caller's working directory.
fn locate_repo(home: &str) -> Result<(String, Option<String>), String> {
    let reg = spira_config::repos::Registry::from_env_checkout(std::env::vars().collect(), Path::new(home));
    let root = reg.root("").ok_or("rule: the repo map has no path for the home repository — refusing")?;
    if !Path::new(&root).join(".git").exists() {
        return Err(format!("rule: {root} is not a git checkout — refusing"));
    }
    let base = spira_config::repos::landref(&reg, &root);
    Ok((root, base))
}

fn read_toc_at(repo: &(String, Option<String>)) -> Result<Vec<code::Part>, String> {
    let (root, base) = repo;
    let text = match base {
        Some(b) => {
            let out = spira_config::bounded::bounded("git").arg("-C").arg(root).arg("show").arg(format!("{b}:law/toc.toml")).output().map_err(|e| format!("rule: git: {e}"))?;
            if !out.status.success() {
                return Err(format!("rule: {b}:law/toc.toml: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            String::from_utf8_lossy(&out.stdout).into_owned()
        }
        None => std::fs::read_to_string(Path::new(root).join("law/toc.toml")).map_err(|e| format!("rule: {root}/law/toc.toml: {e}"))?,
    };
    code::parse_toc(&text).map(|t| code::with_in_flight(Path::new(root), base.as_deref(), t)).map_err(|e| format!("rule: {e}"))
}

fn today() -> String {
    spira_config::bounded::bounded("date")
        .arg("+%Y-%m-%d")
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or_else(String::new, |o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn read_pointers(law: &Path) -> Result<code::Pointers, String> {
    match std::fs::read_to_string(law.join(code::POINTERS_FILE)) {
        Ok(t) => code::parse_pointers(&t).map_err(|e| format!("rule: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(code::Pointers::default()),
        Err(e) => Err(format!("rule: {}: {e}", law.join(code::POINTERS_FILE).display())),
    }
}

/// `law/<file>` at the land ref of the home repository (its checkout when there is none);
/// `None` when the ref has no such file.
fn read_law_file_at(repo: &(String, Option<String>), file: &str) -> Result<Option<String>, String> {
    let (root, base) = repo;
    match base {
        Some(b) => {
            let out = spira_config::bounded::bounded("git").arg("-C").arg(root).arg("show").arg(format!("{b}:law/{file}")).output().map_err(|e| format!("rule: git: {e}"))?;
            Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
        }
        None => Ok(std::fs::read_to_string(Path::new(root).join("law").join(file)).ok()),
    }
}

fn read_pointers_at(repo: &(String, Option<String>)) -> Result<code::Pointers, String> {
    match read_law_file_at(repo, code::POINTERS_FILE)? {
        Some(t) => code::parse_pointers(&t).map_err(|e| format!("rule: {e}")),
        None => Ok(code::Pointers::default()),
    }
}

fn read_pointers_for_write(repo: &(String, Option<String>)) -> Result<code::Pointers, String> {
    read_pointers_at(repo).map(|p| code::with_in_flight_pointers(Path::new(&repo.0), repo.1.as_deref(), p))
}

fn read_toc(law: &Path) -> Result<Vec<code::Part>, String> {
    let text = std::fs::read_to_string(law.join("toc.toml")).map_err(|e| format!("rule: {}: {e}", law.join("toc.toml").display()))?;
    code::parse_toc(&text).map_err(|e| format!("rule: {e}"))
}

fn strings(book: &BTreeMap<String, serde_json::Value>) -> BTreeMap<String, String> {
    book.iter().filter_map(|(k, v)| v.as_str().map(|t| (k.clone(), t.to_string()))).collect()
}

fn book_texts(book: &BTreeMap<String, serde_json::Value>) -> impl Fn(&str) -> Option<String> + '_ {
    move |slug| book.get(slug).and_then(|v| v.as_str()).map(str::to_string)
}


fn cmd_code(db: &str, rest: &[String]) -> i32 {
    let (mut check, mut texts) = (false, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--check" => check = true,
            "--texts" => texts = it.next().cloned(),
            _ => {
                println!("{USAGE}");
                return 1;
            }
        }
    }
    let law = match law_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let toc = match read_toc(&law) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let book = match &texts {
        Some(dir) => {
            let mut m = BTreeMap::new();
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if let (Some(stem), Ok(t)) = (p.file_stem().and_then(|s| s.to_str()), std::fs::read_to_string(&p)) {
                        m.insert(stem.to_string(), serde_json::Value::String(t));
                    }
                }
            }
            m
        }
        None => match read_book(db) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        },
    };
    let on_disk = read_pointers(&law);
    let pointers = match on_disk.as_ref().map_err(String::clone).and_then(|p| code::record_moves(&code::numbers_on_disk(&law), &toc, p, &today())) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", e.trim_start_matches("rule: "));
            return 1;
        }
    };
    let old = on_disk.unwrap_or_default();
    let new_moves = &pointers.moved[old.moved.len()..];
    if check {
        let mut drift = code::drift(&law, &code::generate(&toc, &pointers, book_texts(&book)));
        drift.extend(code::check_pointers(&toc, &pointers));
        drift.extend(new_moves.iter().map(|m| format!("{}: §{} moved to {}, not yet recorded", code::POINTERS_FILE, m.section, code::section_of(&toc, &m.slug).unwrap_or_default())));
        for d in &drift {
            eprintln!("law/{d}");
        }
        return if drift.is_empty() { 0 } else { 1 };
    }
    let bad = code::check_pointers(&toc, &pointers);
    if !bad.is_empty() {
        for b in &bad {
            eprintln!("law/{b}");
        }
        return 1;
    }
    for m in new_moves {
        println!("§{} moved: {} is now §{}", m.section, m.slug, code::section_of(&toc, &m.slug).unwrap_or_default());
    }
    match code::write_law(&law, &toc, &pointers, &strings(&book)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn tool_line(tool: &ToolDecl) -> String {
    match tool {
        ToolDecl::Tool(t) => format!("complied with by: {t}"),
        ToolDecl::Mechanism(b) => format!("compliance mechanism being built in {b}"),
        ToolDecl::None(r) => format!("constrains judgement only: {r}"),
    }
}

fn release_lookup(home: &str) -> impl Fn(&str) -> Option<String> {
    let root = Path::new(home).parent().map(Path::to_path_buf).unwrap_or_default();
    let dirs = [
        Path::new(home).to_path_buf(),
        root.clone(),
        root.join("bin"),
        root.join("model-bin"),
    ];
    move |name: &str| {
        dirs.iter()
            .map(|d| d.join(name))
            .find(|p| p.is_file())
            .map(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned())
    }
}

fn open_bead_titles(db: &str) -> Vec<String> {
    let (code, out, _) = run_bd(db, &["list", "--json", "--limit", "0"]);
    if code != 0 {
        return Vec::new();
    }
    serde_json::from_str::<Vec<serde_json::Value>>(&out)
        .unwrap_or_default()
        .iter()
        .filter_map(|b| b.get("title").and_then(|t| t.as_str()).map(str::to_string))
        .collect()
}

fn cmd_audit(db: &str, home: &str, rest: &[String]) -> i32 {
    let (mut file, mut persona, mut repo, mut max) = (false, None, None, 5usize);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--file" => file = true,
            "--for" => persona = it.next().cloned(),
            "--repo" => repo = it.next().cloned(),
            "--max" => match it.next().and_then(|v| v.parse().ok()) {
                Some(n) => max = n,
                None => {
                    eprintln!("rule: audit: --max needs a number");
                    return 1;
                }
            },
            other => {
                eprintln!("rule: audit does not take '{other}'");
                return 1;
            }
        }
    }
    if file && repo.is_none() {
        repo = spira_config::process::cfg("SPIRA_HOME_REPO")
            .ok()
            .and_then(|p| Path::new(&p).file_name().map(|n| n.to_string_lossy().into_owned()));
    }
    if file && (persona.is_none() || repo.is_none()) {
        eprintln!("rule: audit --file needs --for <persona>, and --repo <name> where SPIRA_HOME_REPO does not name the harness repo");
        return 1;
    }
    let memories = match memories_json(db) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let findings = audit(&memories, &release_lookup(home));
    for f in &findings {
        println!("{}: {}", f.slug, f.missing.join(", "));
    }
    println!("{} of {} statutes name something the release does not have", findings.len(), memories.keys().filter(|k| k.starts_with("law-")).count());
    if !file {
        return 0;
    }
    let open = open_bead_titles(db);
    let mut filed = 0;
    for f in &findings {
        if open.iter().any(|t| t.contains(&f.slug)) {
            continue;
        }
        if filed == max {
            println!("rule: audit filed {max}, the batch bound; the rest wait for the next run.");
            break;
        }
        let title = finding_title(f);
        let body = format!(
            "{}\n\nThe statute names the above and the activated release has no such tool or verb. Build it, or amend the statute to name what exists (rule.sh enact with --tool).\n",
            title
        );
        let bead_sh = Path::new(home).join("bead.sh");
        let child = Command::new(&bead_sh) // batch-job: bead.sh file writes the store under its lock
            .args(["file", &title, "--for", persona.as_deref().unwrap_or(""), "--repo", repo.as_deref().unwrap_or(""), "--body-file", "-"])
            .stdin(Stdio::piped())
            .spawn();
        let Ok(mut child) = child else {
            eprintln!("rule: audit could not run {}", bead_sh.display());
            return 1;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body.as_bytes());
        }
        match child.wait() {
            Ok(s) if s.success() => filed += 1,
            _ => eprintln!("rule: audit: filing for {} failed", f.slug),
        }
    }
    0
}

struct RetireArgs {
    key: String,
    reason: String,
    replaced_by: Option<String>,
    merged_into: Option<String>,
}

fn parse_retire_args(rest: &[String]) -> Result<RetireArgs, String> {
    let (mut key, mut reason, mut replaced_by, mut merged_into) = (None, None, None, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| it.next().cloned().ok_or_else(|| format!("rule: {flag} needs a value"));
        match a.as_str() {
            "--reason" => reason = Some(value("--reason")?),
            "--replaced-by" => replaced_by = Some(slugify(&value("--replaced-by")?)),
            "--merged-into" => merged_into = Some(slugify(&value("--merged-into")?)),
            f if f.starts_with("--") => return Err(format!("rule: unknown flag {f}")),
            _ if key.is_none() => key = Some(slugify(a)),
            _ => return Err("rule: retire takes one slug".to_string()),
        }
    }
    let key = key.ok_or("rule: retire needs a slug")?;
    let reason = reason.filter(|r| !r.trim().is_empty()).ok_or("rule: refusing — a retirement leaves a pointer, and a pointer needs its reason. Pass --reason <why>.")?;
    if replaced_by.is_some() && merged_into.is_some() {
        return Err("rule: pass --replaced-by or --merged-into, not both".to_string());
    }
    Ok(RetireArgs { key, reason, replaced_by, merged_into })
}

fn cmd_retire(db: &str, home: &str, rest: &[String]) -> i32 {
    if rest.is_empty() {
        println!("{USAGE}");
        return 1;
    }
    let RetireArgs { key, reason, replaced_by, merged_into } = match parse_retire_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            eprintln!("usage: rule.sh retire <slug> --reason <why> [--replaced-by <slug> | --merged-into <slug>]");
            return 1;
        }
    };

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
    for successor in replaced_by.iter().chain(&merged_into) {
        if *successor == key || !memories.contains_key(successor) {
            eprintln!("rule: refusing — {successor} is not a statute in force, so it cannot stand in for {key}");
            return 1;
        }
    }

    let hook = match resolve_hook(home) {
        Ok(h) => h,
        Err(e) => return refuse_hook(&e),
    };
    let repo = match locate_repo(home) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let (toc, mut pointers) = match read_toc_at(&repo).and_then(|t| read_pointers_for_write(&repo).map(|p| (t, p))) {
        Ok(tp) => tp,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if code::section_of(&toc, &key).is_none() {
        eprintln!("rule: refusing — {key} holds no section in law/toc.toml, so no pointer can be left for it. Enact it into a Part first.");
        return 1;
    }
    let on = today();
    match merged_into {
        Some(into) => pointers.merged.push(code::Merged { from: key.clone(), into, reason, on }),
        None => pointers.retired.push(code::Retired { slug: key.clone(), reason, replaced_by, on }),
    }
    if let Err(e) = code::resolve(&toc, &pointers, &key) {
        eprintln!("rule: refusing — {e}");
        return 1;
    }

    let (code, _, stderr) = run_bd(db, &["forget", &key]);
    if code != 0 {
        eprintln!("rule: failed to forget {key}: {stderr}");
        return 1;
    }
    println!("forgot {key}");
    rm_memories_cache();

    let mut after = memories.clone();
    after.remove(&key);
    let rc = finish_write(
        db,
        home,
        &hook,
        "retire",
        &key,
        &memories,
        &after,
        "Retired. Do not leave a retired statute standing with a correction attached —\nthat is the same defect as a correction banner on a stale page.",
    );
    if rc != 0 {
        return rc;
    }
    match land_law(home, &repo, "retire", &key, &toc, &pointers, &strings(&after)) {
        Ok((branch, bead)) => {
            println!("law/: {key} keeps its number, marked retired; committed on {branch}, submitted under {bead}.");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            eprintln!("rule: {key} is retired but law/ was not submitted — run `rule.sh code`.");
            1
        }
    }
}

fn cmd_list(db: &str) -> i32 {
    match read_book(db) {
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

fn cmd_show(db: &str, home: &str, rest: &[String]) -> i32 {
    if rest.len() != 1 {
        println!("{USAGE}");
        return 1;
    }
    let key = slugify(&rest[0]);
    let book = match read_book(db) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let numbered = rest[0].strip_prefix('§').unwrap_or(&rest[0]);
    let is_number = code::is_section(numbered);
    if !is_number {
        if let Some(text) = book.get(&key).and_then(|v| v.as_str()) {
            println!("{}", text.trim_end_matches('\n'));
            return 0;
        }
    }
    let arg = if is_number { numbered } else { key.as_str() };
    let looked = locate_repo(home).and_then(|repo| {
        let toc = code::parse_toc(&read_law_file_at(&repo, "toc.toml")?.unwrap_or_default()).map_err(|e| format!("rule: {e}"))?;
        Ok((toc, read_pointers_at(&repo)?))
    });
    let resolved = looked.map(|(toc, pointers)| (code::resolve(&toc, &pointers, arg), toc));
    match resolved {
        Ok((Ok(r), toc)) => {
            for h in &r.hops {
                println!("{h}");
            }
            match (&r.retired, book.get(&r.slug).and_then(|v| v.as_str())) {
                (Some(ret), _) => println!("{}", code::retired_body(ret, &toc)),
                (None, Some(text)) => println!("{}", text.trim_end_matches('\n')),
                (None, None) => {
                    eprintln!("rule: {} (§{}) is in the code but not in force", r.slug, r.section);
                    return 1;
                }
            }
            0
        }
        Ok((Err(code::Miss::Cycle(c)), _)) => {
            eprintln!("rule: pointers form a cycle: {c}");
            1
        }
        Ok((Err(code::Miss::Unknown), _)) | Err(_) => {
            eprintln!("rule: no statute '{}'", if is_number { numbered } else { &key });
            eprintln!("rule: `rule.sh list` shows what is in force");
            1
        }
    }
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
    // SPIRA_REPO is this copy's checkout — an identity fact, never a config key (resolve()
    // deliberately never returns it): the same derivation every crate uses for it.
    let harness = spira_config::resolve::derive_home_repo(std::path::Path::new(home), &env).to_string_lossy().into_owned();

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
    let seam = std::env::var("SPIRA_MEMORIES_CMD").is_ok_and(|c| !c.is_empty());
    if !seam {
        if let Some(book) = statute_dir().ok().and_then(|d| rule::mirror::read(&d)) {
            return serde_json::to_string(&book).map_err(|e| e.to_string());
        }
    }
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
        if !seam {
            if let (Ok(serde_json::Value::Object(o)), Ok(dir)) = (serde_json::from_str(&mem_json), statute_dir()) {
                let _ = rule::mirror::sync(&dir, &o.into_iter().collect());
            }
        }
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
            // batch-job: runs a gate, build or forge script that takes as long as its work
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
    spira_config::bounded::bounded("bdq")
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

#[cfg(test)]
mod statute_dir_tests {
    use super::*;

    #[test]
    fn an_empty_key_defaults_under_run() {
        assert_eq!(statute_dir_from("", "/r"), Some("/r/statutes".into()));
        assert_eq!(statute_dir_from("/s", "/r"), Some("/s".into()));
        assert_eq!(statute_dir_from("", ""), None);
    }

    #[test]
    fn the_delta_rendered_toml_resolves() {
        let delta = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../spira/config-delta.toml")).unwrap();
        assert!(delta.contains("\"spira.statute_dir\" = \"\""));
        let tmp = testkit::TempDir::new("rule-sd");
        let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_STATUTE_DIR", ""), ("SPIRA_RUN", "/fx/run")]);
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
        assert_eq!(statute_dir().unwrap(), std::path::PathBuf::from("/fx/run/statutes"));
    }
}
