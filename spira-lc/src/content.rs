//! `content <bd-subcommand> …` — the one door for bead content: its reads, and the narrow
//! writes a task makes (labels, comments, notes, metadata, a close, a filing), so nothing
//! outside the store forks `bd`. The server forks bd for them with bd's own argv: the content
//! store is out of scope, and this is the one place allowed to run it. State is never served
//! here: the machine's own verbs answer that. Anything outside the allowlist is refused.

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
        ["list" | "show" | "memories" | "state", ..] => Ok(Kind::Read),
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
        _ => Err(format!("`{}` is not a content verb", a.join(" "))),
    }
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

/// A content write: bd's argv, and its stdin when the sub-verb takes the request body.
pub type Write<'a> = &'a dyn Fn(&[String], Option<&str>) -> Result<String, String>;

/// Whether the client sends its stdin as the request body for `args` (after `content`).
pub fn takes_body(args: &[String]) -> bool {
    matches!(check(args), Ok(Kind::Write { body: true }))
}

/// One content call. A read goes to `read`; a write goes to `write`, with the request body
/// when the sub-verb takes one — and refused, before bd runs, when none was sent.
pub fn run(
    args: &[String],
    stdin: &mut dyn FnMut() -> Result<String, String>,
    read: &dyn Fn(&[String]) -> Result<String, String>,
    write: Write<'_>,
) -> Answer {
    if args.is_empty() {
        return Answer { code: CANNOT_TELL, stderr: "usage: spira-lc content <list|show|comments|gate list|memories|state|config get|comments add|label add|label remove|note|close|update|create> …\n".into(), ..Default::default() };
    }
    let kind = match check(args) {
        Ok(k) => k,
        Err(e) => return Answer { code: REFUSED, stderr: format!("spira-lc content: {e}\n"), ..Default::default() },
    };
    let res = match kind {
        Kind::Read => read(args),
        Kind::Write { body: false } => write(args, None),
        Kind::Write { body: true } => match stdin() {
            Ok(b) => write(args, Some(&b)),
            Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc content: stdin: {e}\n"), ..Default::default() },
        },
    };
    match res {
        Ok(out) => Answer { stdout: out, ..Default::default() },
        Err(e) => Answer { code: 1, stderr: format!("{}\n", e.trim_end()), ..Default::default() },
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

    #[test]
    fn run_refuses_before_calling_bd() {
        let never = |_: &[String]| -> Result<String, String> { panic!("bd must not run") };
        let never_w = |_: &[String], _: Option<&str>| -> Result<String, String> { panic!("bd must not run") };
        let mut no_stdin = || Err::<String, String>("nothing was sent".into());
        let ans = run(&v(&["delete", "sp-a"]), &mut no_stdin, &never, &never_w);
        assert_eq!(ans.code, REFUSED);
        let ans = run(&v(&["note", "sp-a", "--stdin"]), &mut no_stdin, &never, &never_w);
        assert_eq!((ans.code, ans.stderr.as_str()), (CANNOT_TELL, "spira-lc content: stdin: nothing was sent\n"));
        let ans = run(&v(&["show", "sp-a"]), &mut no_stdin, &|a| Ok(format!("ran {}", a.join(" "))), &never_w);
        assert_eq!((ans.code, ans.stdout.as_str()), (0, "ran show sp-a"));
    }
}
