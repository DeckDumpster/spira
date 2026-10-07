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
    /// Close `id` through the lifecycle machine (`spira-lc close`, sp-3fue0j) — never bd.
    fn close(&self, id: &str, reason: &str) -> BdOut;
}

/// The real `bd` binary, `-C <db>` prefixed. Refuses (matching mail.sh's own
/// `[ -n "${SPIRA_DB:-}" ]` guards at every call site) rather than let `bd` fall through to
/// auto-discovering the operator's real store when `SPIRA_DB` is unset.
///
/// `conn_retries` (>= 1) retries a call whose stderr names a dropped pooled connection —
/// `bead::bdq::should_retry`'s own rule, the one every other `bd` caller in this workspace
/// (aeon, cockpit-collect, incident, sentinel) already applies. A store freshly started by
/// install (dolt-beads.service listening, but the pool's first connection already stale) is
/// not "unreachable" — it is the one failure mode retrying past is sound for; every other
/// failure (a real auth/schema/network refusal) still surfaces on the first try, unretried.
pub struct BdCli {
    pub bin: String,
    pub db: String,
    pub conn_retries: u32,
}

impl Bd for BdCli {
    fn close(&self, id: &str, reason: &str) -> BdOut {
        match spira_config::lifecycle_row::close(id, reason, "mail", None) {
            Ok(()) => BdOut::ok(""),
            Err(e) => BdOut::fail(1, e),
        }
    }
    fn run(&self, args: &[String], stdin: Option<&str>) -> BdOut {
        if self.db.is_empty() {
            return BdOut::fail(1, "SPIRA_DB is empty/unset");
        }
        let tries = self.conn_retries.max(1);
        let mut last = BdOut::default();
        for t in 1..=tries {
            last = self.run_once(args, stdin);
            if !bead::bdq::should_retry(last.code, t, tries, last.stderr.contains("invalid connection")) {
                break;
            }
        }
        last
    }
}

impl BdCli {
    fn run_once(&self, args: &[String], stdin: Option<&str>) -> BdOut {
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
///
/// `work_bead` (empty for none) is the bead the question is about; it rides the tracking bead
/// as a `work-bead:<id>` label, so a path that answers or closes the ASK BEAD rather than
/// replying to the mail — the cockpit pane's verdict, `resolve`, `verify-asks` — can still
/// find the work bead whose `ask` hold the answer lifts (sp-v62vn follow-up).
pub fn create_tracking_bead(bd: &dyn Bd, db_configured: bool, subject: &str, body: &str, ask_label: &str, work_bead: &str) -> Option<String> {
    if !db_configured {
        return None;
    }
    let mut labels = format!("{ask_label},overseer");
    if !work_bead.is_empty() {
        labels.push_str(&format!(",{WORK_BEAD_LABEL}{work_bead}"));
    }
    let out = bd.run(&a(&["create", subject, "-l", &labels, "--type", "decision", "--body-file", "-", "--silent"]), Some(body));
    let id = out.stdout.trim();
    if out.code == 0 && !id.is_empty() {
        if let Err(e) = spira_config::lifecycle_row::after_create("mail", &out.stdout) {
            eprintln!("mail: LIFECYCLE: row not written after create: {e}; the new bead is rowless and cannot be claimed");
        }
        Some(id.to_string())
    } else {
        None
    }
}

/// The label prefix naming an ask bead's work bead (`work-bead:sp-xxxx`).
pub const WORK_BEAD_LABEL: &str = "work-bead:";

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
    let out = bd.close(id, reason);
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

/// The last `max_bytes` of `stderr`, trimmed and UTF-8 safe (rounds forward to the next
/// char boundary rather than splitting one). Empty when `stderr` is empty.
fn stderr_tail(stderr: &str, max_bytes: usize) -> String {
    let trimmed = stderr.trim();
    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - max_bytes;
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

/// `"(bd exit <code>): <last ~300 bytes of stderr>"`, or `"(bd exit <code>, no stderr)"`
/// when bd left nothing on stderr — the detail every refusal rooted in a failed `bd` call
/// appends to its own message. Before this, a refusal named only "bead store query failed",
/// swallowing bd's own stderr; acceptance run 37217149529's forensics carry no "invalid
/// connection" anywhere because of exactly that — the dropped-pooled-connection cause sp-yrxoc
/// fixed is plausible but was never confirmed from that log. This makes the next failure name
/// itself: a dropped connection reads differently from a real auth or schema refusal. bd's
/// own stderr never carries a credential (the store's password lives in a file bd never
/// echoes); this never touches the process environment, so none can leak through it either.
pub fn bd_failure_detail(out: &BdOut) -> String {
    let tail = stderr_tail(&out.stderr, 300);
    if tail.is_empty() {
        format!("(bd exit {}, no stderr)", out.code)
    } else {
        format!("(bd exit {}): {tail}", out.code)
    }
}

/// The open ask-labelled bead ids `tidy` keeps mail for. `Err` on a query the store could
/// not answer (law-a-control-that-cannot-check-must-refuse) — distinct from a real `[]`.
pub fn open_ask_ids(bd: &dyn Bd, ask_label: &str) -> Result<Vec<String>, String> {
    // An ask is not a work bead: bd's status is its whole state (spira_config::nonwork, sp-mve9i).
    let [flag, live] = spira_config::nonwork::status_args(spira_config::nonwork::Kind::Ask, spira_config::nonwork::Which::Live);
    let out = bd.run(&a(&["list", &flag, &live, "--label", ask_label, "--limit", "0", "--brief", "--json"]), None);
    match serde_json::from_str::<Value>(&out.stdout) {
        Ok(v) => {
            let arr = match v {
                Value::Array(a) => a,
                obj @ Value::Object(_) => vec![obj],
                _ => Vec::new(),
            };
            Ok(arr.iter().filter_map(|x| x.get("id").and_then(|i| i.as_str()).map(str::to_string)).filter(|s| !s.is_empty()).collect())
        }
        Err(_) => Err(format!("bead store query failed {} — refusing to move any mail", bd_failure_detail(&out))),
    }
}

/// The probe an empty [`open_ask_ids`] result needs before it is trusted (an empty result
/// from a broken query would archive every live ask). `Err` carries the failing call's exit
/// code and stderr so the caller's refusal can name what actually went wrong.
pub fn probe_store(bd: &dyn Bd) -> Result<(), BdOut> {
    let out = bd.run(&a(&["list", "--limit", "1", "--brief", "--json"]), None);
    if out.code == 0 {
        Ok(())
    } else {
        Err(out)
    }
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
        // Recorded as the argv the real close used to be, so the callers' tests read the same.
        fn close(&self, id: &str, reason: &str) -> BdOut {
            self.run(&[String::from("close"), id.to_string(), "--reason-file".into(), "-".into()], Some(reason))
        }
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
    use std::path::Path;

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
        assert_eq!(create_tracking_bead(&bd, false, "subj", "body", "needs-operator", ""), None); // literal-ok: test fixture
    }

    #[test]
    fn create_tracking_bead_returns_the_new_id() {
        let bd = FakeBd::new(vec![BdOut::ok("sp-newid1\n")]);
        let id = create_tracking_bead(&bd, true, "subj", "body", "needs-operator", "").unwrap(); // literal-ok: test fixture
        assert_eq!(id, "sp-newid1");
        let calls = bd.calls();
        assert_eq!(calls[0].1.as_deref(), Some("body"));
        assert!(calls[0].0.contains(&"needs-operator,overseer".to_string())); // literal-ok: test fixture
    }

    /// The ask bead names the work bead it is about, so a path that closes the ask bead
    /// itself (the pane's verdict, `resolve`) can lift that bead's `ask` hold.
    #[test]
    fn a_tracking_bead_for_a_work_bead_carries_its_work_bead_label() {
        let bd = FakeBd::new(vec![BdOut::ok("sp-newid1\n")]);
        create_tracking_bead(&bd, true, "subj", "body", "needs-operator", "sp-work1").unwrap(); // literal-ok: test fixture
        assert!(bd.calls()[0].0.contains(&"needs-operator,overseer,work-bead:sp-work1".to_string()), "{:?}", bd.calls()); // literal-ok: test fixture
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
        assert!(open_ask_ids(&bd, "needs-operator").is_err()); // literal-ok: test fixture
    }

    #[test]
    fn open_ask_ids_parses_a_real_list() {
        let bd = FakeBd::new(vec![BdOut::ok(r#"[{"id":"sp-open1"}]"#)]);
        assert_eq!(open_ask_ids(&bd, "needs-operator").unwrap(), vec!["sp-open1".to_string()]); // literal-ok: test fixture
        // Every status but closed: an ask's bd status is its state (sp-mve9i, nonwork::Which::Live).
        assert_eq!(bd.calls()[0].0[..3], ["list", "--status", "open,in_progress,blocked,deferred"]);
    }

    #[test]
    fn open_ask_ids_refusal_names_bds_exit_code_and_stderr() {
        let bd = FakeBd::new(vec![BdOut::fail(2, "Error 1105: invalid connection to dolt-beads on 127.0.0.1:3307")]);
        let err = open_ask_ids(&bd, "needs-operator").unwrap_err(); // literal-ok: test fixture
        assert!(err.contains("bd exit 2"), "{err}");
        assert!(err.contains("invalid connection to dolt-beads on 127.0.0.1:3307"), "{err}");
        assert!(err.contains("refusing to move any mail"), "{err}");
    }

    #[test]
    fn bd_failure_detail_says_so_when_stderr_is_empty() {
        let out = BdOut::fail(127, "");
        assert_eq!(bd_failure_detail(&out), "(bd exit 127, no stderr)");
    }

    #[test]
    fn bd_failure_detail_keeps_only_the_last_300_bytes() {
        let long = "x".repeat(400);
        let out = BdOut::fail(1, long.as_str());
        let detail = bd_failure_detail(&out);
        assert!(detail.starts_with("(bd exit 1): …"), "{detail}");
        // 300 bytes of 'x' plus the leading ellipsis character, plus the "(bd exit 1): " prefix.
        assert!(detail.ends_with(&"x".repeat(300)), "{detail}");
    }

    // -- BdCli's own retry on a dropped pooled connection ------------------------------------
    //
    // A store install just started (dolt-beads.service listening, but the first pooled
    // connection already gone stale) fails `bd`'s first call with "invalid connection" on
    // stderr — not a genuinely unreachable store, the one case every other `bd` caller in
    // this workspace (aeon::bd::BdCli, cockpit-collect, incident, sentinel) already retries
    // past via `bead::bdq::should_retry`. Before this fix, mail's own `BdCli` had no retry
    // at all, so `open_ask_ids` surfaced that single transient hiccup as "bead store query
    // failed — refusing to move any mail" on a store that was, in truth, healthy.

    /// A scripted fake `bd`: fails once with "invalid connection" on stderr, then succeeds —
    /// a stand-in for a store whose pool just dropped its first connection. `calls.log`
    /// (one line per invocation) is the test's own call count, independent of the script's
    /// internal state file.
    fn flaky_bd_script(dir: &Path) -> std::path::PathBuf {
        let script = dir.join("fake-bd.sh");
        let state = dir.join("state");
        let log = dir.join("calls.log");
        let body = format!(
            "#!/bin/sh\necho called >> {log}\nif [ ! -f {state} ]; then\n  touch {state}\n  echo 'Error 1105: invalid connection' >&2\n  exit 1\nfi\necho '[]'\nexit 0\n",
            log = log.display(),
            state = state.display(),
        );
        // testkit::write_exe, never fs::write + chmod: a write descriptor another test thread's
        // fork inherits makes the exec below fail ETXTBSY under load (a round VM's unit step).
        testkit::write_exe(&script, &body);
        script
    }

    #[test]
    fn retries_once_past_a_dropped_connection_then_succeeds() {
        let d = testkit::TempDir::new("mail-bdcli-retry");
        let script = flaky_bd_script(d.path());
        let bd = BdCli { bin: script.display().to_string(), db: d.path().display().to_string(), conn_retries: 2 };
        let out = bd.run(&a(&["list", "--json"]), None);
        assert_eq!(out.code, 0, "stderr={}", out.stderr);
        assert_eq!(out.stdout.trim(), "[]");
        let calls = std::fs::read_to_string(d.path().join("calls.log")).unwrap();
        assert_eq!(calls.lines().count(), 2, "expected exactly one retry: {calls:?}");
    }

    #[test]
    fn does_not_retry_a_failure_that_is_not_a_dropped_connection() {
        let d = testkit::TempDir::new("mail-bdcli-no-retry");
        let script = d.path().join("fake-bd.sh");
        let log = d.path().join("calls.log");
        let body = format!("#!/bin/sh\necho called >> {log}\necho 'Error: no beads project found' >&2\nexit 1\n", log = log.display());
        // testkit::write_exe, never fs::write + chmod: a write descriptor another test thread's
        // fork inherits makes the exec below fail ETXTBSY under load (a round VM's unit step).
        testkit::write_exe(&script, &body);
        let bd = BdCli { bin: script.display().to_string(), db: d.path().display().to_string(), conn_retries: 2 };
        let out = bd.run(&a(&["list", "--json"]), None);
        assert_eq!(out.code, 1);
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(calls.lines().count(), 1, "a real refusal must not be retried: {calls:?}");
    }

    #[test]
    fn open_ask_ids_succeeds_through_bdcli_once_the_dropped_connection_is_retried() {
        let d = testkit::TempDir::new("mail-bdcli-retry-open-ask");
        let script = flaky_bd_script(d.path());
        let bd = BdCli { bin: script.display().to_string(), db: d.path().display().to_string(), conn_retries: 2 };
        assert_eq!(open_ask_ids(&bd, "needs-operator").unwrap(), Vec::<String>::new()); // literal-ok: test fixture
    }
}

#[cfg(test)]
mod after_create_not_discarded {
    #[test]
    fn no_caller_discards_after_create_failure() {
        let sources = [
            ("gh-intake", include_str!("../../gh-intake/src/real.rs")),
            ("groomer", include_str!("../../groomer/src/bd.rs")),
            ("incident", include_str!("../../incident/src/real.rs")),
            ("maechen-trigger", include_str!("../../maechen-trigger/src/real.rs")),
            ("mail", include_str!("bead.rs")),
            ("bdq", include_str!("../../bead/src/bin/bdq.rs")),
        ];
        for (name, src) in sources {
            assert!(src.contains("lifecycle_row::after_create"), "{name}: positive control: caller not found");
            for line in src.lines() {
                let l = line.trim_start();
                if l.starts_with("let _ =") && l.contains("after_create") {
                    panic!("{name}: discards after_create failure: {l}");
                }
            }
        }
    }
}
