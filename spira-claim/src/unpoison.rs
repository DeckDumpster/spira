//! `spira-claim unpoison` — clear a bead's poison so it stays cleared, and prove it
//! (DESIGN.md §8). The flow is written against [`World`], so every store, file, clock and
//! sleep it touches is a trait method; [`Live`] is the real one, the tests' fake is in memory.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

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
    /// The dwarf holding a WORKING row, as the refusal names it.
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
}

// ---------------------------------------------------------------------------------------
// Options

#[derive(Debug, Clone)]
pub struct Opts {
    pub beads: Vec<String>,
    pub cause: String,
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

/// Run unpoison. Returns (exit code, stdout).
pub fn run(o: &Opts, w: &mut dyn World) -> (i32, String) {
    let mut out = String::new();
    let mut failed = false;
    let t = Thresholds { poison_at: o.poison_at, ..Thresholds::default() };

    for id in &o.beads {
        if one(o, t, w, id, &mut out).is_none() {
            failed = true;
        }
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
                "held by {h} — live work is never touched: let that dwarf finish (or stop it: slay.sh --bead {id}), then re-run"
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
    // 4. Why — the next dwarf reads this.
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
        .map(|h| HoldKind::from_str(h.as_str().unwrap_or("")).unwrap_or(HoldKind::Manual))
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
        crate::counters::write_event(&self.store, &self.beads_actor, id, event_type, value)
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

    fn record_failing(&mut self, id: &str, suites: &str) {
        let checks: Vec<String> = suites.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
        let _ = spira_config::proof::record_failing(&self.run_dir, id, &checks);
    }

    fn record_rejection(&mut self, id: &str, rejection: &spira_config::escalation::Rejection) {
        let mut r = rejection.clone();
        if r.tip.is_empty() {
            r.tip = self.store.lifecycle_of(&[id.to_string()]).ok().and_then(|m| m.get(id).and_then(|row| row.tip.clone())).unwrap_or_default();
        }
        let _ = self.lc(&["fact", id, "--kind", spira_config::escalation::REJECTION, "--actor", &self.beads_actor, "--cause", &r.encode()]);
    }

    fn lc_reopen(&mut self, id: &str, cause: &str) -> Result<(), String> {
        self.store.lc_reopen(id, cause, &self.beads_actor)
    }

    fn stop_holder(&mut self, id: &str, cause: &str) -> Result<(), String> {
        live_stop_holder(&self.run_dir, id, cause)
    }

    fn write_reopen_event(&mut self, id: &str, cause: &str) {
        let _ = crate::counters::write_event(&self.store, &self.beads_actor, id, "reopen", cause);
    }

    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        self.bd_ok(&["note", id, "--stdin"], Some(text.as_bytes())).map(|_| ())
    }
}

pub fn live_stop_holder(run: &std::path::Path, id: &str, why: &str) -> Result<(), String> {
    let epoch = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let own_dwarf = std::env::var("SPIRA_DWARF").is_ok_and(|v| !v.is_empty());
    crate::reopen::stop_holder_in(
        run,
        id,
        why,
        own_dwarf,
        crate::reopen::STOP_WAIT_S,
        &epoch,
        &|| std::thread::sleep(std::time::Duration::from_millis(500)),
    )?;
    if own_dwarf {
        return Ok(());
    }
    let systemctl = std::env::var("SPIRA_SYSTEMCTL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "systemctl".into());
    let mut stop = |unit: &str| {
        // batch-job: a unit stop lasts as long as the dwarf takes to exit
        let _ = std::process::Command::new(&systemctl).args(["--user", "stop", unit]).status();
    };
    hel::reap::stop_occupants(std::path::Path::new("/proc"), &run.join("worktree").join(id), &mut stop, 20, &|| {
        // waits-on: worktree occupants exiting; cap 20 polls
        std::thread::sleep(std::time::Duration::from_millis(500))
    })
    .map_err(|pids| format!("process(es) {pids:?} still work in {id}'s worktree after their unit was stopped"))
}

#[cfg(test)]
#[path = "unpoison_tests.rs"]
mod tests;
