//! `close-on-land <id> [sha]` — the landing pass's record of a work bead whose work landed.
//! A bead is acted on only once its builder has handed it on and it has not ended any other
//! way — its lifecycle row SUBMITTED, CERTIFIED, IN_DELIVERY or LANDED (the machine's state,
//! never bd's status); one still with a builder, one dropped or superseded, or one with no row
//! is left alone. For one it acts on, the landing is recorded on the lifecycle record (a
//! `ContentOnBase` event whose proof is `landed:<sha>`) and the landed branch is reaped. bd's
//! status is not written here: reconcile-content closes the bead from the LANDED record.
//! Best-effort throughout and always exits 0, since every caller discards the answer.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

/// Every subprocess here runs under `timeout` (call-deadline). A close or reap cut short is
/// the same miss as one that failed: CHECK 5 and the Hel's sweep stay the backstops.
const CALL_SECS: &str = "5";

/// The bd content close-on-land reads: the labels naming the bead's repo and branch.
pub struct Row {
    pub labels: Vec<String>,
}

pub fn parse_row(text: &str) -> Option<Row> {
    let start = text.find(['[', '{'])?;
    let v: Value = serde_json::from_str(&text[start..]).ok()?;
    let item = v.as_array().map(|a| a.first().cloned().unwrap_or(Value::Null)).unwrap_or(v);
    item.get("id")?;
    let labels = item.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    Some(Row { labels })
}

/// Whether the lifecycle state `state` (None: the machine has no row) is a bead this verb
/// closes for a landed reason: the builder handed it on and it has not been dropped,
/// superseded or finished some other way. LANDED is included — a batch's own `land` may have
/// recorded it already, and bd still needs closing. No row: nothing says it was ever
/// submitted, so it is left alone.
pub fn should_close(state: Option<&str>) -> bool {
    matches!(state, Some("SUBMITTED" | "CERTIFIED" | "IN_DELIVERY" | "LANDED"))
}

/// Whether the landing still needs recording: a LANDED row already carries it.
pub fn should_record(state: Option<&str>) -> bool {
    should_close(state) && state != Some("LANDED")
}

fn label_value<'a>(labels: &'a [String], prefix: &str) -> Option<&'a str> {
    labels.iter().find_map(|l| l.strip_prefix(prefix))
}

/// The `ContentOnBase` proof this verb records: the land ref's sha the caller saw the work at.
pub fn proof(sha: &str) -> String {
    format!("landed:{}", if sha.is_empty() { "unknown" } else { sha })
}

pub fn show_row(id: &str) -> Option<Row> {
    // One source of config (per Ryan 2026-10-05): spira.db/spira.bd from $SPIRA_TOML, never
    // this process's own environment. Empty (either key) means "nothing to show" here, the
    // same best-effort bail this whole module already uses.
    let db = spira_config::process::cfg("SPIRA_DB").ok().filter(|d| !d.is_empty())?;
    let bd = spira_config::process::cfg("SPIRA_BD").ok().filter(|b| !b.is_empty())?;
    let out = Command::new("timeout").arg(CALL_SECS).arg(bd).args(["-C", &db, "show", id, "--json"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| parse_row(&String::from_utf8_lossy(&out.stdout))).flatten()
}

fn branch_exists(root: &str, branch: &str) -> bool {
    Command::new("timeout").args([CALL_SECS, "git", "-C", root, "show-ref", "--verify", "-q", &format!("refs/heads/{branch}")]).status().map(|s| s.success()).unwrap_or(false)
}

fn reap(id: &str, branch: &str, root: &str, why: &str) -> Result<(), String> {
    let mut c = Command::new("timeout");
    c.args([CALL_SECS, "hel", "reap-landed-branch"]);
    if let Ok(f) = std::env::var("SPIRA_STATUS_FILE") {
        c.arg("--status-from").arg(f);
    }
    c.args([id, branch, root, why]).stdin(Stdio::null()).stderr(Stdio::null());
    match c.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
            Err(if t.is_empty() { "unknown".into() } else { t })
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Record the landing on the lifecycle record. A refusal (already terminal, an ask hold) or an
/// unreachable record is reported, never fatal: the bd close and the reap do not depend on it.
pub fn record_landing(id: &str, sha: &str, record: &mut dyn FnMut(&str, &str) -> i32) -> i32 {
    let rc = record(id, &proof(sha));
    if rc != 0 {
        println!("land-close {id}: lifecycle content-on-base {} not applied (rc={rc})", proof(sha));
    }
    rc
}

/// `record(id, proof)` applies the `ContentOnBase` event on the lifecycle record (main.rs
/// hands in the caller-verb path, so the switch and the machine are read exactly as
/// `spira-lc content-on-base` reads them) and returns its exit code.
/// `state(id)` reads the bead's lifecycle state (None: no row, or the machine cannot say).
/// `labels(id)` reads the bead's planning labels (main.rs hands in [`show_row`]).
pub fn run(args: &[String], state: &mut dyn FnMut(&str) -> Option<String>, record: &mut dyn FnMut(&str, &str) -> i32, labels: &mut dyn FnMut(&str) -> Option<Row>) -> i32 {
    let Some(id) = args.first().filter(|s| !s.is_empty()) else {
        eprintln!("spira-lc close-on-land: usage: close-on-land <bead-id> [sha]");
        return 2;
    };
    let sha = args.get(1).map(String::as_str).unwrap_or("");
    let st = state(id);
    if !should_close(st.as_deref()) {
        return 0;
    }
    let Some(row) = labels(id) else { return 0 };
    if should_record(st.as_deref()) {
        record_landing(id, sha, record);
    }
    let shown = if sha.is_empty() { "unknown" } else { sha };
    println!("land-close {id}: landed at {shown}; reconcile-content closes it from the record");

    let (Some(repo_label), Some(branch)) = (label_value(&row.labels, "repo:"), label_value(&row.labels, "branch:")) else { return 0 };
    let Ok(home) = spira_config::resolve::locate_home_for_process() else { return 0 };
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let Some(root) = spira_config::repos::Registry::from_env(env, Path::new(&home)).root(repo_label) else { return 0 };
    if !branch_exists(&root, branch) {
        return 0;
    }
    match reap(id, branch, &root, &format!("landed at {shown}")) {
        Ok(()) => println!("land-close {id}: reaped branch {branch}"),
        Err(e) => println!("land-close {id}: branch {branch} not reaped: {e} — left for the Hel"),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sp-mve9i: whether to close is the lifecycle row's call, never bd's status or the
    /// retired submitted label.
    #[test]
    fn only_a_handed_on_bead_that_did_not_end_otherwise_is_closed() {
        for s in ["SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"] {
            assert!(should_close(Some(s)), "{s}");
        }
        for s in ["READY", "WORKING", "REWORK", "DROPPED", "SUPERSEDED", "DONE"] {
            assert!(!should_close(Some(s)), "{s}");
        }
        assert!(!should_close(None), "no row: nothing says it was submitted");
        assert!(should_record(Some("CERTIFIED")) && !should_record(Some("LANDED")), "a LANDED row already carries the landing");
    }

    fn labelled(_: &str) -> Option<Row> {
        Some(Row { labels: vec!["repo:nowhere".into()] })
    }

    /// The state is read before bd is: a bead the machine says is still WORKING is never
    /// treated as landed, whatever bd shows.
    #[test]
    fn a_working_bead_is_left_alone_and_its_labels_are_not_read() {
        let (mut recorded, mut read) = (false, false);
        let rc = run(&["sp-w".to_string()], &mut |_| Some("WORKING".to_string()), &mut |_, _| {
            recorded = true;
            0
        }, &mut |_| {
            read = true;
            None
        });
        assert_eq!(rc, 0);
        assert!(!recorded && !read, "a WORKING bead was treated as landed");
    }

    #[test]
    fn close_on_land_of_a_landed_record_writes_no_bd_status_and_reconcile_content_closes_it() {
        use crate::callers::{self, world::World, Machine};
        let w = World::new();
        w.file("sp-l", "task", None, &[]);
        w.ok(&["create-bead", "sp-l", "--submitted-tip", "abc"]);
        let mut m = &w;
        let landed = callers::run("content-on-base", &["sp-l".to_string(), "landed:abc".to_string()], &mut m);
        assert_eq!(landed.code, 0, "{landed:?}");
        let mut state = |id: &str| {
            let (rc, out) = (&w).call(&["show".to_string(), id.to_string()]);
            (rc == 0).then(|| serde_json::from_str::<Value>(&out).unwrap()["bead"]["state"].as_str().unwrap().to_string())
        };
        assert_eq!(state("sp-l").as_deref(), Some("LANDED"));
        let rc = run(&["sp-l".to_string(), "abc".to_string()], &mut state, &mut |_, _| panic!("a LANDED record already carries the landing"), &mut labelled);
        assert_eq!(rc, 0);
        assert!(w.writes.borrow().is_empty(), "close-on-land wrote bd status: {:?}", w.writes.borrow());
        let (mut m, mut b) = (&w, &w);
        let a = callers::reconcile_content(&["--apply".to_string()], &mut m, &mut b);
        assert_eq!(a.code, 0, "{a:?}");
        assert_eq!(*w.writes.borrow(), ["close sp-l"]);
    }

    #[test]
    fn a_submitted_bead_whose_work_landed_has_the_landing_recorded_and_bd_left_alone() {
        let mut seen = Vec::new();
        let rc = run(&["sp-s".to_string(), "abc".to_string()], &mut |_| Some("SUBMITTED".to_string()), &mut |id, p| {
            seen.push((id.to_string(), p.to_string()));
            0
        }, &mut labelled);
        assert_eq!(rc, 0);
        assert_eq!(seen, vec![("sp-s".to_string(), "landed:abc".to_string())]);
    }

    #[test]
    fn parses_bd_json_after_a_warning_line() {
        let r = parse_row("warning: x\n[{\"id\":\"sp-a\",\"status\":\"open\",\"labels\":[\"a\",\"repo:r\"]}]").unwrap();
        assert_eq!(r.labels, vec!["a", "repo:r"]);
        assert!(parse_row("no json").is_none());
    }

    #[test]
    fn a_refused_landing_record_is_reported_back() {
        assert_eq!(record_landing("sp-x", "abc", &mut |_, _| 3), 3, "a refusal is reported back, not swallowed");
        assert_eq!(proof(""), "landed:unknown");
    }

    #[test]
    fn the_retired_landstate_writer_is_not_called() {
        // Guard (sp-2c1n0): the ledger and `landing-pass mark` are deleted; this verb records
        // LANDED through the lifecycle record alone.
        let src = include_str!("close_on_land.rs");
        let needle = ["landing-pass", "\"mark\""].join("\", ");
        assert!(!src.contains(&needle), "close-on-land shells to the retired landstate writer again");
    }
}
