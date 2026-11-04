//! `content <bd-subcommand> …` — the one door for the cockpit's bead-content reads and its
//! label/comment writes, so nothing under `cockpit/` or `cockpit-collect/` invokes `bd`.
//! State is never served here: the machine's own verbs answer that. Anything outside the
//! allowlist is refused.

use crate::callers::Answer;

const REFUSED: i32 = 3;
const CANNOT_TELL: i32 = 2;

pub fn check(args: &[String]) -> Result<(), String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["list" | "show" | "memories" | "state", ..] => Ok(()),
        ["gate", "list", ..] => Ok(()),
        // The planning store's id prefix, which a caller filing beads stamps its ids with.
        ["config", "get", key] if !key.starts_with('-') => Ok(()),
        ["comments", "add", id, _text] if !id.starts_with('-') => Ok(()),
        ["comments", sub, ..] if *sub != "add" => Ok(()),
        ["update", id, flags @ ..] if !id.starts_with('-') && !flags.is_empty() => labels_only(flags),
        _ => Err(format!("`{}` is not a content verb", a.join(" "))),
    }
}

fn labels_only(flags: &[&str]) -> Result<(), String> {
    let mut it = flags.iter();
    while let Some(f) = it.next() {
        match (*f, it.next()) {
            ("--add-label" | "--remove-label", Some(l)) if !l.is_empty() => {}
            _ => return Err(format!("update takes only --add-label/--remove-label <label>, not `{f}`")),
        }
    }
    Ok(())
}

pub fn run(args: &[String], bd: &dyn Fn(&[String]) -> Result<String, String>) -> Answer {
    if args.is_empty() {
        return Answer { code: CANNOT_TELL, stderr: "usage: spira-lc content <list|show|comments|gate list|memories|state|config get|update|comments add> …\n".into(), ..Default::default() };
    }
    if let Err(e) = check(args) {
        return Answer { code: REFUSED, stderr: format!("spira-lc content: {e}\n"), ..Default::default() };
    }
    match bd(args) {
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
    fn reads_and_label_comment_writes_pass() {
        for ok in [
            &["list", "--all", "--json"][..],
            &["show", "sp-a", "--json"],
            &["comments", "sp-a", "--json"],
            &["comments", "add", "sp-a", "text"],
            &["gate", "list", "--json"],
            &["update", "sp-a", "--add-label", "x", "--remove-label", "y"],
            &["state", "sp-a", "rune"],
            &["memories", "--json"],
        ] {
            assert!(check(&v(ok)).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn every_other_write_is_refused() {
        for bad in [
            &["close", "sp-a"][..],
            &["close", "sp-a", "--reason", "r"],
            &["close", "sp-a", "--reason", "r", "--force"],
            &["update", "sp-a", "--status", "open"],
            &["update", "sp-a", "--add-label"],
            &["update", "sp-a"],
            &["create", "t"],
            &["comments", "add", "sp-a"],
            &["gate", "resolve", "x"],
            &[][..],
        ] {
            assert!(check(&v(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn run_refuses_before_calling_bd() {
        let ans = run(&v(&["close", "sp-a"]), &|_| panic!("bd must not run"));
        assert_eq!(ans.code, REFUSED);
        let ans = run(&v(&["show", "sp-a"]), &|a| Ok(format!("ran {}", a.join(" "))));
        assert_eq!((ans.code, ans.stdout.as_str()), (0, "ran show sp-a"));
    }
}
