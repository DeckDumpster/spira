//! `content <bd-subcommand> …` — the one door for bead content: its reads, and the narrow
//! writes a task makes (labels, comments, notes, metadata, a close, a filing), so nothing
//! outside the store forks `bd`. Reads are SQL against the beads database (`content_sql`); the
//! writes fork bd with bd's own argv, which owns id allocation and events. State is never
//! served here: the machine's own verbs answer that. Anything outside the allowlist is refused.

use crate::callers::Answer;

const REFUSED: i32 = 3;
const CANNOT_TELL: i32 = 2;

/// What an allowed sub-verb is: a read, or a write, which takes the request body as bd's stdin
/// when `body` (`note --stdin`, `create --body-file -`).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Kind {
    Read,
    Write { body: bool },
}

const WRITE: Result<Kind, String> = Ok(Kind::Write { body: false });

fn word(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-')
}

pub fn check(args: &[String]) -> Result<Kind, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["list" | "show" | "memories" | "state" | "search" | "events", ..] => Ok(Kind::Read),
        ["dep", "list", ..] => Ok(Kind::Read),
        ["label", "list", id] | ["label", "list", id, "--json"] if word(id) => Ok(Kind::Read),
        ["gate", "list", ..] => Ok(Kind::Read),
        // The planning store's id prefix, which a caller filing beads stamps its ids with.
        ["config", "get", key] if !key.starts_with('-') => Ok(Kind::Read),
        ["comments", "add", id, _text] if !id.starts_with('-') => WRITE,
        ["comments", sub, ..] if *sub != "add" => Ok(Kind::Read),
        ["label", "add" | "remove", id, label] if word(id) && word(label) => WRITE,
        ["note", id, "--stdin"] if word(id) => Ok(Kind::Write { body: true }),
        ["note", id, text] if word(id) && !text.starts_with('-') && !text.trim().is_empty() => WRITE,
        ["close", id] if word(id) => WRITE,
        ["close", id, "--reason", reason] if word(id) && !reason.trim().is_empty() => WRITE,
        ["update", id, flags @ ..] if !id.starts_with('-') && !flags.is_empty() => update_flags(flags),
        ["create", rest @ ..] if !rest.is_empty() => create_flags(rest),
        ["ready", ..] => Err(unserved("ready", "`spira-lc list --state READY,REWORK` (the published claim_verdict) or `spira-claim rune-ready`")),
        ["blocked", ..] => Err(unserved("blocked", "`spira-lc list` (the published blocked_by)")),
        ["history", ..] => Err(unserved("history", "`spira-lc facts --ids` or `content list --updated-after`")),
        ["children", ..] => Err(unserved("children", "`content list --all --limit 0 --parent <id>`")),
        ["sql", ..] => Err(unserved("sql", "a typed read: add the read this query stands for to the door (content_sql) rather than sending SQL")),
        _ => Err(format!("`{}` is not a content verb", a.join(" "))),
    }
}

fn unserved(verb: &str, instead: &str) -> String {
    format!("`{verb}` is not a content verb; use {instead}")
}

fn update_flags(flags: &[&str]) -> Result<Kind, String> {
    let mut it = flags.iter();
    while let Some(f) = it.next() {
        match (*f, it.next()) {
            ("--add-label" | "--remove-label", Some(l)) if !l.is_empty() => {}
            ("--set-metadata", Some(kv)) if kv.split_once('=').is_some_and(|(k, _)| word(k)) => {}
            _ => return Err(format!("update takes only --add-label/--remove-label <label> and --set-metadata <key=value>, not `{f}`")),
        }
    }
    WRITE
}

/// `create [<title>] <flag>…`: the filing flags the store's writers pass, a body only on stdin.
fn create_flags(args: &[&str]) -> Result<Kind, String> {
    let (mut titled, mut body) = (false, false);
    let mut it = args.iter().enumerate();
    while let Some((i, f)) = it.next() {
        match *f {
            t if i == 0 && word(t) => titled = true,
            "--silent" | "--json" => {}
            "--body-file" => match it.next() {
                Some((_, &"-")) => body = true,
                _ => return Err("create takes its body only as --body-file - (the request body)".into()),
            },
            "--title" | "--type" | "-t" | "--priority" | "-p" | "--labels" | "--label" | "-l" | "--external-ref" | "--parent" | "--deps" | "--description" | "-d" | "--assignee" | "--id" => match it.next() {
                Some((_, v)) if !v.is_empty() => titled |= *f == "--title",
                _ => return Err(format!("create: `{f}` needs a value")),
            },
            _ => return Err(format!("create takes only a title and the filing flags, not `{f}`")),
        }
    }
    if !titled {
        return Err("create needs a title".into());
    }
    Ok(Kind::Write { body })
}

/// The programs whose ticks must not read the whole bead history (a caller is named
/// `<program>` or `<program>/<subcommand>`).
const TICK_DRIVEN: &[&str] = &["spira-runtime", "cockpit-collect", "panel"];

/// Refuses `content list --all --limit 0` (exit 3) from a scheduler task or the cockpit collector.
pub fn refuse_whole_history(args: &[String], caller: &str) -> Option<Answer> {
    let argv: Vec<&str> = std::iter::once("content").chain(args.iter().map(String::as_str)).collect();
    let program = caller.split('/').next().unwrap_or("");
    if !spira_config::lc_call::reads_whole_history(&argv) || !TICK_DRIVEN.contains(&program) {
        return None;
    }
    Some(Answer {
        code: REFUSED,
        stderr: format!("spira-lc content: `list --all --limit 0` reads every bead ever filed and is refused from {caller} (exit {REFUSED}); read open beads, or bound the read with --id, --parent or --updated-after/--closed-after\n"),
        ..Default::default()
    })
}

/// A content write: bd's argv, and its stdin when the sub-verb takes the request body.
pub type Write<'a> = &'a dyn Fn(&[String], Option<&str>) -> Result<String, String>;

/// Whether the client sends its stdin as the request body for `args` (after `content`).
pub fn takes_body(args: &[String]) -> bool {
    matches!(check(args), Ok(Kind::Write { body: true }))
}

/// What the guards read besides the machine: the caller's guard variables (off the request),
/// the ask label and the repo map (this installation's), and the time.
pub struct Guards<'a> {
    pub caller: &'a std::collections::BTreeMap<String, String>,
    pub ask_label: &'a dyn Fn() -> Result<String, String>,
    pub registry: &'a dyn Fn() -> spira_config::repos::Registry,
    pub now: i64,
}

/// bdq's write guards on a content write (`bead::guard::write`), in bdq's order: the
/// create-time fences, the ask-label write fence, the state-verb fence, the czar fence, the
/// claimed-description check. The lifecycle row comes from the machine `m`; `body` is the
/// request body a create judges as its description. A refusal is bdq's: exit 1 and its text,
/// so a write through this door is refused whenever the same write through bdq would be.
/// `Ok` carries the lines to log (an overridden state write).
pub fn guard(args: &[String], body: Option<&str>, g: &Guards, m: &mut dyn crate::callers::Machine) -> Result<String, Answer> {
    let rows = std::cell::RefCell::new(m);
    let row = |id: &str| match rows.borrow_mut().call(&["show".to_string(), id.to_string()]) {
        (0, out) => spira_config::lc_state::parse_show(&out).ok().flatten(),
        _ => None,
    };
    let vars = |k: &str| g.caller.get(k).cloned();
    let ctx = bead::guard::Ctx { who: "spira-lc content", vars: &vars, rows: &row, now: g.now, ask_label: g.ask_label, registry: g.registry };
    match bead::guard::write(args, body, &ctx) {
        Ok(bead::guard::Admit { claim: bead::guard::Claim::Free, log }) => Ok(log),
        // The door takes no --force-claimed (`check`), so an acknowledged edit never reaches
        // here; were one to, it is refused rather than written without its acknowledgment.
        Ok(bead::guard::Admit { claim: bead::guard::Claim::Forced { id, .. }, .. }) => {
            Err(Answer { code: REFUSED, stderr: format!("spira-lc content: {id}: a claimed edit is acknowledged only through bdq's --force-claimed\n"), ..Default::default() })
        }
        Err(r) => Err(Answer { code: r.code, stderr: r.text, ..Default::default() }),
    }
}

/// `create-bead` is idempotent, so a transient failure is retried before it is reported, as
/// bdq's `ensure_row_retrying` does.
const ROW_ATTEMPTS: u32 = 3;

/// What a write bd applied owes the lifecycle machine (`bead::guard::after`), written here in
/// the same request: a row for every bead a create's `stdout` names, a terminal row for a
/// closed bead. bd's write stands whatever happens; a lifecycle write that fails is reported
/// (the returned stderr lines), never swallowed and never a reason to repeat the bd write.
pub fn after(args: &[String], stdout: &str, m: &mut dyn crate::callers::Machine, bd: &mut dyn crate::callers::Bd, backoff_ms: u64) -> String {
    match bead::guard::after(args) {
        bead::guard::After::Nothing => String::new(),
        bead::guard::After::Rows => {
            let ids = spira_config::lifecycle_row::created_ids(stdout);
            if ids.is_empty() {
                return format!("spira-lc content: LIFECYCLE: cannot read any created bead id from {stdout:?}; the new beads have no lifecycle row and cannot be claimed\n");
            }
            let mut errs = Vec::new();
            for id in &ids {
                let argv = ["create-bead".to_string(), id.clone(), "--ready".to_string()];
                let mut last = m.call(&argv);
                for n in 1..ROW_ATTEMPTS {
                    if last.0 == 0 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms * u64::from(n)));
                    last = m.call(&argv);
                }
                if last.0 != 0 {
                    errs.push(format!("create-bead {id} --ready exited {}: {}", last.0, last.1.trim()));
                }
            }
            if errs.is_empty() {
                String::new()
            } else {
                format!("spira-lc content: LIFECYCLE: some created beads have NO lifecycle row and cannot be claimed: {}\n", errs.join("; "))
            }
        }
        bead::guard::After::Terminal(id) => {
            let ans = crate::callers::reconcile_closed(&["--apply".to_string(), id.clone()], m, bd);
            if ans.code == 0 {
                String::new()
            } else {
                format!("spira-lc content: LIFECYCLE: {id} was closed but its lifecycle row is not terminal and may read as claimable: {}\n", format!("{}{}", ans.stdout, ans.stderr).trim())
            }
        }
    }
}

/// A content write's guards: `Ok` is the lines to log, `Err` the refusal answered instead of
/// the write. Given the write's argv and its request body.
pub type Guard<'a> = &'a mut dyn FnMut(&[String], Option<&str>) -> Result<String, Answer>;
/// What a write bd applied owes the lifecycle machine: the lines reporting any that failed.
pub type After<'a> = &'a mut dyn FnMut(&[String], &str) -> String;

/// One content call. A read goes to `read`. A write reads its request body when the sub-verb
/// takes one (refused, before bd runs, when none was sent), passes `guard` (bdq's write
/// guards), goes to `write`, and once bd has applied it, owes the lifecycle machine `after`.
pub fn run(
    args: &[String],
    stdin: &mut dyn FnMut() -> Result<String, String>,
    guard: Guard<'_>,
    after: After<'_>,
    read: &dyn Fn(&[String]) -> Result<String, crate::composed::ReadError>,
    write: Write<'_>,
) -> Answer {
    if args.is_empty() {
        return Answer { code: CANNOT_TELL, stderr: "usage: spira-lc content <list|show|search|dep list|label list|events|comments|gate list|memories|state|config get|comments add|label add|label remove|note|close|update|create> …\n".into(), ..Default::default() };
    }
    let kind = match check(args) {
        Ok(k) => k,
        Err(e) => return Answer { code: REFUSED, stderr: format!("spira-lc content: {e}\n"), ..Default::default() },
    };
    let body = match kind {
        Kind::Read => {
            return match read(args) {
                Err(e) if e.cannot_tell => Answer { code: CANNOT_TELL, stderr: format!("spira-lc content: cannot tell: {}\n", e.text.trim_end()), ..Default::default() },
                res => answer_of(res.map_err(|e| e.text), String::new()),
            }
        }
        Kind::Write { body: false } => None,
        Kind::Write { body: true } => match stdin() {
            Ok(b) => Some(b),
            Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc content: stdin: {e}\n"), ..Default::default() },
        },
    };
    let log = match guard(args, body.as_deref()) {
        Ok(log) => log,
        Err(refused) => return refused,
    };
    let res = write(args, body.as_deref());
    let report = match &res {
        Ok(out) => after(args, out),
        Err(_) => String::new(),
    };
    answer_of(res, format!("{log}{report}"))
}

fn answer_of(res: Result<String, String>, stderr: String) -> Answer {
    match res {
        Ok(out) => Answer { stdout: out, stderr, ..Default::default() },
        Err(e) => Answer { code: 1, stderr: format!("{stderr}{}\n", e.trim_end()), ..Default::default() },
    }
}

/// The labels a bd alert item carried before alerts became control-plane conditions: the alert
/// itself and the watchdog's old write-probe item.
pub const ALERT_LABELS: &[&str] = &["alert", "heimdall:probe"];
const DELETE_CHUNK: usize = 100;

/// `content purge-alerts [--apply]`: every bd item carrying an [`ALERT_LABELS`] label. A dry
/// run reports the count; `--apply` writes each item as one JSONL line to a fresh file under
/// `export_dir` and only then deletes them through `delete`. With none left, nothing is written,
/// so a second run changes nothing.
pub fn purge_alerts(
    args: &[String],
    bd: &dyn Fn(&[String]) -> Result<String, String>,
    delete: &dyn Fn(&[String]) -> Result<(), String>,
    export_dir: &std::path::Path,
    now: i64,
) -> Answer {
    let apply = match args {
        [] => false,
        [a] if a == "--apply" => true,
        _ => return Answer { code: CANNOT_TELL, stderr: "usage: spira-lc content purge-alerts [--apply]\n".into(), ..Default::default() },
    };
    let cannot = |why: String| Answer { code: CANNOT_TELL, stderr: format!("spira-lc content purge-alerts: {why}\n"), ..Default::default() };
    let mut items: Vec<serde_json::Value> = Vec::new();
    for label in ALERT_LABELS {
        let argv: Vec<String> = ["list", "--all", "--limit", "0", "--label", label, "--json"].iter().map(|s| s.to_string()).collect();
        let out = match bd(&argv) {
            Ok(o) => o,
            Err(e) => return cannot(format!("bd list --label {label} failed: {}", e.trim())),
        };
        let text = out.find(['[', '{']).map_or("[]", |i| &out[i..]);
        let rows = match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Array(a)) => a,
            Ok(o @ serde_json::Value::Object(_)) => vec![o],
            _ => return cannot(format!("bd list --label {label} printed no JSON list")),
        };
        for r in rows {
            if r.get("id").and_then(|i| i.as_str()).is_some() && !items.iter().any(|x| x["id"] == r["id"]) {
                items.push(r);
            }
        }
    }
    let ids: Vec<String> = items.iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect();
    if !apply || ids.is_empty() {
        let note = if apply { "" } else { " (dry run; --apply to export and delete)" };
        return Answer { stdout: format!("purge-alerts: {} bd alert item(s){note}", ids.len()), ..Default::default() };
    }
    let file = export_dir.join(format!("purge-alerts-{now}.jsonl"));
    let lines: String = items.iter().map(|r| format!("{r}\n")).collect();
    let written = std::fs::create_dir_all(export_dir)
        .and_then(|()| std::fs::OpenOptions::new().write(true).create_new(true).open(&file))
        .and_then(|mut f| std::io::Write::write_all(&mut f, lines.as_bytes()).and_then(|()| f.sync_all()));
    if let Err(e) = written {
        return cannot(format!("{}: {e} — nothing deleted", file.display()));
    }
    for chunk in ids.chunks(DELETE_CHUNK) {
        if let Err(e) = delete(chunk) {
            return cannot(format!("delete failed after exporting to {}: {} — re-run to finish", file.display(), e.trim()));
        }
    }
    Answer { stdout: format!("purge-alerts: exported {} bd alert item(s) to {} and deleted them", ids.len(), file.display()), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// A bd holding items by label; `delete` removes them.
    struct Bd(RefCell<BTreeMap<String, (String, serde_json::Value)>>);

    impl Bd {
        fn new(items: &[(&str, &str)]) -> Self {
            Bd(RefCell::new(items.iter().map(|(id, l)| (id.to_string(), (l.to_string(), serde_json::json!({"id": id, "labels": [l], "title": format!("{id} fired")})))).collect()))
        }
        fn list(&self, a: &[String]) -> Result<String, String> {
            let label = a.iter().position(|x| x == "--label").map(|i| a[i + 1].clone()).unwrap();
            let rows: Vec<serde_json::Value> = self.0.borrow().values().filter(|(l, _)| *l == label).map(|(_, r)| r.clone()).collect();
            Ok(format!("note: bd says hello\n{}", serde_json::Value::Array(rows)))
        }
        fn delete(&self, ids: &[String]) -> Result<(), String> {
            for i in ids {
                self.0.borrow_mut().remove(i);
            }
            Ok(())
        }
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_tick_driven_caller_is_refused_the_whole_history_with_exit_3_named() {
        let whole = argv(&["list", "--all", "--limit", "0", "--json"]);
        for caller in ["spira-runtime", "spira-runtime/list", "cockpit-collect/list", "panel"] {
            let r = refuse_whole_history(&whole, caller).unwrap_or_else(|| panic!("{caller} was served the whole history"));
            assert_eq!(r.code, REFUSED);
            assert!(r.stderr.contains("exit 3") && r.stderr.contains(caller), "{}", r.stderr);
        }
    }

    #[test]
    fn other_callers_and_bounded_reads_are_not_refused() {
        let whole = argv(&["list", "--all", "--limit", "0", "--json"]);
        assert!(refuse_whole_history(&whole, "spira-lc").is_none(), "an operator's own read stands");
        for bounded in [&["list", "--limit", "0", "--json"][..], &["list", "--all", "--limit", "0", "--id", "sp-a"], &["list", "--all", "--limit", "0", "--parent", "sp-e"], &["list", "--all", "--limit", "0", "--closed-after", "2026-11-01"], &["list", "--all", "--json"]] {
            assert!(refuse_whole_history(&argv(bounded), "spira-runtime/list").is_none(), "{bounded:?}");
        }
        assert!(refuse_whole_history(&argv(&["list", "--all", "--limit", "0", "--label-any", "x"]), "spira-runtime").is_some(), "a label can match every bead");
    }

    #[test]
    fn purge_alerts_reports_dry_then_exports_before_deleting_and_a_second_run_changes_nothing() {
        let dir = testkit::TempDir::new("purge-alerts");
        let bd = Bd::new(&[("sp-a1", "alert"), ("sp-a2", "alert"), ("sp-p", "heimdall:probe"), ("sp-w", "plan")]);
        let deleted = RefCell::new(Vec::new());
        let del = |ids: &[String]| {
            assert!(std::fs::read_dir(dir.path()).unwrap().count() == 1, "exported before any delete");
            deleted.borrow_mut().extend(ids.iter().cloned());
            bd.delete(ids)
        };
        let dry = purge_alerts(&[], &|a| bd.list(a), &|_| panic!("a dry run deletes nothing"), dir.path(), 7);
        assert_eq!((dry.code, dry.stdout.contains("3 bd alert item(s) (dry run")), (0, true), "{dry:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "a dry run writes nothing");

        let applied = purge_alerts(&v(&["--apply"]), &|a| bd.list(a), &del, dir.path(), 7);
        assert_eq!(applied.code, 0, "{applied:?}");
        let export = std::fs::read_to_string(dir.join("purge-alerts-7.jsonl")).unwrap();
        let ids: Vec<String> = export.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(ids, ["sp-a1", "sp-a2", "sp-p"]);
        assert_eq!(*deleted.borrow(), ids);
        assert!(bd.0.borrow().contains_key("sp-w"), "a work bead is never touched");

        let again = purge_alerts(&v(&["--apply"]), &|a| bd.list(a), &|_| panic!("nothing left to delete"), dir.path(), 8);
        assert!(again.stdout.contains("0 bd alert item(s)"), "{again:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "a second run writes nothing");
    }

    #[test]
    fn purge_alerts_deletes_nothing_when_bd_cannot_be_read() {
        let dir = testkit::TempDir::new("purge-alerts-down");
        let a = purge_alerts(&v(&["--apply"]), &|_| Err("down".into()), &|_| panic!("must not delete"), dir.path(), 1);
        assert_eq!(a.code, CANNOT_TELL);
        assert_eq!(purge_alerts(&v(&["--force"]), &|_| panic!(), &|_| panic!(), dir.path(), 1).code, CANNOT_TELL);
    }

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn reads_and_the_task_writes_pass() {
        for (ok, kind) in [
            (&["list", "--all", "--json"][..], Kind::Read),
            (&["show", "sp-a", "--json"], Kind::Read),
            (&["comments", "sp-a", "--json"], Kind::Read),
            (&["gate", "list", "--json"], Kind::Read),
            (&["state", "sp-a", "rune"], Kind::Read),
            (&["memories", "--json"], Kind::Read),
            (&["search", "merge queue", "--json"], Kind::Read),
            (&["dep", "list", "sp-a", "--direction=up", "--json"], Kind::Read),
            (&["label", "list", "sp-a", "--json"], Kind::Read),
            (&["events", "--id", "sp-a,sp-b"], Kind::Read),
            (&["comments", "add", "sp-a", "text"], Kind::Write { body: false }),
            (&["update", "sp-a", "--add-label", "x", "--remove-label", "y"], Kind::Write { body: false }),
            (&["update", "sp-a", "--set-metadata", "k=v", "--add-label", "x"], Kind::Write { body: false }),
            (&["label", "add", "sp-a", "x"], Kind::Write { body: false }),
            (&["label", "remove", "sp-a", "x"], Kind::Write { body: false }),
            (&["note", "sp-a", "text"], Kind::Write { body: false }),
            (&["note", "sp-a", "--stdin"], Kind::Write { body: true }),
            (&["close", "sp-a"], Kind::Write { body: false }),
            (&["close", "sp-a", "--reason", "r"], Kind::Write { body: false }),
            (&["create", "t"], Kind::Write { body: false }),
            (&["create", "T", "-t", "bug", "-p", "2", "--labels", "a,b", "--external-ref", "gh-1", "--body-file", "-", "--silent"], Kind::Write { body: true }),
            (&["create", "--parent", "sp-e", "--silent", "--title", "T"], Kind::Write { body: false }),
        ] {
            assert_eq!(check(&v(ok)), Ok(kind), "{ok:?}");
        }
    }

    #[test]
    fn a_read_the_door_does_not_serve_is_refused_naming_its_exit() {
        for (args, exit) in [
            (&["ready"][..], "claim_verdict"),
            (&["blocked"], "blocked_by"),
            (&["history", "sp-a"], "facts"),
            (&["children", "sp-a"], "--parent"),
            (&["sql", "select 1"], "typed read"),
        ] {
            let e = check(&v(args)).unwrap_err();
            assert!(e.contains(exit), "{args:?}: {e}");
        }
    }

    #[test]
    fn every_other_write_is_refused() {
        for bad in [
            &["close", "sp-a", "--reason", "r", "--force"][..],
            &["close", "sp-a", "--reason", " "],
            &["close", "--force"],
            &["update", "sp-a", "--status", "open"],
            &["update", "sp-a", "--add-label"],
            &["update", "sp-a", "--set-metadata", "novalue"],
            &["update", "sp-a", "--set-metadata", "=v"],
            &["update", "sp-a"],
            &["label", "add", "sp-a"],
            &["label", "list", "sp-a", "x"],
            &["note", "sp-a", "--file", "x"],
            &["note", "sp-a", " "],
            &["create"],
            &["create", "--type", "task"],
            &["create", "T", "--body-file", "/etc/passwd"],
            &["create", "T", "--force"],
            &["comments", "add", "sp-a"],
            &["delete", "sp-a"],
            &["reopen", "sp-a"],
            &["set-state", "sp-a", "rune=x"],
            &["dep", "add", "sp-a", "sp-b"],
            &["gate", "resolve", "x"],
            &[][..],
        ] {
            assert!(check(&v(bad)).is_err(), "{bad:?}");
        }
    }

    /// The lifecycle machine as the guard reads it: `show <id>` answers `row`, or exit 1.
    struct Rows(Option<serde_json::Value>);

    impl crate::callers::Machine for Rows {
        fn call(&mut self, args: &[String]) -> (i32, String) {
            assert_eq!(args.first().map(String::as_str), Some("show"), "the guard only reads a row: {args:?}");
            match &self.0 {
                Some(r) => (0, serde_json::json!({"bead": r, "delivery": null}).to_string()),
                None => (1, String::new()),
            }
        }
    }

    const NOW: i64 = 1_800_000_000;

    fn caller(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A write's verdict: admitted, or refused with an exit code and its text.
    type Verdict = Result<(), (i32, String)>;

    // literal-ok: test fixture label
    const ASK: &str = "needs-ryan";

    fn registry() -> spira_config::repos::Registry {
        let map = "# name|root|base|gate\nspira|/r/spira|main|gate.sh\ndeck|/r/deck|main|gate.sh\n";
        spira_config::repos::Registry::new(Some(map), &BTreeMap::new(), std::path::Path::new("/r/spira"))
    }

    fn row_of(row: Option<&serde_json::Value>) -> Option<spira_config::lc_state::Row> {
        row.and_then(|r| spira_config::lc_state::parse_show(&serde_json::json!({"bead": r}).to_string()).ok().flatten())
    }

    /// bdq's guards as `src/bin/bdq.rs` calls them, each at its own point and in its order: the
    /// create-time fences, the ask-label write fence and the state-verb fence; then the czar
    /// fence; then the claimed-description check — on the process environment and the row.
    fn through_bdq(args: &[String], body: Option<&str>, env: &BTreeMap<String, String>, row: Option<&serde_json::Value>) -> Verdict {
        let vars = |k: &str| env.get(k).cloned();
        let rows = |_: &str| row_of(row);
        let ask = || Ok::<_, String>(ASK.to_string());
        let ctx = bead::guard::Ctx { who: "bdq", vars: &vars, rows: &|_| None, now: 0, ask_label: &ask, registry: &registry };
        let err = |r: bead::guard::Refusal| (r.code, r.text);
        bead::guard::create(args, body, &ctx).map_err(err)?;
        bead::guard::ask_label_write(args, &ctx).map_err(err)?;
        bead::guard::state_verb(args, &vars, "bdq").map_err(err)?;
        bead::guard::czar(args, &vars).map_err(err)?;
        bead::guard::claim(args, &rows, NOW).map(|_| ()).map_err(err)
    }

    /// The content door's guard, as the server runs it: the caller's variables off the request,
    /// the row off the machine.
    fn through_content(args: &[String], body: Option<&str>, env: &BTreeMap<String, String>, row: Option<&serde_json::Value>) -> Verdict {
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: env, ask_label: &ask, registry: &registry, now: NOW };
        guard(args, body, &g, &mut Rows(row.cloned())).map(|_| ()).map_err(|a| (a.code, a.stderr))
    }

    fn czar(stage: Option<&str>) -> BTreeMap<String, String> {
        let mut c = caller(&[("SPIRA_RUNE", "czar"), ("SPIRA_CZAR_CLASS", "loop-stalled"), ("SPIRA_CZAR_TRIGGER_BEAD", "sp-t")]);
        if let Some(s) = stage {
            c.insert("SPIRA_CZAR_STAGE_LOOP_STALLED".into(), s.into());
        }
        c
    }

    const WHOLE_ASK: &str = "## Question\nWhich?\nDefault: a\nClass: policy\n";

    #[test]
    fn the_content_guard_reaches_bdqs_verdict_on_every_write() {
        let with_override = |mut c: BTreeMap<String, String>| {
            c.insert("SPIRA_BDQ_STATE_WRITE".into(), "landing".into());
            c
        };
        let contexts = [
            ("no rune", caller(&[])),
            ("state override", with_override(caller(&[]))),
            ("builder", caller(&[("SPIRA_RUNE", "builder"), ("SPIRA_CZAR_CLASS", "loop-stalled")])),
            ("czar, class unset", caller(&[("SPIRA_RUNE", "czar")])),
            ("czar shadow (unset)", czar(None)),
            ("czar shadow, state override", with_override(czar(Some("shadow")))),
            ("czar act, state override", with_override(czar(Some("act")))),
            ("czar unknown stage", czar(Some("loud"))),
        ];
        let held = serde_json::json!({"bead_id": "sp-a", "state": "WORKING", "holder": "dwarf-sp-a", "lease_until": NOW + 600});
        let lapsed = serde_json::json!({"bead_id": "sp-a", "state": "WORKING", "holder": "dwarf-sp-a", "lease_until": NOW - 1});
        let ready = serde_json::json!({"bead_id": "sp-a", "state": "READY"});
        let rows = [None, Some(&held), Some(&lapsed), Some(&ready)];
        let writes: &[(&[&str], Option<&str>)] = &[
            (&["close", "sp-a"], None),
            (&["close", "sp-a", "--reason", "r"], None),
            (&["close", "sp-t"], None),
            (&["update", "sp-a", "--add-label", "x"], None),
            (&["update", "sp-a", "--add-label", ASK], None),
            (&["update", "sp-a", "--remove-label", ASK], None),
            (&["update", "sp-a", "--set-metadata", "k=v"], None),
            (&["update", "sp-t", "--set-metadata", "k=v"], None),
            (&["update", "sp-a", "--title", "T"], None),
            (&["update", "sp-a", "--body-file", "f"], None),
            (&["update", "sp-t", "--title", "T"], None),
            (&["update", "sp-a", "--status", "open"], None),
            (&["label", "add", "sp-a", "x"], None),
            (&["label", "add", "sp-a", ASK], None),
            (&["label", "remove", "sp-a", ASK], None),
            (&["note", "sp-a", "t"], None),
            (&["note", "sp-a", "--stdin"], Some("world.sh stop")),
            (&["comments", "add", "sp-a", "t"], None),
            (&["create", "T", "--type", "task"], None),
            (&["create", "T", "--labels", "repo:deck"], None),
            (&["create", "T", "--labels", "repo:nowhere"], None),
            (&["create", "Stop it", "--description", "run world.sh stop first"], None),
            (&["create", "Stop it", "--labels", ASK, "--description", "run world.sh stop first"], None),
            (&["create", "Pick", "--labels", ASK], None),
            (&["create", "Pick", "--labels", ASK, "--body-file", "-"], Some(WHOLE_ASK)),
            (&["create", "Pick", "--labels", ASK, "--description", "x", "--body-file", "-"], Some(WHOLE_ASK)),
            (&["create", "Body", "--body-file", "-"], Some("run world.sh stop first")),
            (&["create", "Fix", "--description", "DELETE FROM schema_migrations"], None),
            (&["reopen", "sp-a"], None),
        ];
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for (name, env) in &contexts {
            for row in rows {
                for (w, body) in writes {
                    let args = v(w);
                    let (bdq, content) = (through_bdq(&args, *body, env, row), through_content(&args, *body, env, row));
                    assert_eq!(content, bdq, "{name}, row {row:?}: {w:?} body {body:?}");
                    let kind = match &bdq {
                        Ok(()) => "admitted",
                        Err((1, t)) if t.starts_with("czar-fence: ") => "czar",
                        Err((1, t)) if t.contains("is claimed by dwarf-sp-a") => "claim",
                        Err((1, t)) if t.starts_with("spira: bdq refuses") => "state",
                        Err((1, t)) if t.starts_with("spira: refusing to add") => "ask-label",
                        Err((1, t)) if t.contains("is not in the repo map") => "repo-label",
                        Err((1, t)) if t.contains("DELETE FROM schema_migrations") => "schema-delete",
                        Err((1, t)) if t.contains(ASK) => "ask-shape",
                        Err(other) => panic!("a refusal no guard gives: {other:?}"),
                    };
                    *seen.entry(kind).or_default() += 1;
                }
            }
        }
        let kinds: Vec<&str> = seen.keys().copied().collect();
        assert_eq!(kinds, ["admitted", "ask-label", "ask-shape", "claim", "czar", "repo-label", "schema-delete", "state"], "every guard refuses somewhere in the table: {seen:?}");
        assert_eq!(
            through_content(&v(&["update", "sp-a", "--add-label", "x"]), None, &czar(None), None),
            Err((1, "czar-fence: loop-stalled is shadow — mutation refused (SPIRA_CZAR_STAGE_LOOP_STALLED=act to enable)\n".into()))
        );
        assert_eq!(through_content(&v(&["update", "sp-t", "--add-label", "x"]), None, &czar(None), None), Ok(()), "the trigger bead itself passes");
    }

    #[test]
    fn an_overridden_state_write_is_admitted_and_logged() {
        let env = caller(&[("SPIRA_BDQ_STATE_WRITE", "landing")]);
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: &env, ask_label: &ask, registry: &registry, now: NOW };
        assert_eq!(
            guard(&v(&["close", "sp-a"]), None, &g, &mut Rows(None)).map_err(|a| a.stderr),
            Ok("spira-lc content: STATE WRITE OVERRIDE (close sp-a) — bd's status moved outside the lifecycle machine: landing\n".into())
        );
    }

    #[test]
    fn a_guard_refusal_is_the_answer_and_bd_never_runs() {
        let never_w = |_: &[String], _: Option<&str>| -> Result<String, String> { panic!("bd must not run") };
        let never = |_: &[String]| -> Result<String, crate::composed::ReadError> { panic!("bd must not run") };
        let env = caller(&[("SPIRA_RUNE", "czar"), ("SPIRA_CZAR_CLASS", "deadlock")]);
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: &env, ask_label: &ask, registry: &registry, now: NOW };
        let mut machine = Rows(None);
        let mut gd = |a: &[String], b: Option<&str>| guard(a, b, &g, &mut machine);
        let mut no_after = |_: &[String], _: &str| -> String { panic!("nothing was written") };
        let mut body = || Ok::<String, String>("never read".into());
        let ans = run(&v(&["update", "sp-a", "--add-label", "x"]), &mut body, &mut gd, &mut no_after, &never, &never_w);
        assert_eq!((ans.code, ans.stderr.as_str()), (1, "czar-fence: deadlock is shadow — mutation refused (SPIRA_CZAR_STAGE_DEADLOCK=act to enable)\n"));
        let mut gd = |a: &[String], b: Option<&str>| guard(a, b, &g, &mut Rows(None));
        let mut ask_body = || Ok::<String, String>("no shape at all".into());
        let ans = run(&v(&["create", "Pick", "--labels", ASK, "--body-file", "-"]), &mut ask_body, &mut gd, &mut no_after, &never, &never_w);
        assert_eq!(ans.code, 1, "the request body is judged as the ask's description: {ans:?}");
        let mut refuse_all = |_: &[String], _: Option<&str>| Err(Answer { code: 1, stderr: "no\n".into(), ..Default::default() });
        let ans = run(&v(&["show", "sp-a"]), &mut body, &mut refuse_all, &mut no_after, &|_| Ok("read".into()), &never_w);
        assert_eq!((ans.code, ans.stdout.as_str()), (0, "read"), "a read is never guarded");
    }

    #[test]
    fn a_lifecycle_write_that_fails_after_bd_applied_is_reported_with_bds_answer() {
        let w = |_: &[String], _: Option<&str>| Ok::<_, String>("sp-new\n".to_string());
        let mut admit = |_: &[String], _: Option<&str>| Ok(String::new());
        let mut failed = |_: &[String], out: &str| format!("LIFECYCLE: {} has no row\n", out.trim());
        let mut body = || Err::<String, String>("none".into());
        let ans = run(&v(&["create", "T", "--silent"]), &mut body, &mut admit, &mut failed, &|_| panic!(), &w);
        assert_eq!((ans.code, ans.stdout.as_str(), ans.stderr.as_str()), (0, "sp-new\n", "LIFECYCLE: sp-new has no row\n"), "bd's write stands; the failure is on stderr");
        let bad = |_: &[String], _: Option<&str>| Err::<String, String>("bd: down".into());
        let mut never_after = |_: &[String], _: &str| -> String { panic!("bd did not apply it") };
        let ans = run(&v(&["create", "T", "--silent"]), &mut body, &mut admit, &mut never_after, &|_| panic!(), &bad);
        assert_eq!((ans.code, ans.stderr.as_str()), (1, "bd: down\n"));
    }

    #[test]
    fn run_refuses_before_calling_bd() {
        let never = |_: &[String]| -> Result<String, crate::composed::ReadError> { panic!("bd must not run") };
        let never_w = |_: &[String], _: Option<&str>| -> Result<String, String> { panic!("bd must not run") };
        let mut no_stdin = || Err::<String, String>("nothing was sent".into());
        let mut admit = |_: &[String], _: Option<&str>| Ok(String::new());
        let mut no_after = |_: &[String], _: &str| String::new();
        let ans = run(&v(&["delete", "sp-a"]), &mut no_stdin, &mut admit, &mut no_after, &never, &never_w);
        assert_eq!(ans.code, REFUSED);
        let ans = run(&v(&["note", "sp-a", "--stdin"]), &mut no_stdin, &mut admit, &mut no_after, &never, &never_w);
        assert_eq!((ans.code, ans.stderr.as_str()), (CANNOT_TELL, "spira-lc content: stdin: nothing was sent\n"));
        let ans = run(&v(&["show", "sp-a"]), &mut no_stdin, &mut admit, &mut no_after, &|a| Ok(format!("ran {}", a.join(" "))), &never_w);
        assert_eq!((ans.code, ans.stdout.as_str()), (0, "ran show sp-a"));
    }
}
