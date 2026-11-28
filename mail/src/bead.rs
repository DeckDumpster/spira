//! Everything mail.sh asked `bd` to do: render a cited bead's details at the top of a
//! message, open a tracking decision bead for a question/decision send, close the tracking
//! bead (and note the work it cites) on `sendmail`, and the two bead-store queries `tidy`
//! and `sweep-dismissed` make. One seam (`Bd`) so every one of these is a unit test against
//! a fake, never a real store (matching dwarf's/queue's own ports/real split).

use serde_json::Value;
use spira_store::lc_call::LcError;

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
    fn close(&self, id: &str, reason: &str) -> Result<(), LcError>;
    /// Open `id` on the ask machine (never the bead machine), linked to `work_bead` when non-empty.
    fn ensure_ask(&self, id: &str, work_bead: &str) -> Result<(), String>;
}

/// The store, through its `content` door: every verb mail sends goes there ([`door`]), and a
/// verb the door does not serve answers nonzero without a spawn. `conn_retries` (>= 1) retries
/// only a call whose stderr names a dropped pooled connection (`bead::bdq::should_retry`);
/// every other failure surfaces on the first try.
pub struct BdCli {
    pub conn_retries: u32,
}

impl BdCli {
    pub fn configured(conn_retries: u32) -> Result<BdCli, String> {
        Ok(BdCli { conn_retries })
    }
}

/// Where one bd argv goes: the argv after `content`, and the request body.
#[derive(Debug, PartialEq, Eq)]
pub struct Door(pub Vec<String>, pub Option<String>);

/// The door for `args` (with `stdin`), or the verb it does not serve. A `note`'s text goes as
/// the request body (`note <id> --stdin`) so no text is mistaken for a flag; `history <id>
/// --events` is the door's `events --id <id>`.
pub fn door(args: &[String], stdin: Option<&str>) -> Result<Door, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match (words.as_slice(), stdin) {
        (["show" | "list", ..], None) => Ok(Door(args.to_vec(), None)),
        (["create", ..], body) => Ok(Door(args.to_vec(), body.map(str::to_string))),
        (["note", id, text], None) if !id.starts_with('-') => Ok(Door(s(&["note", id, "--stdin"]), Some(text.to_string()))),
        (["dep", "add" | "list", ..], None) => Ok(Door(args.to_vec(), None)),
        (["history", id, "--events", "--json"], None) => Ok(Door(s(&["events", "--id", id, "--json"]), None)),
        _ => Err(format!("mail: `{}` is not a content verb", words.first().copied().unwrap_or(""))),
    }
}

/// One request through `lc`: `door(args)` as a `content` call.
pub fn send(lc: &spira_store::lc_call::LcClient, args: &[String], stdin: Option<&str>) -> BdOut {
    let Door(argv, body) = match door(args, stdin) {
        Ok(d) => d,
        Err(e) => return BdOut::fail(2, e),
    };
    let argv: Vec<String> = std::iter::once("content".to_string()).chain(argv).collect();
    let reply = match body {
        Some(b) => lc.call_stdin(&argv, &b),
        None => lc.call(&argv),
    };
    match reply {
        Ok(r) => BdOut { code: r.code, stdout: r.out, stderr: r.err },
        Err(e) => BdOut::fail(2, e.to_string()),
    }
}

impl Bd for BdCli {
    fn ensure_ask(&self, id: &str, work_bead: &str) -> Result<(), String> {
        spira_store::lifecycle_row::create_ask(id, work_bead)
    }
    fn close(&self, id: &str, reason: &str) -> Result<(), LcError> {
        spira_store::lifecycle_row::close_as(id, reason, "mail")
    }
    fn run(&self, args: &[String], stdin: Option<&str>) -> BdOut {
        let lc = match spira_store::lc_call::LcClient::configured() {
            Ok(c) => c,
            Err(e) => return BdOut::fail(2, e),
        };
        let tries = self.conn_retries.max(1);
        let mut last = BdOut::default();
        for t in 1..=tries {
            last = send(&lc, args, stdin);
            if !bead::bdq::should_retry(last.code, t, tries, last.stderr.contains("invalid connection")) {
                break;
            }
        }
        last
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
            Some(Value::String(s)) => s.lines().rfind(|l| !l.trim().is_empty()).map(str::to_string),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|n| match n {
                    Value::Object(o) => o.get("text").and_then(|t| t.as_str()).map(str::to_string),
                    Value::String(s) => Some(s.clone()),
                    other => Some(other.to_string()),
                })
                .next_back(),
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

/// A store reply that says it could not tell whether the write happened.
const CANNOT_TELL_EXIT: i32 = 2;

/// Creates the tracking decision bead a question/decision send wires itself to, and opens its
/// row on the ask machine (never the bead machine: an ask is not claimable work). `None` if
/// the store is unconfigured; a failed create is an `Err`, so no ask is ever sent without its id.
///
/// `work_bead` (empty for none) is the bead the question is about; it rides the tracking bead
/// as a `work-bead:<id>` label, so a path that answers or closes the ASK BEAD rather than
/// replying to the mail — the cockpit pane's verdict, `resolve`, `verify-asks` — can still
/// find the work bead whose `ask` hold the answer lifts (sp-v62vn follow-up).
pub fn create_tracking_bead(bd: &dyn Bd, db_configured: bool, subject: &str, body: &str, ask_label: &str, work_bead: &str) -> Result<Option<String>, String> {
    if !db_configured {
        return Ok(None);
    }
    let mut labels = format!("{ask_label},overseer");
    if !work_bead.is_empty() {
        labels.push_str(&format!(",{WORK_BEAD_LABEL}{work_bead}"));
    }
    let out = bd.run(&a(&["create", subject, "-l", &labels, "--type", "decision", "--body-file", "-", "--silent"]), Some(body));
    let read_back;
    let id = if out.code == 0 && !out.stdout.trim().is_empty() {
        out.stdout.trim()
    } else if out.code == CANNOT_TELL_EXIT || out.code == 0 {
        read_back = match open_duplicate_ask(bd, ask_label, subject, "") {
            Ok(Some(id)) => id,
            Ok(None) => return Err(format!("bead create failed {}; read back from the store: nothing was recorded", bd_failure_detail(&out))),
            Err(e) => return Err(format!("bead create failed {}; the read-back also failed ({e}), so the ask may exist — look for it by subject before filing again", bd_failure_detail(&out))),
        };
        read_back.as_str()
    } else {
        return Err(format!("bead create failed {}", bd_failure_detail(&out)));
    };
    if let Err(e) = retry_until_deadline(|| bd.ensure_ask(id, work_bead)) {
        let closed = abandon_bead(bd, id, &format!("ask row not written: {e}"));
        return Err(format!("{id}: ask row not written: {e}; {closed}"));
    }
    Ok(Some(id.to_string()))
}

#[cfg(not(test))]
pub const RETRY_DEADLINE_MS: u64 = 5000;
#[cfg(test)]
pub const RETRY_DEADLINE_MS: u64 = 250;
pub const RETRY_PAUSE_MS: u64 = 100;

/// Runs `f` until it succeeds, at least twice and then until `RETRY_DEADLINE_MS` has passed.
pub fn retry_until_deadline<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let start = std::time::Instant::now();
    let mut attempt = 1;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if attempt >= 2 && start.elapsed().as_millis() as u64 >= RETRY_DEADLINE_MS => return Err(e),
            Err(_) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(RETRY_PAUSE_MS));
            }
        }
    }
}

/// Closes a tracking bead that could not be completed, so no ask is left that cannot be
/// answered; retries the lifecycle close within the deadline. Returns what happened, for the caller's error.
pub fn abandon_bead(bd: &dyn Bd, id: &str, why: &str) -> String {
    let reason = format!("ask abandoned by mail send: {why}");
    let mut last = String::new();
    let closed = retry_until_deadline(|| {
        bd.close(id, &reason).map_err(|e| {
            last = e.to_string();
            last.clone()
        })
    });
    match closed {
        Ok(()) => format!("{id} closed"),
        Err(_) => format!("{id} could NOT be closed: {last}"),
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

pub fn close(bd: &dyn Bd, id: &str, reason: &str) -> Result<(), LcError> {
    bd.close(id, reason)
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
pub fn sendmail_close_bead(bd: &dyn Bd, db_configured: bool, bead: &str, kind: &str, first_para: &str) -> Result<Vec<String>, LcError> {
    if !db_configured {
        return Ok(Vec::new());
    }
    let reason = suit_reason(kind, first_para);
    close(bd, bead, &reason).map_err(|e| match e {
        LcError::Refused(m) => LcError::Refused(format!("mail.sh: bead close refused for {bead}: {m}")),
        LcError::Fault(m) => LcError::Fault(format!("mail.sh: bead close failed for {bead}: {m}")),
        LcError::ReadEnded(m) => LcError::ReadEnded(format!("mail.sh: bead close failed for {bead}: {m}")),
        LcError::NotRunning(m) => LcError::NotRunning(format!("mail.sh: bead close failed for {bead}: {m}")),
    })?;
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

/// The open ask a new operator ask duplicates: one about the same work bead, or carrying
/// the same normalized subject. `Err` when the store cannot answer.
pub fn open_duplicate_ask(bd: &dyn Bd, ask_label: &str, subject: &str, work_bead: &str) -> Result<Option<String>, String> {
    let [flag, live] = spira_config::nonwork::status_args(spira_config::nonwork::Kind::Ask, spira_config::nonwork::Which::Live);
    let out = bd.run(&a(&["list", &flag, &live, "--label", ask_label, "--limit", "0", "--json"]), None);
    let v: Value = serde_json::from_str(&out.stdout).map_err(|_| format!("bead store query failed {}", bd_failure_detail(&out)))?;
    let arr = match v {
        Value::Array(a) => a,
        obj @ Value::Object(_) => vec![obj],
        _ => Vec::new(),
    };
    let norm = crate::repeat::normalize_subject(subject);
    let work_label = format!("{WORK_BEAD_LABEL}{work_bead}");
    for x in &arr {
        let Some(id) = x.get("id").and_then(Value::as_str) else { continue };
        let title = x.get("title").and_then(Value::as_str).unwrap_or("");
        let same_bead = !work_bead.is_empty()
            && x.get("labels").and_then(Value::as_array).is_some_and(|ls| ls.iter().any(|l| l.as_str() == Some(work_label.as_str())));
        if same_bead || (!norm.trim().is_empty() && crate::repeat::normalize_subject(title) == norm) {
            return Ok(Some(id.to_string()));
        }
    }
    Ok(None)
}

/// True only when the store resolves `id` and reports it closed; unresolved or unreadable
/// is "not known closed", so the caller keeps the mail.
pub fn bead_is_closed(bd: &dyn Bd, id: &str) -> bool {
    let out = bd.run(&a(&["show", id, "--json"]), None);
    first_object(&out.stdout)
        .filter(|d| d.get("error").is_none())
        .is_some_and(|d| spira_config::nonwork::row_closed(spira_config::nonwork::Kind::Ask, &d))
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
        pub row_failures: std::cell::Cell<u32>,
        pub row_attempts: std::cell::Cell<u32>,
        pub ask_rows: RefCell<Vec<(String, String)>>,
    }

    impl FakeBd {
        pub fn new(responses: Vec<BdOut>) -> FakeBd {
            FakeBd { calls: RefCell::new(Vec::new()), responses: RefCell::new(responses), row_failures: std::cell::Cell::new(0), row_attempts: std::cell::Cell::new(0), ask_rows: RefCell::new(Vec::new()) }
        }
        pub fn calls(&self) -> Vec<(Vec<String>, Option<String>)> {
            self.calls.borrow().clone()
        }
    }

    impl Bd for FakeBd {
        fn ensure_ask(&self, id: &str, work_bead: &str) -> Result<(), String> {
            self.ask_rows.borrow_mut().push((id.into(), work_bead.into()));
            self.row_attempts.set(self.row_attempts.get() + 1);
            let left = self.row_failures.get();
            if left > 0 {
                self.row_failures.set(left - 1);
                return Err("lc: injected failure".into());
            }
            Ok(())
        }
        // Recorded as the argv the real close used to be, so the callers' tests read the same.
        fn close(&self, id: &str, reason: &str) -> Result<(), LcError> {
            let out = self.run(&[String::from("close"), id.to_string(), "--reason-file".into(), "-".into()], Some(reason));
            match out.code {
                0 => Ok(()),
                3 => Err(LcError::Refused(out.combined())),
                _ => Err(LcError::Fault(out.combined())),
            }
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


    #[test_id(tzh366d)]
    #[test]
    fn unresolved_when_store_not_configured() {
        let bd = FakeBd::new(vec![]);
        assert_eq!(render_bead_block(&bd, false, "sp-zzz99"), "unresolved: sp-zzz99");
        assert!(bd.calls().is_empty());
    }

    #[test_id(tuysbq0)]
    #[test]
    fn unresolved_when_store_returns_nothing() {
        let bd = FakeBd::new(vec![BdOut::ok("")]);
        assert_eq!(render_bead_block(&bd, true, "sp-zzz99"), "unresolved: sp-zzz99");
    }

    #[test_id(tvbh0s0)]
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

    #[test_id(t9x5cij)]
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

    #[test_id(tyhp5bo)]
    #[test]
    fn falls_back_to_the_last_note_when_no_close_reason() {
        let json = r#"{"id":"sp-a","title":"t","status":"open","notes":["first note","second note  "]}"#;
        let bd = FakeBd::new(vec![BdOut::ok(json)]);
        let block = render_bead_block(&bd, true, "sp-a");
        assert!(block.ends_with("second note"), "{block}");
    }

    #[test_id(tjyfdaj)]
    #[test]
    fn suit_reason_maps_uphold_retire_amend() {
        assert_eq!(suit_reason("suit", "Uphold the finding."), "upheld");
        assert_eq!(suit_reason("suit", "Retire: superseded."), "retired");
        assert_eq!(suit_reason("suit", "Amend: narrower scope"), "narrower scope");
        assert_eq!(suit_reason("suit", "amend"), "amended");
        assert_eq!(suit_reason("suit", "Something else entirely"), "Something else entirely");
        assert_eq!(suit_reason("question", "Uphold the finding."), "Uphold the finding.");
    }

    #[test_id(tdgzhfl)]
    #[test]
    fn create_tracking_bead_returns_none_when_store_unconfigured() {
        let bd = FakeBd::new(vec![]);
        assert_eq!(create_tracking_bead(&bd, false, "subj", "body", "needs-operator", ""), Ok(None)); // literal-ok: test fixture
    }

    #[test_id(tunkh4c)]
    #[test]
    fn create_tracking_bead_returns_the_new_id() {
        let bd = FakeBd::new(vec![BdOut::ok("sp-newid1\n")]);
        let id = create_tracking_bead(&bd, true, "subj", "body", "needs-operator", "").unwrap().unwrap(); // literal-ok: test fixture
        assert_eq!(id, "sp-newid1");
        let calls = bd.calls();
        assert_eq!(calls[0].1.as_deref(), Some("body"));
        assert!(calls[0].0.contains(&"needs-operator,overseer".to_string())); // literal-ok: test fixture
    }

    /// The ask bead names the work bead it is about, so a path that closes the ask bead
    /// itself (the pane's verdict, `resolve`) can lift that bead's `ask` hold.
    #[test_id(tp93r5m)]
    #[test]
    fn a_tracking_bead_for_a_work_bead_carries_its_work_bead_label() {
        let bd = FakeBd::new(vec![BdOut::ok("sp-newid1\n")]);
        create_tracking_bead(&bd, true, "subj", "body", "needs-operator", "sp-work1").unwrap().unwrap(); // literal-ok: test fixture
        assert!(bd.calls()[0].0.contains(&"needs-operator,overseer,work-bead:sp-work1".to_string()), "{:?}", bd.calls()); // literal-ok: test fixture
    }

    #[test_id(tj7nem7)]
    #[test]
    fn sendmail_close_bead_is_a_noop_without_a_store() {
        let bd = FakeBd::new(vec![]);
        assert_eq!(sendmail_close_bead(&bd, false, "sp-a", "question", "para").unwrap(), Vec::<String>::new());
    }

    #[test_id(t531oav)]
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

    #[test_id(trt3bys)]
    #[test]
    fn sendmail_close_bead_propagates_a_close_failure() {
        let bd = FakeBd::new(vec![BdOut::fail(1, "no such bead")]);
        let err = sendmail_close_bead(&bd, true, "sp-a", "note", "para").unwrap_err();
        assert!(err.to_string().contains("sp-a"), "{err}");
    }

    #[test_id(tf79jcf)]
    #[test]
    fn sendmail_close_bead_keeps_a_refusal_distinct_from_a_fault() {
        let refused = FakeBd::new(vec![BdOut::fail(3, "a work bead in READY is not closed by a mail reply")]);
        assert!(matches!(sendmail_close_bead(&refused, true, "sp-a", "note", "p"), Err(LcError::Refused(_))));
        let faulted = FakeBd::new(vec![BdOut::fail(2, "exited 3 in a bead title")]);
        assert!(matches!(sendmail_close_bead(&faulted, true, "sp-a", "note", "p"), Err(LcError::Fault(_))));
    }

    #[test_id(t5eqyry)]
    #[test]
    fn open_ask_ids_fails_closed_on_bad_json() {
        let bd = FakeBd::new(vec![BdOut::fail(1, "boom")]);
        assert!(open_ask_ids(&bd, "needs-operator").is_err()); // literal-ok: test fixture
    }

    #[test_id(tp7gnhs)]
    #[test]
    fn open_ask_ids_parses_a_real_list() {
        let bd = FakeBd::new(vec![BdOut::ok(r#"[{"id":"sp-open1"}]"#)]);
        assert_eq!(open_ask_ids(&bd, "needs-operator").unwrap(), vec!["sp-open1".to_string()]); // literal-ok: test fixture
        // Every status but closed: an ask's bd status is its state (sp-mve9i, nonwork::Which::Live).
        assert_eq!(bd.calls()[0].0[..3], ["list", "--status", "open,in_progress,blocked,deferred"]);
    }

    #[test_id(twr48y8)]
    #[test]
    fn open_ask_ids_refusal_names_bds_exit_code_and_stderr() {
        let bd = FakeBd::new(vec![BdOut::fail(2, "Error 1105: invalid connection to dolt-beads on 127.0.0.1:3307")]);
        let err = open_ask_ids(&bd, "needs-operator").unwrap_err(); // literal-ok: test fixture
        assert!(err.contains("bd exit 2"), "{err}");
        assert!(err.contains("invalid connection to dolt-beads on 127.0.0.1:3307"), "{err}");
        assert!(err.contains("refusing to move any mail"), "{err}");
    }

    #[test_id(tfy8iis)]
    #[test]
    fn bd_failure_detail_says_so_when_stderr_is_empty() {
        let out = BdOut::fail(127, "");
        assert_eq!(bd_failure_detail(&out), "(bd exit 127, no stderr)");
    }

    #[test_id(te0mj89)]
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
}

#[cfg(test)]
mod after_create_not_discarded {
    #[test_id(t08swe5)]
    #[test]
    fn no_caller_discards_after_create_failure() {
        // Every create's lifecycle-row write is the door's `after`; nothing else writes it.
        let sources = [("content", include_str!("../../spira-lc/src/content.rs"))];
        for (name, src) in sources {
            assert!(src.contains("lifecycle_row::created_ids"), "{name}: positive control: caller not found");
            for line in src.lines() {
                let l = line.trim_start();
                if l.starts_with("let _ =") && (l.contains("created_ids") || l.contains("create-bead")) {
                    panic!("{name}: discards the create's lifecycle write: {l}");
                }
            }
        }
    }
}

#[cfg(test)]
mod door_tests {
    use super::*;
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    struct Recorder {
        seen: Mutex<Vec<Vec<String>>>,
        out: String,
    }

    impl Transport for Recorder {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            self.seen.lock().unwrap().push(argv.to_vec());
            Ok(Reply { code: 0, out: self.out.clone(), err: String::new(), read_ended: false })
        }
    }

    fn recorder(out: &str) -> (Arc<Recorder>, LcClient) {
        let r = Arc::new(Recorder { seen: Mutex::new(vec![]), out: out.into() });
        let lc = LcClient::over(r.clone(), true);
        (r, lc)
    }

    #[test_id(t7st3e5)]
    #[test]
    fn every_verb_mail_sends_goes_to_the_content_door() {
        let (r, lc) = recorder("[]");
        for argv in [&["dep", "add", "sp-a", "sp-b", "--type", "blocks"][..], &["dep", "list", "sp-a", "--direction=up", "--json"], &["history", "sp-a", "--events", "--json"]] {
            assert_eq!(send(&lc, &v(argv), None).code, 0, "{argv:?}");
        }
        let seen = r.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0], v(&["content", "dep", "add", "sp-a", "sp-b", "--type", "blocks"]));
        assert_eq!(seen[2], v(&["content", "events", "--id", "sp-a", "--json"]));

        let refused = send(&lc, &v(&["gate", "list"]), None);
        assert_eq!(refused.code, 2);
        assert!(refused.stderr.contains("gate"), "{}", refused.stderr);
        assert_eq!(r.seen.lock().unwrap().len(), 3, "an unserved verb sends nothing");
    }

    #[test_id(tkhn3et)]
    #[test]
    fn the_content_door_takes_each_verb_mail_sends() {
        assert_eq!(door(&v(&["show", "sp-a", "--json"]), None), Ok(Door(v(&["show", "sp-a", "--json"]), None)));
        // literal-ok: test fixture label
        let create = v(&["create", "Q?", "-l", "needs-ryan", "--type", "decision", "--body-file", "-", "--silent"]);
        assert_eq!(door(&create, Some("body")), Ok(Door(create.clone(), Some("body".into()))));
        assert_eq!(door(&v(&["note", "sp-a", "-- a note that looks like a flag"]), None), Ok(Door(v(&["note", "sp-a", "--stdin"]), Some("-- a note that looks like a flag".into()))));
    }

    #[test_id(t4umvvf)]
    #[test]
    fn bead_replied_reads_the_actor_off_the_events_answer() {
        let (_, lc) = recorder(r#"[{"actor":"ryan"},{"actor":"dwarf"}]"#);
        let bd = Fixed(lc);
        assert!(bead_replied(&bd, true, "sp-a", "ryan"));
        assert!(!bead_replied(&bd, true, "sp-a", "nobody"));
    }

    struct Fixed(LcClient);
    impl Bd for Fixed {
        fn run(&self, args: &[String], stdin: Option<&str>) -> BdOut {
            send(&self.0, args, stdin)
        }
        fn close(&self, _: &str, _: &str) -> Result<(), LcError> {
            unreachable!()
        }
        fn ensure_ask(&self, _: &str, _: &str) -> Result<(), String> {
            unreachable!()
        }
    }

    #[test_id(tjxpvzk)]
    #[test]
    fn no_bdq_resolution_remains() {
        let resolve = concat!("tool(\"", "bdq\")");
        let ghq = concat!("\"__", "ghq\"");
        let bounded = concat!("bounded(\"", "bdq\")");
        let offends = |src: &str| [resolve, ghq, bounded].iter().any(|p| name_code(src).contains(p));
        assert!(offends(&format!("let c = {bounded};")), "the matcher finds a planted bounded resolution");
        assert!(offends(&format!("let b = spira_config::release_env::{resolve}?;")), "the matcher finds a planted resolution");
        assert!(offends(&format!("c.arg({ghq});")), "the matcher finds a planted gh-through-bdq");
        for (name, src) in [
            ("mail/src/bead.rs", include_str!("bead.rs")),
            ("mail/src/main.rs", include_str!("main.rs")),
            ("incident/src/main.rs", include_str!("../../incident/src/main.rs")),
            ("incident/src/real.rs", include_str!("../../incident/src/real.rs")),
            ("gh-intake/src/main.rs", include_str!("../../gh-intake/src/main.rs")),
            ("gh-intake/src/real.rs", include_str!("../../gh-intake/src/real.rs")),
            ("landing-pass/src/main.rs", include_str!("../../landing-pass/src/main.rs")),
            ("landing-pass/src/real.rs", include_str!("../../landing-pass/src/real.rs")),
            ("landing-pass/src/land_verify.rs", include_str!("../../landing-pass/src/land_verify.rs")),
            ("watchd/src/send.rs", include_str!("../../watchd/src/send.rs")),
            ("watchd/src/context.rs", include_str!("../../watchd/src/context.rs")),
            ("spira-world/src/mail_out.rs", include_str!("../../spira-world/src/mail_out.rs")),
            ("spira-runtime/src/tasks/b/incident_settle.rs", include_str!("../../spira-runtime/src/tasks/b/incident_settle.rs")),
            ("spira-runtime/src/tasks/b/incident_alarms.rs", include_str!("../../spira-runtime/src/tasks/b/incident_alarms.rs")),
            ("spira-runtime/src/tasks/b/mail_reconcile.rs", include_str!("../../spira-runtime/src/tasks/b/mail_reconcile.rs")),
            ("spira-runtime/src/tasks/e/incident_probe.rs", include_str!("../../spira-runtime/src/tasks/e/incident_probe.rs")),
            ("spira-runtime/src/tasks/f/incident_file.rs", include_str!("../../spira-runtime/src/tasks/f/incident_file.rs")),
            ("spira-runtime/src/tasks/f/intake_gh.rs", include_str!("../../spira-runtime/src/tasks/f/intake_gh.rs")),
            ("spira-runtime/src/tasks/f/intake_gh_closeout.rs", include_str!("../../spira-runtime/src/tasks/f/intake_gh_closeout.rs")),
        ] {
            assert!(!offends(src), "{name} resolves bdq or runs gh through it");
        }
    }

    /// The source before its test module.
    fn name_code(src: &str) -> &str {
        src.split("#[cfg(test)]").next().unwrap_or(src)
    }
}
