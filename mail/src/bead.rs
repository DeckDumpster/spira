//! Everything mail.sh asked `bd` to do: render a cited bead's details at the top of a
//! message, open a tracking decision bead for a question/decision send, close the tracking
//! bead (and note the work it cites) on `sendmail`, and the two bead-store queries `tidy`
//! and `sweep-dismissed` make. One seam (`Bd`) so every one of these is a unit test against
//! a fake, never a real store (matching aeon's/queue's own ports/real split).

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

#[derive(Debug, Default, Clone)]
pub struct BdOut {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl BdOut {
    pub fn ok(stdout: impl Into<String>) -> BdOut {
        BdOut { code: 0, stdout: stdout.into(), stderr: String::new() }
    }
    pub fn fail(code: i32, stderr: impl Into<String>) -> BdOut {
        BdOut { code, stdout: String::new(), stderr: stderr.into() }
    }
    /// stdout if present, else stderr — mail.sh's own `2>&1` capture of `bd close`'s output.
    pub fn combined(&self) -> String {
        if !self.stderr.trim().is_empty() {
            self.stderr.clone()
        } else {
            self.stdout.clone()
        }
    }
}

pub trait Bd {
    fn run(&self, args: &[String], stdin: Option<&str>) -> BdOut;
}

/// The real `bd` binary, `-C <db>` prefixed. Refuses (matching mail.sh's own
/// `[ -n "${SPIRA_DB:-}" ]` guards at every call site) rather than let `bd` fall through to
/// auto-discovering the operator's real store when `SPIRA_DB` is unset.
pub struct BdCli {
    pub bin: String,
    pub db: String,
}

impl Bd for BdCli {
    fn run(&self, args: &[String], stdin: Option<&str>) -> BdOut {
        if self.db.is_empty() {
            return BdOut::fail(1, "SPIRA_DB is empty/unset");
        }
        let mut cmd = Command::new(&self.bin);
        cmd.arg("-C").arg(&self.db);
        cmd.args(args);
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return BdOut::fail(127, format!("spawn failed: {e}")),
        };
        if let Some(s) = stdin {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(s.as_bytes());
            }
        }
        match child.wait_with_output() {
            Ok(o) => BdOut {
                code: o.status.code().unwrap_or(1),
                stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            },
            Err(e) => BdOut::fail(127, format!("wait failed: {e}")),
        }
    }
}

fn a(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// `_render_bead_block`: title, status, priority, type, repo, branch, and the close reason
/// or last note (truncated to 300 chars). `"unresolved: <id>"` if the store can't resolve it
/// — never a send failure.
pub fn render_bead_block(bd: &dyn Bd, db_configured: bool, id: &str) -> String {
    if !db_configured {
        return format!("unresolved: {id}");
    }
    let out = bd.run(&a(&["show", id, "--json"]), None);
    if out.stdout.trim().is_empty() {
        return format!("unresolved: {id}");
    }
    render_from_json(&out.stdout).unwrap_or_else(|| format!("unresolved: {id}"))
}

fn first_object(json_text: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(json_text).ok()?;
    match v {
        Value::Array(mut arr) => arr.drain(..).next(),
        obj @ Value::Object(_) => Some(obj),
        _ => None,
    }
}

fn render_from_json(json_text: &str) -> Option<String> {
    let d = first_object(json_text)?;
    if d.get("error").is_some() {
        return None;
    }
    let real_id = d.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty())?;
    let title = d.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let status = d.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let priority = number_or_string(d.get("priority"));
    let itype = d.get("issue_type").and_then(|v| v.as_str()).unwrap_or("");
    let labels: Vec<String> = d
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let repo = labels.iter().find_map(|l| l.strip_prefix("repo:")).unwrap_or("");
    let branch = labels.iter().find_map(|l| l.strip_prefix("branch:")).unwrap_or("");

    let mut reason = d.get("close_reason").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if reason.is_empty() {
        let notes = d.get("notes");
        let last: Option<String> = match notes {
            Some(Value::String(s)) => s.lines().filter(|l| !l.trim().is_empty()).last().map(str::to_string),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|n| match n {
                    Value::Object(o) => o.get("text").and_then(|t| t.as_str()).map(str::to_string),
                    Value::String(s) => Some(s.clone()),
                    other => Some(other.to_string()),
                })
                .last(),
            _ => None,
        };
        if let Some(l) = last {
            reason = l.trim().to_string();
        }
    }
    reason = reason.lines().next().unwrap_or("").trim().to_string();
    if reason.chars().count() > 300 {
        reason = format!("{}…", reason.chars().take(300).collect::<String>());
    }

    let mut lines = vec![format!("{real_id}: {title}")];
    let mut line2 = format!("Status: {status}");
    if let Some(p) = priority {
        line2.push_str(&format!("  ·  Priority: P{p}"));
    }
    if !itype.is_empty() {
        line2.push_str(&format!("  ·  Type: {itype}"));
    }
    lines.push(line2);
    let mut meta = Vec::new();
    if !repo.is_empty() {
        meta.push(format!("Repo: {repo}"));
    }
    if !branch.is_empty() {
        meta.push(format!("Branch: {branch}"));
    }
    if !meta.is_empty() {
        lines.push(meta.join("  ·  "));
    }
    if !reason.is_empty() {
        lines.push(String::new());
        lines.push(reason);
    }
    Some(lines.join("\n"))
}

fn number_or_string(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Some(i.to_string())
            } else {
                Some(n.to_string())
            }
        }
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// `_suit_reason`: a suit verdict (uphold/retire/amend) closes with its own word; any other
/// kind, or an unrecognised suit paragraph, closes with the paragraph unchanged.
pub fn suit_reason(kind: &str, first_para: &str) -> String {
    if kind != "suit" {
        return first_para.to_string();
    }
    let lc = first_para.to_lowercase();
    if lc.starts_with("uphold") {
        return "upheld".to_string();
    }
    if lc.starts_with("retire") {
        return "retired".to_string();
    }
    if lc.starts_with("amend") {
        let rest = &first_para[5.min(first_para.len())..];
        let trimmed = rest.trim_start_matches([':', ' ']);
        return if trimmed.is_empty() { "amended".to_string() } else { trimmed.to_string() };
    }
    first_para.to_string()
}

/// Creates the tracking decision bead a question/decision send wires itself to. `None` if
/// the store is unconfigured or the create failed (mail.sh: `dec_bead=""` either way — the
/// send still succeeds, just without a tracking bead).
pub fn create_tracking_bead(bd: &dyn Bd, db_configured: bool, subject: &str, body: &str, ask_label: &str) -> Option<String> {
    if !db_configured {
        return None;
    }
    let labels = format!("{ask_label},overseer");
    let out = bd.run(&a(&["create", subject, "-l", &labels, "--type", "decision", "--body-file", "-", "--silent"]), Some(body));
    let id = out.stdout.trim();
    if out.code == 0 && !id.is_empty() {
        Some(id.to_string())
    } else {
        None
    }
}

pub fn dep_add(bd: &dyn Bd, from: &str, to: &str, dep_type: &str) -> bool {
    bd.run(&a(&["dep", "add", from, to, "--type", dep_type]), None).code == 0
}

fn ids_from_json_array(text: &str) -> Vec<String> {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = match v {
        Value::Array(a) => a,
        obj @ Value::Object(_) => vec![obj],
        _ => return Vec::new(),
    };
    arr.iter().filter_map(|x| x.get("id").and_then(|i| i.as_str()).map(str::to_string)).filter(|s| !s.is_empty()).collect()
}

pub fn dep_list_up_ids(bd: &dyn Bd, db_configured: bool, bead: &str) -> Vec<String> {
    if !db_configured {
        return Vec::new();
    }
    let out = bd.run(&a(&["dep", "list", bead, "--direction=up", "--json"]), None);
    ids_from_json_array(&out.stdout)
}

pub fn close(bd: &dyn Bd, id: &str, reason: &str) -> Result<(), String> {
    let out = bd.run(&a(&["close", id, "--reason-file", "-"]), Some(reason));
    if out.code == 0 {
        Ok(())
    } else {
        Err(out.combined())
    }
}

pub fn note(bd: &dyn Bd, id: &str, text: &str) -> Result<(), String> {
    let out = bd.run(&a(&["note", id, text]), None);
    if out.code == 0 {
        Ok(())
    } else {
        Err(out.combined())
    }
}

/// `_sendmail_close_bead`: closes the tracking bead with the suit-mapped reason, and (for a
/// question/decision) notes every upstream work bead with the verdict. A close failure is
/// fatal to the caller; a note failure is only printed (mail.sh: `printf ... >&2`, loop
/// continues) — reported here as a side channel so callers can surface it the same way.
pub fn sendmail_close_bead(bd: &dyn Bd, db_configured: bool, bead: &str, kind: &str, first_para: &str) -> Result<Vec<String>, String> {
    if !db_configured {
        return Ok(Vec::new());
    }
    let reason = suit_reason(kind, first_para);
    close(bd, bead, &reason).map_err(|e| format!("mail.sh: bead close failed for {bead}: {e}"))?;
    let mut warnings = Vec::new();
    if kind == "question" || kind == "decision" {
        for wid in dep_list_up_ids(bd, db_configured, bead) {
            if let Err(e) = note(bd, &wid, &format!("Operator verdict on decision bead {bead}: {reason}")) {
                warnings.push(format!("mail.sh: note failed for {wid}: {e}"));
            }
        }
    }
    Ok(warnings)
}

pub fn bead_status(bd: &dyn Bd, db_configured: bool, id: &str) -> Option<String> {
    if !db_configured {
        return None;
    }
    let out = bd.run(&a(&["show", id, "--json"]), None);
    let d = first_object(&out.stdout)?;
    d.get("status").and_then(|v| v.as_str()).map(str::to_string)
}

pub fn bead_replied(bd: &dyn Bd, db_configured: bool, id: &str, actor: &str) -> bool {
    if !db_configured {
        return false;
    }
    let out = bd.run(&a(&["history", id, "--events", "--json"]), None);
    let v: Value = match serde_json::from_str(&out.stdout) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let arr = match v {
        Value::Array(a) => a,
        obj @ Value::Object(_) => vec![obj],
        _ => return false,
    };
    arr.iter().any(|e| e.get("actor").and_then(|a| a.as_str()) == Some(actor))
}

/// The open ask-labelled bead ids `tidy` keeps mail for. `Err` on a query the store could
/// not answer (law-a-control-that-cannot-check-must-refuse) — distinct from a real `[]`.
pub fn open_ask_ids(bd: &dyn Bd, ask_label: &str) -> Result<Vec<String>, String> {
    let out = bd.run(
        &a(&["list", "--status", "open,in_progress,blocked,deferred", "--label", ask_label, "--limit", "0", "--brief", "--json"]),
        None,
    );
    match serde_json::from_str::<Value>(&out.stdout) {
        Ok(v) => {
            let arr = match v {
                Value::Array(a) => a,
                obj @ Value::Object(_) => vec![obj],
                _ => Vec::new(),
            };
            Ok(arr.iter().filter_map(|x| x.get("id").and_then(|i| i.as_str()).map(str::to_string)).filter(|s| !s.is_empty()).collect())
        }
        Err(_) => Err("bead store query failed — refusing to move any mail".to_string()),
    }
}

/// The probe an empty [`open_ask_ids`] result needs before it is trusted (an empty result
/// from a broken query would archive every live ask).
pub fn probe_store(bd: &dyn Bd) -> bool {
    bd.run(&a(&["list", "--limit", "1", "--brief", "--json"]), None).code == 0
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;

    /// A scripted `Bd`: each call is matched against `expect` in order; panics (a loud test
    /// failure) on an unexpected call, so a test's call list is also an assertion on what
    /// the code under test actually asked the store.
    pub struct FakeBd {
        pub calls: RefCell<Vec<(Vec<String>, Option<String>)>>,
        pub responses: RefCell<Vec<BdOut>>,
    }

    impl FakeBd {
        pub fn new(responses: Vec<BdOut>) -> FakeBd {
            FakeBd { calls: RefCell::new(Vec::new()), responses: RefCell::new(responses) }
        }
        pub fn calls(&self) -> Vec<(Vec<String>, Option<String>)> {
            self.calls.borrow().clone()
        }
    }

    impl Bd for FakeBd {
        fn run(&self, args: &[String], stdin: Option<&str>) -> BdOut {
            self.calls.borrow_mut().push((args.to_vec(), stdin.map(str::to_string)));
            let mut r = self.responses.borrow_mut();
            if r.is_empty() {
                BdOut::fail(1, "FakeBd: no more scripted responses")
            } else {
                r.remove(0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeBd;
    use super::*;

    #[test]
    fn unresolved_when_store_not_configured() {
        let bd = FakeBd::new(vec![]);
        assert_eq!(render_bead_block(&bd, false, "sp-zzz99"), "unresolved: sp-zzz99");
        assert!(bd.calls().is_empty());
    }

    #[test]
    fn unresolved_when_store_returns_nothing() {
        let bd = FakeBd::new(vec![BdOut::ok("")]);
        assert_eq!(render_bead_block(&bd, true, "sp-zzz99"), "unresolved: sp-zzz99");
    }

    #[test]
    fn renders_title_status_and_priority() {
        let json = r#"{"id":"sp-titl01","title":"a well-described feature bead with a clear title","status":"open","issue_type":"task","priority":3,"labels":["spira","repo:fixture-repo"]}"#;
        let bd = FakeBd::new(vec![BdOut::ok(json)]);
        let block = render_bead_block(&bd, true, "sp-titl01");
        assert_eq!(block.lines().next().unwrap(), "sp-titl01: a well-described feature bead with a clear title");
        assert!(block.contains("Status: open"), "{block}");
        assert!(block.contains("Priority: P3"), "{block}");
        assert!(block.contains("Repo: fixture-repo"), "{block}");
    }

    #[test]
    fn renders_close_reason_truncated_to_300_chars() {
        let long = "x".repeat(400);
        let json = format!(r#"{{"id":"sp-a","title":"t","status":"closed","close_reason":"{long}"}}"#);
        let bd = FakeBd::new(vec![BdOut::ok(json)]);
        let block = render_bead_block(&bd, true, "sp-a");
        let reason_line = block.lines().last().unwrap();
        assert_eq!(reason_line.chars().count(), 301); // 300 chars + the ellipsis
        assert!(reason_line.ends_with('…'));
    }

    #[test]
    fn falls_back_to_the_last_note_when_no_close_reason() {
        let json = r#"{"id":"sp-a","title":"t","status":"open","notes":["first note","second note  "]}"#;
        let bd = FakeBd::new(vec![BdOut::ok(json)]);
        let block = render_bead_block(&bd, true, "sp-a");
        assert!(block.ends_with("second note"), "{block}");
    }

    #[test]
    fn suit_reason_maps_uphold_retire_amend() {
        assert_eq!(suit_reason("suit", "Uphold the finding."), "upheld");
        assert_eq!(suit_reason("suit", "Retire: superseded."), "retired");
        assert_eq!(suit_reason("suit", "Amend: narrower scope"), "narrower scope");
        assert_eq!(suit_reason("suit", "amend"), "amended");
        assert_eq!(suit_reason("suit", "Something else entirely"), "Something else entirely");
        assert_eq!(suit_reason("question", "Uphold the finding."), "Uphold the finding.");
    }

    #[test]
    fn create_tracking_bead_returns_none_when_store_unconfigured() {
        let bd = FakeBd::new(vec![]);
        assert_eq!(create_tracking_bead(&bd, false, "subj", "body", "needs-operator"), None);
    }

    #[test]
    fn create_tracking_bead_returns_the_new_id() {
        let bd = FakeBd::new(vec![BdOut::ok("sp-newid1\n")]);
        let id = create_tracking_bead(&bd, true, "subj", "body", "needs-operator").unwrap();
        assert_eq!(id, "sp-newid1");
        let calls = bd.calls();
        assert_eq!(calls[0].1.as_deref(), Some("body"));
        assert!(calls[0].0.contains(&"needs-operator,overseer".to_string()));
    }

    #[test]
    fn sendmail_close_bead_is_a_noop_without_a_store() {
        let bd = FakeBd::new(vec![]);
        assert_eq!(sendmail_close_bead(&bd, false, "sp-a", "question", "para").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn sendmail_close_bead_notes_every_upstream_work_bead() {
        let bd = FakeBd::new(vec![
            BdOut::ok(""),                                   // close
            BdOut::ok(r#"[{"id":"sp-work1"},{"id":"sp-work2"}]"#), // dep list up
            BdOut::ok(""),                                   // note work1
            BdOut::ok(""),                                   // note work2
        ]);
        let warnings = sendmail_close_bead(&bd, true, "sp-dec1", "decision", "Approve it.").unwrap();
        assert!(warnings.is_empty());
        let calls = bd.calls();
        assert_eq!(calls[0].0[0], "close");
        assert_eq!(calls[2].0, vec!["note", "sp-work1", "Operator verdict on decision bead sp-dec1: Approve it."]);
        assert_eq!(calls[3].0, vec!["note", "sp-work2", "Operator verdict on decision bead sp-dec1: Approve it."]);
    }

    #[test]
    fn sendmail_close_bead_propagates_a_close_failure() {
        let bd = FakeBd::new(vec![BdOut::fail(1, "no such bead")]);
        let err = sendmail_close_bead(&bd, true, "sp-a", "note", "para").unwrap_err();
        assert!(err.contains("sp-a"), "{err}");
    }

    #[test]
    fn open_ask_ids_fails_closed_on_bad_json() {
        let bd = FakeBd::new(vec![BdOut::fail(1, "boom")]);
        assert!(open_ask_ids(&bd, "needs-operator").is_err());
    }

    #[test]
    fn open_ask_ids_parses_a_real_list() {
        let bd = FakeBd::new(vec![BdOut::ok(r#"[{"id":"sp-open1"}]"#)]);
        assert_eq!(open_ask_ids(&bd, "needs-operator").unwrap(), vec!["sp-open1".to_string()]);
    }
}
