//! `spira-claim unpoison` — clear a bead's poison so it stays cleared, and prove it
//! (DESIGN.md §8). The flow is written against [`World`], so every store, file, clock and
//! sleep it touches is a trait method; [`Live`] is the real one, the tests' fake is in memory.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use lifecycle::bead::{BeadState, HoldKind};
use serde_json::Value;

use crate::decide::{self, AskedStamp, Inputs, Thresholds};
use crate::events::{self, EventRow};
use crate::store::{self, Store};

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 3;

pub const POISON_LABEL: &str = "spira-poison";
pub const ASK_SUFFIX: &str = "— change the approach or drop it?";
pub const CAUSE_EVENT_MAX: usize = 200;
pub const WATCH_POLL_S: u64 = 10;
pub const WATCH_TIMEOUT_S: u64 = 2400;

// ---------------------------------------------------------------------------------------
// Schema (DESIGN.md §8.3)

/// `bd show <id> --json`, first element — only what unpoison reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeadRecord {
    pub id: String,
    pub status: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
}

/// `spira-lc show <id>`'s `bead` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LcRow {
    pub state: BeadState,
    pub version: u64,
    pub holds: BTreeSet<HoldKind>,
    pub holder: Option<String>,
}

impl LcRow {
    /// The aeon holding a WORKING row, as the refusal names it.
    pub fn working_holder(&self) -> Option<String> {
        let h = self.holder.as_deref().filter(|h| !h.trim().is_empty())?;
        (self.state == BeadState::Working).then(|| format!("{h} (lifecycle WORKING)"))
    }
}

impl LcRow {
    pub fn poisoned(&self) -> bool {
        self.holds.contains(&HoldKind::Poison)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LcApply {
    Applied,
    Refused(String),
    CannotTell(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRow {
    pub id: String,
    pub title: String,
}

/// One parsed audit.log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditLine {
    Examining { at: i64, poison_at: Option<u32> },
    CountsFailed { at: i64 },
    Complete { at: i64 },
    Other,
}

// ---------------------------------------------------------------------------------------
// The world

pub trait World {
    /// Ok(None) when bd says there is no such bead.
    fn bead(&mut self, id: &str) -> Result<Option<BeadRecord>, String>;
    fn events(&mut self, id: &str) -> Result<Vec<EventRow>, String>;
    /// Ok(None) when the machine has no row for the bead (spira-lc show exit 1).
    fn lc_row(&mut self, id: &str) -> Result<Option<LcRow>, String>;
    fn lc_unhold_poison(&mut self, id: &str, row: &LcRow, actor: &str) -> LcApply;
    fn write_event(&mut self, id: &str, event_type: &str, value: &str) -> Result<(), String>;
    fn clear_ask_history(&mut self, id: &str) -> Result<(), String>;
    fn ask_history_exists(&mut self, id: &str) -> bool;
    fn remove_label(&mut self, id: &str, label: &str) -> Result<(), String>;
    fn note(&mut self, id: &str, text: &str) -> Result<(), String>;
    fn open_asks(&mut self) -> Result<Vec<AskRow>, String>;
    fn close(&mut self, id: &str, reason: &str) -> Result<(), String>;
    /// `$SPIRA_RUN/poison-lifted/<id>` <- `attempts` (sp-wiyr2): the count a lift happened at, so CHECK 4's very next
    /// pass does not read the same unchanged count against a hold that is no longer there and poison it right back.
    /// `unpoison` never calls this — its own floor (`poison.cleared`) is what keeps it lifted; `deadlocked` does,
    /// since it deliberately never floors the count (DESIGN.md §9).
    fn mark_poison_lifted(&mut self, id: &str, attempts: u32) -> Result<(), String>;
    /// Current length of the audit log in bytes (0 when absent).
    fn audit_len(&mut self) -> u64;
    /// The audit log's bytes from `offset` to its end.
    fn audit_read_from(&mut self, offset: u64) -> Result<Vec<u8>, String>;
    fn now(&mut self) -> i64;
    fn sleep(&mut self, secs: u64);
}

// ---------------------------------------------------------------------------------------
// Options

#[derive(Debug, Clone)]
pub struct Opts {
    pub beads: Vec<String>,
    pub cause: String,
    pub watch: bool,
    pub watch_timeout_s: u64,
    pub dry_run: bool,
    pub credit: Option<String>,
    pub actor: String,
    pub poison_at: u32,
}

pub fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn valid_slug(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The event's `new_value`: quote-, backslash- and control-free, at most 200 characters.
pub fn bounded_cause(cause: &str) -> String {
    cause
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\') && !c.is_control())
        .take(CAUSE_EVENT_MAX)
        .collect()
}

/// Is this ask the one CHECK 4 raised when it poisoned `id` (sentinel.sh:669's subject)?
pub fn is_poison_ask(title: &str, id: &str) -> bool {
    title.starts_with(&format!("Spira bead {id} — ")) && title.trim_end().ends_with(ASK_SUFFIX)
}

// ---------------------------------------------------------------------------------------
// The flow

struct Cleared {
    id: String,
}

/// Run unpoison. Returns (exit code, stdout).
pub fn run(o: &Opts, w: &mut dyn World) -> (i32, String) {
    let mut out = String::new();
    let mut failed = false;
    let mut cleared: Vec<Cleared> = Vec::new();
    let t = Thresholds { poison_at: o.poison_at, ..Thresholds::default() };

    for id in &o.beads {
        match one(o, t, w, id, &mut out) {
            Some(true) => cleared.push(Cleared { id: id.clone() }),
            Some(false) => {}
            None => failed = true,
        }
    }

    if o.watch && !o.dry_run && !failed && !cleared.is_empty() && !watch(o, w, &cleared, &mut out) {
        failed = true;
    }
    (if failed { EXIT_FAILED } else { EXIT_OK }, out)
}

/// One bead. Some(true) cleared and verified, Some(false) skipped/would, None failed.
fn one(o: &Opts, t: Thresholds, w: &mut dyn World, id: &str, out: &mut String) -> Option<bool> {
    let fail = |out: &mut String, msg: String| {
        out.push_str(&format!("FAIL {id}: {msg}\n"));
        None
    };

    // Preconditions — nothing is written until every one of them passed.
    let bead = match w.bead(id) {
        Ok(Some(b)) => b,
        Ok(None) => return fail(out, "no such bead".into()),
        Err(e) => return fail(out, format!("cannot tell (bd show: {e}) — nothing was written")),
    };
    let rows = match w.events(id) {
        Ok(r) => r,
        Err(e) => return fail(out, format!("cannot tell (events: {e}) — nothing was written")),
    };
    let lc = match w.lc_row(id) {
        Ok(r) => r,
        Err(e) => {
            return fail(out, format!("cannot tell (spira-lc show: {e}; the machine must answer) — nothing was written"))
        }
    };
    let before = events::fold(id, &rows);
    let labelled = bead.labels.iter().any(|l| l == POISON_LABEL);
    let poisoned = lc.as_ref().is_some_and(LcRow::poisoned);

    // The live holder is the lifecycle row's WORKING holder — bd's status is never read
    // (sp-mve9i).
    let held = lc.as_ref().and_then(LcRow::working_holder);
    if let Some(h) = held {
        return fail(
            out,
            format!(
                "held by {h} — live work is never touched: let that aeon finish (or stop it: slay.sh --bead {id}), then re-run"
            ),
        );
    }
    if !poisoned && before.attempts < t.poison_at {
        out.push_str(&format!(
            "SKIP {id}: not poisoned and attempts {} < {} — nothing to clear\n",
            before.attempts, t.poison_at
        ));
        return Some(false);
    }
    if o.dry_run {
        out.push_str(&format!(
            "WOULD {id}: attempts {}, poisoned={} — write poison.cleared, reset ask history, release the lifecycle poison hold, note, resolve ask\n",
            before.attempts,
            u8::from(poisoned),
        ));
        return Some(false);
    }

    let mut warns: Vec<String> = Vec::new();

    // (credit) groomer's harness credit, before the floor so the fold floors it away.
    if let Some(slug) = &o.credit {
        if let Err(e) = w.write_event(id, "requeued", &format!("unjudged-{slug}")) {
            return fail(out, format!("could not write the unjudged-{slug} credit ({e}) — nothing else was changed"));
        }
    }
    // 1. The floor, FIRST: a racing CHECK 4 pass sees the reset count before the released hold.
    if let Err(e) = w.write_event(id, "poison.cleared", &bounded_cause(&o.cause)) {
        return fail(out, format!("could not write the poison.cleared floor ({e}) — nothing else was changed"));
    }
    // 2. The ask-dedup history (verify reports it if it stays).
    if let Err(e) = w.clear_ask_history(id) {
        warns.push(format!("ask history: {e}"));
    }
    // 3. The lifecycle hold; one retry from a fresh read on a lost race.
    if let Some(row) = lc.as_ref().filter(|r| r.poisoned()) {
        match w.lc_unhold_poison(id, row, &o.actor) {
            LcApply::Applied => {}
            LcApply::Refused(_) => {
                if let Ok(Some(fresh)) = w.lc_row(id) {
                    if fresh.poisoned() {
                        let _ = w.lc_unhold_poison(id, &fresh, &o.actor);
                    }
                }
            }
            LcApply::CannotTell(e) => warns.push(format!("unhold: {e}")),
        }
    }
    // 3b. The legacy label: vestigial (best effort, silent) — the poison is the hold.
    if labelled {
        let _ = w.remove_label(id, POISON_LABEL);
    }
    // 4. Why — the next aeon reads this.
    let note = format!(
        "Poison cleared by spira-claim unpoison ({}; attempts were {}): {}",
        o.actor, before.attempts, o.cause
    );
    if let Err(e) = w.note(id, &note) {
        warns.push(format!("note: {e}"));
    }
    // 5. The operator ask this poisoning raised.
    let mut resolved = Vec::new();
    match w.open_asks() {
        Ok(asks) => {
            for a in asks.iter().filter(|a| is_poison_ask(&a.title, id)) {
                let reason = format!("Resolved by spira-claim unpoison: {id}'s poison was cleared — {}", o.cause);
                match w.close(&a.id, &reason) {
                    Ok(()) => resolved.push(a.id.clone()),
                    Err(e) => warns.push(format!("close ask {}: {e}", a.id)),
                }
            }
        }
        Err(e) => warns.push(format!("list asks: {e}")),
    }

    // 6. VERIFY with CHECK 4's own counts and decision.
    let mut bad: Vec<String> = Vec::new();
    let after = match w.events(id) {
        Ok(r) => Some(events::fold(id, &r)),
        Err(_) => {
            bad.push("events-unreadable".into());
            None
        }
    };
    let bead2 = w.bead(id);
    let labels2 = match &bead2 {
        Ok(Some(b)) => b.labels.join(","),
        _ => bead.labels.join(","),
    };
    let poisoned2 = match w.lc_row(id) {
        Ok(r) => {
            let p = r.as_ref().is_some_and(LcRow::poisoned);
            if p {
                bad.push("lifecycle-hold-still-present".into());
            }
            p
        }
        Err(_) => {
            bad.push("lifecycle-unreadable".into());
            true
        }
    };
    let (n2, decision) = match &after {
        Some(l) => {
            let toks = decide::decide(
                &Inputs {
                    attempts: l.attempts,
                    requeues: l.requeues,
                    reclaims: l.reclaims,
                    labels: &labels2,
                    stamp: AskedStamp::default(),
                    poisoned: poisoned2,
                },
                t,
            );
            if l.attempts >= t.poison_at {
                bad.push(format!("attempts-still-{}", l.attempts));
            }
            if toks.contains(&decide::Token::Poison) {
                bad.push("check4-would-repoison".into());
            }
            (l.attempts.to_string(), decide::render(&toks))
        }
        None => ("?".into(), "?".into()),
    };
    if w.ask_history_exists(id) {
        bad.push("ask-history-still-present".into());
    }

    for a in &resolved {
        out.push_str(&format!("     resolved ask {a}\n"));
    }
    for wn in &warns {
        out.push_str(&format!("     warn {id}: {wn}\n"));
    }
    if bad.is_empty() {
        out.push_str(&format!(
            "OK   {id}: cleared — attempts {} -> {n2}, check4 decides \"{decision}\"\n",
            before.attempts
        ));
        Some(true)
    } else {
        let reasons = bad.iter().fold(String::new(), |acc, b| acc + " " + b);
        fail(out, format!("did not verify:{reasons} (attempts {n2}, check4={decision})"))
    }
}

// ---------------------------------------------------------------------------------------
// Watch

/// Parse one audit.log line: `<YYYY-MM-DDTHH:MM:SSZ> spira: <msg>`.
pub fn parse_audit_line(line: &str) -> AuditLine {
    let Some((ts, rest)) = line.split_once(' ') else { return AuditLine::Other };
    if ts.len() != 20 || !ts.ends_with('Z') || ts.as_bytes()[10] != b'T' {
        return AuditLine::Other;
    }
    let Some(at) = events::epoch_s(&events::norm_ts(ts)) else { return AuditLine::Other };
    let Some(msg) = rest.strip_prefix("spira: ") else { return AuditLine::Other };
    if msg.starts_with("CHECK4 examining ") {
        let poison_at = msg
            .split_whitespace()
            .find_map(|w| w.strip_prefix("poison="))
            .and_then(|v| v.trim_end_matches(',').parse().ok());
        AuditLine::Examining { at, poison_at }
    } else if msg.starts_with("CHECK4 bulk attempts query failed") {
        AuditLine::CountsFailed { at }
    } else if msg.starts_with("audit pass complete") {
        AuditLine::Complete { at }
    } else {
        AuditLine::Other
    }
}

/// Tracks audit passes that started strictly after `start`.
#[derive(Debug, Default)]
pub struct PassTracker {
    pub start: i64,
    in_pass: Option<Option<u32>>,
    disqualified: bool,
    /// The poison threshold the first qualifying pass reported, once one completed.
    pub done: Option<Option<u32>>,
}

impl PassTracker {
    pub fn new(start: i64) -> Self {
        PassTracker { start, ..Default::default() }
    }

    pub fn feed(&mut self, l: &AuditLine) {
        if self.done.is_some() {
            return;
        }
        match *l {
            AuditLine::Examining { at, poison_at } => {
                self.in_pass = (at > self.start).then_some(poison_at);
                self.disqualified = false;
            }
            AuditLine::CountsFailed { .. } => self.disqualified = true,
            AuditLine::Complete { .. } => {
                if let Some(p) = self.in_pass.take() {
                    if !self.disqualified {
                        self.done = Some(p);
                    }
                }
                self.disqualified = false;
            }
            AuditLine::Other => {}
        }
    }
}

fn fmt_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86400);
    let secs = epoch.rem_euclid(86400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// Is the bead poisoned now — the lifecycle row's poison hold? Err: cannot tell.
fn poisoned_now(w: &mut dyn World, id: &str) -> Result<bool, String> {
    w.lc_row(id).map(|r| r.as_ref().is_some_and(LcRow::poisoned))
}

fn watch(o: &Opts, w: &mut dyn World, cleared: &[Cleared], out: &mut String) -> bool {
    let start = w.now();
    let mut offset = w.audit_len();
    let mut carry: Vec<u8> = Vec::new();
    let mut tracker = PassTracker::new(start);
    let deadline = start + o.watch_timeout_s as i64;
    out.push_str(&format!(
        "watch: waiting for an audit pass that starts after {} (up to {}s)...\n",
        fmt_utc(start),
        o.watch_timeout_s
    ));
    loop {
        let len = w.audit_len();
        if len < offset {
            offset = 0; // rotated or truncated: read the new file from its start
            carry.clear();
        }
        if len > offset {
            if let Ok(bytes) = w.audit_read_from(offset) {
                offset += bytes.len() as u64;
                carry.extend_from_slice(&bytes);
                while let Some(nl) = carry.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = carry.drain(..=nl).collect();
                    tracker.feed(&parse_audit_line(String::from_utf8_lossy(&line).trim_end()));
                }
            }
        }
        // A hold back mid-watch is a failure at once.
        for c in cleared {
            if let Ok(true) = poisoned_now(w, &c.id) {
                out.push_str(&format!("FAIL watch {}: re-poisoned by the pass\n", c.id));
                return false;
            }
        }
        if tracker.done.is_some() {
            break;
        }
        if w.now() >= deadline {
            out.push_str(&format!("FAIL watch: no audit pass completed in {}s\n", o.watch_timeout_s));
            return false;
        }
        w.sleep(WATCH_POLL_S);
    }
    if let Some(Some(p)) = tracker.done {
        if p != o.poison_at {
            out.push_str(&format!(
                "watch: note — the audit pass ran with poison={p}, this clear verified against {}\n",
                o.poison_at
            ));
        }
    }
    let mut ok = true;
    for c in cleared {
        match poisoned_now(w, &c.id) {
            Ok(true) => {
                out.push_str(&format!("FAIL watch {}: re-poisoned by the pass\n", c.id));
                ok = false;
            }
            Ok(false) => out.push_str(&format!("OK   watch {}: still clear after a full audit pass\n", c.id)),
            Err(e) => {
                out.push_str(&format!("FAIL watch {}: cannot tell whether the poison came back ({e})\n", c.id));
                ok = false;
            }
        }
    }
    ok
}

// ---------------------------------------------------------------------------------------
// Parsing the stores' JSON

/// bd prints warnings before the payload sometimes; start at the first JSON token line.
fn json_only(text: &str) -> &str {
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with('[') || line.starts_with('{') {
            return &text[pos..];
        }
        pos += line.len();
    }
    text.trim()
}

fn s_field(v: &Value, k: &str) -> Option<String> {
    match v.get(k) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

pub fn parse_bead(text: &str) -> Result<Option<BeadRecord>, String> {
    let v: Value = serde_json::from_str(json_only(text).trim()).map_err(|e| format!("bd show: not JSON: {e}"))?;
    let b = match v {
        Value::Array(a) => match a.into_iter().next() {
            Some(b) => b,
            None => return Ok(None),
        },
        Value::Null => return Ok(None),
        other => other,
    };
    if b.get("error").is_some() && b.get("id").is_none() {
        return Ok(None);
    }
    let id = s_field(&b, "id").ok_or("bd show: row without id")?;
    let labels = b
        .get("labels")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(Some(BeadRecord {
        id,
        status: s_field(&b, "status").unwrap_or_default(),
        assignee: s_field(&b, "assignee"),
        labels,
    }))
}

pub fn parse_lc_show(text: &str) -> Result<LcRow, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("spira-lc show: not JSON: {e}"))?;
    let b = v.get("bead").ok_or("spira-lc show: no bead object")?;
    let st = s_field(b, "state").unwrap_or_default();
    let state = BeadState::from_str(&st).ok_or_else(|| format!("spira-lc show: bad state {st:?}"))?;
    let version = match b.get("version") {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
    .ok_or("spira-lc show: no version")?;
    let holds_v = match b.get("holds") {
        Some(Value::String(s)) if s.trim().is_empty() => Value::Null,
        Some(Value::String(s)) => serde_json::from_str(s).map_err(|e| format!("spira-lc show: holds: {e}"))?,
        Some(v) => v.clone(),
        None => Value::Null,
    };
    let holds = holds_v
        .as_array()
        .into_iter()
        .flatten()
        .map(|h| HoldKind::from_str(h.as_str().unwrap_or("")).unwrap_or(HoldKind::Operator))
        .collect();
    Ok(LcRow { state, version, holds, holder: s_field(b, "holder").filter(|h| !h.is_empty()) })
}

pub fn parse_asks(text: &str) -> Result<Vec<AskRow>, String> {
    let t = json_only(text).trim();
    if t.is_empty() {
        return Err("bd list: empty output".into());
    }
    let v: Value = serde_json::from_str(t).map_err(|e| format!("bd list: not JSON: {e}"))?;
    let arr = match v {
        Value::Array(a) => a,
        Value::Null => Vec::new(),
        other => vec![other],
    };
    Ok(arr
        .iter()
        .filter_map(|b| Some(AskRow { id: s_field(b, "id")?, title: s_field(b, "title").unwrap_or_default() }))
        .collect())
}

// ---------------------------------------------------------------------------------------
// The live world

pub struct Live {
    pub store: Store,
    pub run_dir: PathBuf,
    pub asked_dir: PathBuf,
    pub ask_label: String,
    pub beads_actor: String,
}

impl Live {
    fn bd(&self, args: &[&str], input: Option<&[u8]>) -> Result<store::Ran, String> {
        store::run_full(self.store.bd_cmd(args), self.store.timeout, input)
    }

    fn bd_ok(&self, args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
        let r = self.bd(args, input)?;
        if r.code != 0 {
            return Err(format!("bd {} exit {}: {}", args[0], r.code, r.stderr.lines().next().unwrap_or("no stderr")));
        }
        Ok(r.stdout)
    }

    fn lc(&self, args: &[&str]) -> Result<store::Ran, String> {
        let mut c = Command::new(&self.store.lc);
        c.args(args);
        store::run_full(c, self.store.timeout, None)
    }

    fn audit_log(&self) -> PathBuf {
        self.run_dir.join("audit.log")
    }

    /// `$SPIRA_RUN/ejected` — the withdrawn-suites sidecars' own directory (sp-2c1n0: no
    /// longer under the retired landstate ledger). `None` when `run_dir` was never resolved;
    /// the sidecar write is best-effort either way.
    fn ejected_dir(&self) -> Option<PathBuf> {
        if self.run_dir.as_os_str().is_empty() {
            None
        } else {
            Some(self.run_dir.join("ejected"))
        }
    }
}

/// The first non-empty line a failed child said, stderr first, bounded.
fn first_line(r: &store::Ran) -> String {
    let l = r.stderr.lines().chain(r.stdout.lines()).find(|l| !l.trim().is_empty()).unwrap_or("no output");
    l.chars().take(200).collect()
}

fn uuid4() -> Result<String, String> {
    let u = std::fs::read_to_string("/proc/sys/kernel/random/uuid").map_err(|e| format!("uuid: {e}"))?;
    let u = u.trim().to_string();
    if u.len() == 36 && u.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Ok(u)
    } else {
        Err("uuid: unexpected shape".into())
    }
}

impl World for Live {
    fn bead(&mut self, id: &str) -> Result<Option<BeadRecord>, String> {
        let r = self.bd(&["show", id, "--json"], None)?;
        if r.code != 0 {
            let said = format!("{}\n{}", r.stdout, r.stderr);
            if said.contains("no issue found") || said.contains("no issues found") {
                return Ok(None);
            }
            return Err(format!("exit {}: {}", r.code, r.stderr.lines().next().unwrap_or("no stderr")));
        }
        parse_bead(&r.stdout)
    }

    fn events(&mut self, id: &str) -> Result<Vec<EventRow>, String> {
        self.store.events(&[id.to_string()])
    }

    fn lc_row(&mut self, id: &str) -> Result<Option<LcRow>, String> {
        let r = self.lc(&["show", id])?;
        match r.code {
            0 => parse_lc_show(&r.stdout).map(Some),
            // spira-lc's "no such row" is exit 1 with `{}`; any other exit 1 is not that.
            1 if r.stdout.trim() == "{}" => Ok(None),
            // spira-lc prints its "cannot tell: …" on stdout, not stderr.
            c => Err(format!("exit {c}: {}", first_line(&r))),
        }
    }

    fn lc_unhold_poison(&mut self, id: &str, row: &LcRow, actor: &str) -> LcApply {
        let v = row.version.to_string();
        let args = [
            "event", "bead", id, "--expect", row.state.as_str(), "--version", &v, "--actor", actor, "--kind",
            r#"{"Unhold":{"kind":"Poison"}}"#,
        ];
        match self.lc(&args) {
            Ok(r) if r.code == 0 => LcApply::Applied,
            Ok(r) if r.code == 3 => LcApply::Refused(r.stdout.trim().to_string()),
            Ok(r) => LcApply::CannotTell(format!("exit {}: {}", r.code, first_line(&r))),
            Err(e) => LcApply::CannotTell(e),
        }
    }

    fn write_event(&mut self, id: &str, event_type: &str, value: &str) -> Result<(), String> {
        // bd sql takes its query only as argv; every value in it is validated or bounded.
        let u = uuid4()?;
        let q = format!(
            "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ({}, {}, {}, {}, {}, UTC_TIMESTAMP())",
            store::sql_quote(&u),
            store::sql_quote(id),
            store::sql_quote(event_type),
            store::sql_quote(&bounded_cause(&self.beads_actor)),
            store::sql_quote(&bounded_cause(value)),
        );
        self.bd_ok(&["sql", &q], None).map(|_| ())
    }

    fn clear_ask_history(&mut self, id: &str) -> Result<(), String> {
        match std::fs::remove_file(self.asked_dir.join(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn ask_history_exists(&mut self, id: &str) -> bool {
        self.asked_dir.join(id).exists()
    }

    fn remove_label(&mut self, id: &str, label: &str) -> Result<(), String> {
        self.bd_ok(&["label", "remove", id, label], None).map(|_| ())
    }

    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        self.bd_ok(&["note", id, "--stdin"], Some(text.as_bytes())).map(|_| ())
    }

    fn open_asks(&mut self) -> Result<Vec<AskRow>, String> {
        let label = self.ask_label.clone();
        // An ask is a non-work bead: its bd status is its state (spira_config::nonwork, sp-mve9i).
        let [flag, open] = spira_config::nonwork::status_args(spira_config::nonwork::Kind::Ask, spira_config::nonwork::Which::Open);
        let out = self.bd_ok(&["list", &flag, &open, "--label", &label, "--limit", "0", "--json"], None)?;
        parse_asks(&out)
    }

    fn close(&mut self, id: &str, reason: &str) -> Result<(), String> {
        // Through the lifecycle machine (sp-3fue0j), never a raw bd close.
        spira_config::lifecycle_row::close_with(&self.store.lc, id, reason, "spira-claim", None)
    }

    fn audit_len(&mut self) -> u64 {
        std::fs::metadata(self.audit_log()).map(|m| m.len()).unwrap_or(0)
    }

    fn audit_read_from(&mut self, offset: u64) -> Result<Vec<u8>, String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(self.audit_log()).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        let mut b = Vec::new();
        f.read_to_end(&mut b).map_err(|e| e.to_string())?;
        Ok(b)
    }

    fn now(&mut self) -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }

    fn sleep(&mut self, secs: u64) {
        std::thread::sleep(Duration::from_secs(secs));
    }

    fn mark_poison_lifted(&mut self, id: &str, attempts: u32) -> Result<(), String> {
        let dir = self.run_dir.join("poison-lifted");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(id), format!("{attempts}\n")).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------------------
// `reopen::World` (wave 4.19, row I) — written here, not in reopen.rs, so it can reach
// `Live`'s private `bd`/`bd_ok`/`land` helpers directly, the same split `deadlocked`
// already draws ("the write is unpoison's Live, reused rather than duplicated").

impl crate::reopen::World for Live {
    fn write_ejected(&mut self, id: &str, suites: &str) {
        let Some(dir) = self.ejected_dir() else { return };
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let tmp = dir.join(format!("{id}.{}", std::process::id()));
        if std::fs::write(&tmp, suites).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join(id));
        }
    }

    fn bd_reopen(&mut self, id: &str) -> Result<(), String> {
        // The bead's state is the machine's: bd status is inert (sp-mve9i).
        self.bd_ok(&["reopen", id], None).map(|_| ())
    }

    fn remove_submitted_label(&mut self, id: &str, label: &str) {
        let _ = self.bd_ok(&["label", "remove", id, label], None);
    }

    fn release_claim(&mut self, id: &str) -> Result<(), String> {
        self.store.release_claim(id)
    }

    fn write_reopen_event(&mut self, id: &str, cause: &str) {
        let _ = crate::counters::write_event(&self.store, &self.beads_actor, id, "reopen", cause);
    }

    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        self.bd_ok(&["note", id, "--stdin"], Some(text.as_bytes())).map(|_| ())
    }
}

#[cfg(test)]
#[path = "unpoison_tests.rs"]
mod tests;
