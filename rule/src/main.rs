//! `rule` — see DESIGN.md. `rule.sh` (root of the harness) is the shim every caller names;
//! it sources `conf.sh` for its side effects and `exec`s into this binary by bare name on
//! the release PATH.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
use std::collections::BTreeMap;
use std::io::Write;
use std::ops::ControlFlow;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use rule::{
    book::{read_mirrored, Book, Memories, Renamed, Stop},
    code,
    audit::{audit, finding_title}, list_body, parse_enact_args, slugify, word_count, word_limit_refusal,
    hook::synth, CommitOutcome, EnactError, ToolDecl, USAGE,
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

    spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "spira-rule", &cmd);

    let rc = match cmd.as_str() {
        "enact" => with_book(|book| cmd_enact(book, &home, &rest)),
        "retire" => with_book(|book| cmd_retire(book, &home, &rest)),
        "rename-vocabulary" => with_book(|book| cmd_rename_vocabulary(book, &home, &rest)),
        "list" => cmd_list(),
        "show" => cmd_show(&rest),
        "audit" => with_book(|book| cmd_audit(book, &home, &rest)),
        "code" if rest.len() == 1 && rest[0] == "--land" => cmd_land_book(&home),
        "code" => cmd_code(&rest),
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

fn configured_book() -> Result<Book, String> {
    Book::configured().map_err(|e| format!("rule: {e}"))
}

fn with_book(run: impl FnOnce(&Book) -> i32) -> i32 {
    match configured_book() {
        Ok(book) => run(&book),
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
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

/// Reads of the book come from the on-disk mirror, so a book already mirrored needs no store.
fn read_book() -> Result<Memories, String> {
    read_mirrored(&statute_dir()?, configured_book)
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
    rule::hook::check(&hook)
}

fn refuse_hook(e: &str) -> i32 {
    eprintln!("rule: {e} — wiki page NOT regenerated; nothing written, the statute book is unchanged.");
    1
}

/// Commits and pushes `paths` (relative to `wiki`) through `<home>/wiki-commit.sh` under `msg`.
fn wiki_commit(home: &str, wiki: &str, msg: &str, paths: &[String]) -> Result<(), String> {
    let script = format!("{home}/wiki-commit.sh");
    let finish = format!("run: printf '%s\\n' <paths> | bash {script} {wiki} '{msg}'");
    let failed = |why: String| format!("{}; {finish}", why.trim());
    let envs = spira_config::release_env::child_path_env_for_process().map_err(|e| failed(e.to_string()))?;
    // batch-job: runs a gate, build or forge script that takes as long as its work
    let mut child = Command::new("bash")
        .envs(envs)
        .arg(&script)
        .arg(wiki)
        .arg(msg)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| failed(format!("{script}: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        for p in paths {
            let _ = writeln!(stdin, "{p}");
        }
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(failed(format!("{} ({})", String::from_utf8_lossy(&out.stderr).trim(), out.status))),
        Err(e) => Err(failed(format!("{script}: {e}"))),
    }
}

/// Commits the `SPIRA_STATUTE_PAGE` page immediately after `synth()` regenerates it, named only
/// under `law: <verb> <key>`. Best-effort: no `$SPIRA_WIKI` checkout is not a failure.
fn commit_common_law(home: &str, verb: &str, key: &str) -> CommitOutcome {
    let wiki = match spira_config::process::cfg("SPIRA_WIKI") {
        Ok(w) if !w.is_empty() && Path::new(&w).join(".git").is_dir() => w,
        _ => return CommitOutcome::Skipped,
    };
    let Some(page) = spira_config::process::cfg("SPIRA_STATUTE_PAGE").ok().filter(|p| !p.is_empty()) else {
        return CommitOutcome::Failed("SPIRA_STATUTE_PAGE is not resolvable from spira_config".to_string());
    };
    match wiki_commit(home, &wiki, &format!("law: {verb} {key}"), &[page]) {
        Ok(()) => CommitOutcome::Committed,
        Err(e) => CommitOutcome::Failed(e),
    }
}

/// The tail of every `enact`/`retire`: regenerate the page and commit it. A failed
/// regeneration rolls the book back to `prior`, so the write and the page succeed or fail as one.
#[allow(clippy::too_many_arguments)]
fn finish_write(
    book: &Book,
    home: &str,
    hook: &str,
    verb: &str,
    key: &str,
    before: &BTreeMap<String, serde_json::Value>,
    after: &BTreeMap<String, serde_json::Value>,
    live_line: &str,
) -> i32 {
    let mut unmirrored = false;
    let published = book.publish_or_restore(key, before, || match mirror_book(after) {
        Ok(()) => synth(hook),
        Err(e) => {
            eprintln!("{e}");
            unmirrored = true;
            false
        }
    });
    let Some(restored) = published else {
        println!();
        println!("{live_line}");
        let outcome = commit_common_law(home, verb, key);
        let msg = outcome.message(verb, key);
        match outcome {
            CommitOutcome::Failed(_) => eprintln!("{msg}"),
            _ => println!("{msg}"),
        }
        return 0;
    };
    rm_memories_cache();
    let rolled_back = restored && mirror_book(before).is_ok();
    match (unmirrored, rolled_back) {
        (true, true) => eprintln!("rule: {verb} of {key} rolled back, nothing written."),
        (true, false) => eprintln!("rule: the rollback of {key} failed too — run `rule.sh {verb} {key}` again once the mirror is writable."),
        (false, true) => eprintln!("rule: hook '{hook}' failed — wiki page NOT regenerated; {verb} of {key} rolled back, nothing written."),
        (false, false) => {
            eprintln!("rule: hook '{hook}' failed AND the rollback of {key} failed.");
            eprintln!("The book and the wiki page now disagree. Re-run rule.sh {verb} {key} once the hook works.");
        }
    }
    1
}

fn cmd_enact(book: &Book, home: &str, rest: &[String]) -> i32 {
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

    let toc = match code::book_dir().and_then(|d| read_toc(&d)) {
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
    if let Err(e) = code::append(&toc, part, &key) {
        eprintln!("rule: refusing — {e}. The Parts are:");
        eprint!("{}", code::parts_list(&toc));
        return 1;
    }

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

    if let Some(m) = rule::inventory_refusal(&text) {
        eprintln!("{m}");
        spira_otel::oneshot::event("spira_statute_refused", 1, &[("reason", "inventory-fence")]);
        return 1;
    }

    let enacted = book.enact(&key, &text, |current| {
        if let Some(current) = current {
            println!("rule: '{key}' already exists — this enact overwrites it.");
            println!("--- current ---");
            println!("{current}");
            println!("--- new ---");
            println!("{text}");
            println!("---");
        }
        let hook = match resolve_hook(home) {
            Ok(h) => h,
            Err(e) => return ControlFlow::Break(refuse_hook(&e)),
        };
        if dry_run {
            println!("DRY RUN: would enact {key} ({words} words). Nothing written.");
            return ControlFlow::Break(0);
        }
        ControlFlow::Continue(hook)
    });
    let (hook, before) = match enacted {
        Ok(e) => {
            if let Some(notice) = e.put.notice(&key) {
                eprintln!("{notice}");
            }
            if !e.put.in_force() {
                return 1;
            }
            (e.gate, e.before)
        }
        Err(Stop::Gate(rc)) => return rc,
        Err(Stop::Unreachable(e)) => {
            eprintln!("{e}");
            return 1;
        }
        Err(Stop::Refused(e)) => {
            eprintln!("{e}");
            return 1;
        }
        Err(Stop::Duplicate(existing)) => {
            eprintln!("rule: this text is already in force as {existing} — refused, nothing written. Retire or amend {existing} instead.");
            return 1;
        }
    };
    rm_memories_cache();
    println!("enacted {key} ({words} words)");
    println!("{}", tool_line(&tool));

    let mut after = before.clone();
    after.insert(key.clone(), serde_json::Value::String(text.clone()));
    let rc = finish_write(book, home, &hook, "enact", &key, &before, &after, "Statute is live in every agent session at its next call.");
    if rc != 0 {
        return rc;
    }
    let texts = strings(&after);
    let landed = code::book_dir().and_then(|dir| {
        let pointers = read_pointers_for_write(&dir, &texts)?;
        let before_pointers = pointers.clone();
        let (caught, pointers) = code::catch_up(&toc, &pointers, &texts, Some((key.as_str(), part)), &code::numbers_on_disk(&dir), &today()).map_err(|e| format!("rule: {e}"))?;
        let change = code::book_change(&toc, &before_pointers, &caught, &pointers);
        Ok((land_book(home, "enact", &key, &caught, &pointers, &texts)? > 0).then_some(change))
    });
    match landed {
        Ok(Some(n)) => {
            println!("law: {n}; committed and pushed to {}.", book_name());
            0
        }
        Ok(None) => {
            println!("law: book is up to date with the store.");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            log_law_book_failure("enact", &key, &e);
            eprintln!("rule: {key} is in force but was not committed to the book — run rule.sh code --land");
            1
        }
    }
}

fn log_law_book_failure(verb: &str, key: &str, err: &str) {
    let at = spira_config::bounded::bounded("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let line = format!("{at} rule: {verb} {key}: the book was not committed: {}; run rule.sh code --land\n", err.split_whitespace().collect::<Vec<_>>().join(" "));
    let wrote = spira_config::process::cfg("SPIRA_RUN").map_err(|e| e.to_string()).and_then(|run| {
        let log = Path::new(&run).join("law-book.log");
        std::fs::OpenOptions::new().create(true).append(true).open(&log).and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes())).map_err(|e| format!("{}: {e}", log.display()))
    });
    if let Err(e) = wrote {
        eprintln!("rule: the law-book log line could not be written ({e}): {}", line.trim_end());
    }
}

fn book_name() -> String {
    spira_config::process::cfg("SPIRA_LAW_BOOK").unwrap_or_default()
}

fn cmd_land_book(home: &str) -> i32 {
    let run = || -> Result<Option<code::BookChange>, String> {
        let dir = code::book_dir()?;
        let toc = read_toc(&dir)?;
        let book = strings(&read_book()?);
        let pointers = read_pointers_for_write(&dir, &book)?;
        let (caught, caught_pointers) = code::catch_up(&toc, &pointers, &book, None, &code::numbers_on_disk(&dir), &today()).map_err(|e| format!("rule: {e}"))?;
        let change = code::book_change(&toc, &pointers, &caught, &caught_pointers);
        Ok((land_book(home, "code", "book", &caught, &caught_pointers, &book)? > 0).then_some(change))
    };
    match run() {
        Ok(None) => {
            println!("law: book is up to date with the store.");
            0
        }
        Ok(Some(n)) => {
            println!("law: {n}; committed and pushed to {}.", book_name());
            0
        }
        Err(e) => {
            eprintln!("{e}");
            log_law_book_failure("code", "book", &e);
            1
        }
    }
}

/// Renders the book into the wiki checkout and commits and pushes only the paths that changed.
/// Returns how many paths were committed; 0 when the book already matched the store.
fn land_book(home: &str, verb: &str, key: &str, toc: &[code::Part], pointers: &code::Pointers, book: &BTreeMap<String, String>) -> Result<usize, String> {
    let (wiki, rel) = code::wiki_and_book()?;
    code::write_law(&wiki.join(&rel), toc, pointers, book, &today())?;
    let paths = code::changed_paths(&wiki, &rel)?;
    if paths.is_empty() {
        return Ok(0);
    }
    wiki_commit(home, &wiki.to_string_lossy(), &format!("law: {verb} {key}"), &paths).map_err(|e| format!("rule: the book could not be committed: {e}"))?;
    Ok(paths.len())
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

fn read_pointers_for_write(book_dir: &Path, book: &BTreeMap<String, String>) -> Result<code::Pointers, String> {
    read_pointers(book_dir).map(|p| code::with_store_pointers(p, book))
}

fn read_toc(law: &Path) -> Result<Vec<code::Part>, String> {
    let file = law.join("toc.toml");
    let text = std::fs::read_to_string(&file).map_err(|e| format!("rule: {}: {e}", file.display()))?;
    code::parse_toc(&text).map_err(|e| format!("rule: {e}"))
}

fn strings(book: &BTreeMap<String, serde_json::Value>) -> BTreeMap<String, String> {
    book.iter().filter_map(|(k, v)| v.as_str().map(|t| (k.clone(), t.to_string()))).collect()
}

fn book_texts(book: &BTreeMap<String, serde_json::Value>) -> impl Fn(&str) -> Option<String> + '_ {
    move |slug| book.get(slug).and_then(|v| v.as_str()).map(str::to_string)
}


fn cmd_code(rest: &[String]) -> i32 {
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
    let law = match code::book_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let loaded = read_toc(&law).map(|t| (t, read_pointers(&law), code::numbers_on_disk(&law)));
    let (toc, on_disk, numbers) = match loaded {
        Ok(l) => l,
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
        None => match read_book() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        },
    };
    let loaded_toc = toc.clone();
    let recorded_retired: Vec<String> = on_disk.as_ref().map_or(Vec::new(), |p| p.retired.iter().map(|r| r.slug.clone()).collect());
    let on_disk = on_disk.map(|p| code::with_store_pointers(p, &strings(&book)));
    let (toc, pointers) = match on_disk.as_ref().map_err(String::clone).and_then(|p| code::in_force(&toc, p, &strings(&book), &numbers, &today())) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", e.trim_start_matches("rule: "));
            return 1;
        }
    };
    let old = on_disk.unwrap_or_default();
    let new_moves = &pointers.moved[old.moved.len()..];
    let unrecorded: Vec<&str> = pointers.retired.iter().map(|r| r.slug.as_str()).filter(|s| !recorded_retired.iter().any(|r| r == s)).collect();
    if check {
        let expected = code::generate(&toc, &pointers, book_texts(&book), &code::read_pages(&law), &today());
        if let Err(e) = code::require_dates(&expected) {
            eprintln!("{e}");
            return 1;
        }
        let mut drift = code::drift(&law, &expected);
        if toc != loaded_toc {
            let store = strings(&book);
            let lacked: Vec<&str> = loaded_toc.iter().flat_map(|p| p.slugs.iter().map(String::as_str)).filter(|s| !store.contains_key(*s)).collect();
            if lacked.is_empty() {
                drift.push("toc.toml: differs from the book the store generates".to_string());
            } else {
                drift.push(format!("toc.toml: lists statutes the store lacks: {}", lacked.join(", ")));
            }
        }
        if !unrecorded.is_empty() {
            drift.push(format!("{}: retirements in the store not yet recorded: {}", code::POINTERS_FILE, unrecorded.join(", ")));
        }
        drift.extend(code::check_pointers(&toc, &pointers));
        drift.extend(new_moves.iter().map(|m| format!("{}: §{} moved to {}, not yet recorded", code::POINTERS_FILE, m.section, code::section_of(&toc, &m.slug).unwrap_or_default())));
        for d in &drift {
            eprintln!("{}/{d}", book_name());
        }
        let behind = code::behind(&toc, &pointers, &strings(&book));
        if !behind.is_empty() {
            eprintln!("behind the store by {}: {}", behind.len(), behind.join(", "));
            return 1;
        }
        return if drift.is_empty() { 0 } else { 1 };
    }
    let bad = code::check_pointers(&toc, &pointers);
    if !bad.is_empty() {
        for b in &bad {
            eprintln!("{}/{b}", book_name());
        }
        return 1;
    }
    for m in new_moves {
        println!("§{} moved: {} is now §{}", m.section, m.slug, code::section_of(&toc, &m.slug).unwrap_or_default());
    }
    match code::write_law(&law, &toc, &pointers, &strings(&book), &today()) {
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

struct PromptAudit {
    findings: Vec<rule::audit::Finding>,
    total: usize,
}

/// Every `*.md` under `<home>/runes`, one `Finding` per prompt that names something the release
/// lacks; its `missing` holds the full `runes/<file>:<line>: <token>` lines.
fn prompt_audit(home: &str) -> Result<PromptAudit, String> {
    let dir = Path::new(home).join("runes");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| format!("rule: audit cannot read {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    if names.is_empty() {
        return Err(format!("rule: audit found no persona prompts (*.md) in {} — refusing to report them clean", dir.display()));
    }
    names.sort();
    let root = Path::new(home).parent().map(Path::to_path_buf).unwrap_or_default();
    let bins: std::collections::BTreeSet<String> =
        std::fs::read_dir(root.join("bin")).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let resolve = rule::audit::placeholder_resolver(&root, &bins);
    let lookup = rule::audit::release_lookup(home);
    let mut findings = Vec::new();
    for n in &names {
        let text = std::fs::read_to_string(dir.join(n)).map_err(|e| format!("rule: audit cannot read {}: {e}", dir.join(n).display()))?;
        let missing = rule::audit::prompt_findings(n, &text, &resolve, &lookup);
        if !missing.is_empty() {
            findings.push(rule::audit::Finding { slug: format!("runes/{n}"), missing });
        }
    }
    Ok(PromptAudit { findings, total: names.len() })
}

fn cmd_audit(book: &Book, home: &str, rest: &[String]) -> i32 {
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
    let memories = match book.memories() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let findings = audit(&memories, &rule::audit::release_lookup(home));
    for f in &findings {
        println!("{}: {}", f.slug, f.missing.join(", "));
    }
    println!("{} of {} statutes name something the release does not have", findings.len(), memories.keys().filter(|k| k.starts_with("law-")).count());
    let prompts = match prompt_audit(home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    for f in &prompts.findings {
        for t in &f.missing {
            println!("{}", t);
        }
    }
    println!("{} of {} persona prompts name something the release does not have", prompts.findings.len(), prompts.total);
    let mut findings = findings;
    findings.extend(prompts.findings);
    if !file {
        return 0;
    }
    let (batch, bounded) = match book.findings_to_file(&findings, max) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e} — nothing filed");
            return 1;
        }
    };
    for f in batch {
        let title = finding_title(f);
        let body = rule::audit::bead_body(&title);
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
            Ok(s) if s.success() => {}
            _ => eprintln!("rule: audit: filing for {} failed", f.slug),
        }
    }
    if bounded {
        println!("rule: audit filed {max}, the batch bound; the rest wait for the next run.");
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

fn cmd_retire(book: &Book, home: &str, rest: &[String]) -> i32 {
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

    let retired = book.retire(&key, |memories| {
        if !memories.contains_key(&key) {
            eprintln!("rule: no statute '{key}' in the statute book");
            return ControlFlow::Break(1);
        }
        for successor in replaced_by.iter().chain(&merged_into) {
            if *successor == key || !memories.contains_key(successor) {
                eprintln!("rule: refusing — {successor} is not a statute in force, so it cannot stand in for {key}");
                return ControlFlow::Break(1);
            }
        }

        let hook = match resolve_hook(home) {
            Ok(h) => h,
            Err(e) => return ControlFlow::Break(refuse_hook(&e)),
        };
        let refuse = |e: String| {
            eprintln!("{e}");
            ControlFlow::Break(1)
        };
        let law = match code::book_dir() {
            Ok(d) => d,
            Err(e) => return refuse(e),
        };
        let (toc, mut pointers) = match read_toc(&law).and_then(|t| read_pointers_for_write(&law, &strings(memories)).map(|p| (t, p))) {
            Ok(tp) => tp,
            Err(e) => return refuse(e),
        };
        if code::section_of(&toc, &key).is_none() {
            return refuse(format!("rule: refusing — {key} holds no section in the book's toc.toml, so no pointer can be left for it. Run rule.sh code --land to file it in a Part, then retire it again."));
        }
        let on = today();
        let record = merged_into.is_none().then(|| retirement_record(&key, &reason, replaced_by.as_deref(), memories));
        match merged_into.clone() {
            Some(into) => pointers.merged.push(code::Merged { from: key.clone(), into, reason: reason.clone(), on }),
            None => pointers.retired.push(code::Retired { slug: key.clone(), reason: reason.clone(), replaced_by: replaced_by.clone(), on }),
        }
        if let Err(e) = code::resolve(&toc, &pointers, &key) {
            return refuse(format!("rule: refusing — {e}"));
        }
        ControlFlow::Continue((record, (hook, law, toc, pointers)))
    });
    let (memories, (hook, law, toc, pointers)) = match retired {
        Ok(r) => r,
        Err(Stop::Gate(rc)) => return rc,
        Err(Stop::Unreachable(e) | Stop::Refused(e) | Stop::Duplicate(e)) => {
            eprintln!("{e}");
            return 1;
        }
    };
    println!("forgot {key}");
    rm_memories_cache();

    let mut after = memories.clone();
    after.remove(&key);
    let rc = finish_write(
        book,
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
    let texts = strings(&after);
    let landed = code::catch_up(&toc, &pointers, &texts, None, &code::numbers_on_disk(&law), &today())
        .map_err(|e| format!("rule: {e}"))
        .and_then(|(caught, pointers)| land_book(home, "retire", &key, &caught, &pointers, &texts));
    match landed {
        Ok(_) => {
            println!("law: {key} leaves the book and its pointer stays; committed and pushed to {}.", book_name());
            0
        }
        Err(e) => {
            eprintln!("{e}");
            log_law_book_failure("retire", &key, &e);
            eprintln!("rule: {key} is retired but was not committed to the book — run `rule.sh code --land`.");
            1
        }
    }
}

fn retirement_record(from: &str, reason: &str, replaced_by: Option<&str>, before: &BTreeMap<String, serde_json::Value>) -> String {
    use sop::ports::Clock;
    let (epoch, at) = sop::real::RealClock { tz: "UTC".into() }.now();
    let by = std::env::var("BEADS_ACTOR")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".to_string());
    let text = before.get(from).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    sop::shelf::Retired { by, at, epoch, reason: reason.into(), text, replaced_by: replaced_by.map(String::from) }.to_json()
}

fn cmd_rename_vocabulary(book: &Book, home: &str, rest: &[String]) -> i32 {
    use rule::vocab::{plan, Entry};
    let dry_run = match rest {
        [] => false,
        [f] if f == "--dry-run" => true,
        _ => {
            println!("{USAGE}");
            return 1;
        }
    };
    let mut failed = false;
    let outcome = book.rename(
        |before| {
            let refuse = |e: String| {
                eprintln!("{e}");
                ControlFlow::Break(1)
            };
            let law = match code::book_dir() {
                Ok(d) => d,
                Err(e) => return refuse(e),
            };
            let (toc, pointers) = match read_toc(&law).and_then(|t| read_pointers_for_write(&law, &strings(before)).map(|p| (t, p))) {
                Ok(tp) => tp,
                Err(e) => return refuse(e),
            };
            let entries = plan(&strings(before), &toc, &pointers);
            for e in &entries {
                match e {
                    Entry::Unresolved { key, terms } => {
                        eprintln!("rule: {key} holds {terms:?} with no replacement; rephrase it by hand with enact");
                        failed = true;
                    }
                    Entry::Collision { from, to } => {
                        eprintln!("rule: refusing to rename {from}: {to} is already a statute in force (or another rename's target)");
                        failed = true;
                    }
                    _ => {}
                }
            }
            if dry_run {
                print!("{}", rule::vocab::dry_run_report(&strings(before), &entries));
                let runbook = |e: &Entry| matches!(e, Entry::Amend { key, .. } if key.starts_with("sop-")) || matches!(e, Entry::Rename { from, .. } if from.starts_with("sop-"));
                let written = entries.iter().filter(|e| matches!(e, Entry::Amend { .. } | Entry::Rename { .. }));
                let runbooks = written.clone().filter(|e| runbook(e)).count();
                let statutes = written.count() - runbooks;
                println!("DRY RUN: {statutes} statute(s) and {runbooks} runbook(s) would be rewritten. Nothing written.");
                return ControlFlow::Break(i32::from(failed));
            }
            ControlFlow::Continue((entries, (law, toc, pointers)))
        },
        |before, from, to| retirement_record(from, "renamed out of retired vocabulary", Some(to), before),
    );
    let (done, (law, toc, pointers)) = match outcome {
        Ok(o) => o,
        Err(Stop::Gate(rc)) => return rc,
        Err(Stop::Unreachable(e) | Stop::Refused(e) | Stop::Duplicate(e)) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let Renamed { after, written, statutes: statutes_written, runbooks: runbooks_written, statute_renames: renamed, failed: write_failed } = done;
    failed |= write_failed;
    rm_memories_cache();
    if written == 0 {
        eprintln!("rule: nothing rewritten, nothing landed.");
        return i32::from(failed);
    }
    if let Err(e) = mirror_book(&after) {
        eprintln!("{e}");
        return 1;
    }
    match resolve_hook(home) {
        Ok(hook) if synth(&hook) => {
            let _ = commit_common_law(home, "rename-vocabulary", "book");
        }
        Ok(hook) => eprintln!("rule: hook '{hook}' failed — wiki page NOT regenerated."),
        Err(e) => eprintln!("rule: {e} — wiki page NOT regenerated."),
    }
    if statutes_written == 0 {
        println!("law: no statute rewritten; nothing to commit.");
        return i32::from(failed);
    }
    let texts = strings(&after);
    let landed = rule::vocab::record_renames(&toc, &pointers, &renamed, &today()).and_then(|(toc, pointers)| code::catch_up(&toc, &pointers, &texts, None, &code::numbers_on_disk(&law), &today())).map_err(|e| format!("rule: {e}")).and_then(|(caught, pointers)| land_book(home, "rename-vocabulary", "book", &caught, &pointers, &texts));
    match landed {
        Ok(_) => println!("law: {statutes_written} statute(s) rewritten ({runbooks_written} runbook(s) in the store); committed and pushed to {}.", book_name()),
        Err(e) => {
            eprintln!("{e}");
            log_law_book_failure("rename-vocabulary", "book", &e);
            eprintln!("rule: the store is rewritten but the book was not committed — run `rule.sh code --land`.");
            return 1;
        }
    }
    i32::from(failed)
}

fn cmd_list() -> i32 {
    match read_book() {
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

fn cmd_show(rest: &[String]) -> i32 {
    if rest.len() != 1 {
        println!("{USAGE}");
        return 1;
    }
    let key = slugify(&rest[0]);
    let book = match read_book() {
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
    let looked = code::book_dir().and_then(|law| Ok((read_toc(&law)?, read_pointers(&law)?)));
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
// `SPIRA_MEMORIES_CMD` test seam, and the content door's `memories --json` read.
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
/// verbatim through a shell and the store is never touched. Unset, it is the content door's
/// `memories --json`.
fn fetch_memories_json() -> String {
    if let Ok(cmd) = std::env::var("SPIRA_MEMORIES_CMD") {
        if !cmd.is_empty() {
            let envs = match spira_config::release_env::child_path_env_for_process() {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("rule: {e}");
                    return String::new();
                }
            };
            // batch-job: runs a gate, build or forge script that takes as long as its work
            return Command::new("bash").envs(envs)
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
    match configured_book() {
        Ok(book) => book.memories_text().unwrap_or_default(),
        Err(e) => {
            eprintln!("{e}");
            String::new()
        }
    }
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

    #[test_id(tw9e7zp)]
    #[test]
    fn an_empty_key_defaults_under_run() {
        assert_eq!(statute_dir_from("", "r"), Some("r/statutes".into()));
        assert_eq!(statute_dir_from("s", "r"), Some("s".into()));
        assert_eq!(statute_dir_from("", ""), None);
    }
}
