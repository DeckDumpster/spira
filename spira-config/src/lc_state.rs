//! A work bead's state, read from the lifecycle machine (design bead-lifecycle-state-machine
//! §3.4, sp-mve9i): bd holds content, spira-lc holds state. Every Rust decision that used to
//! read a bd row's `status` or `assignee` — "is it closed", "is it in progress", "who holds
//! it" — reads the bead's lifecycle row here instead: `spira-lc list` for a pass over many
//! beads, `spira-lc show <id>` for one. Both are the machine's primitives; neither falls back
//! to bd, because a fallback to bd status would be the very read this module replaces.
//!
//! The state predicates below are the bd vocabulary's lifecycle equivalents, named for the
//! decision rather than the word: bd `closed` on a work bead meant "the builder is done with
//! it" ([`past_builder`]) or "nothing more will happen to it" ([`is_terminal`]); bd
//! `in_progress` meant "a dwarf holds it" ([`is_working`]); bd `open` meant "claimable"
//! ([`is_claimable`]).

use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

use crate::lc_call::LcClient;
use crate::lifecycle_row::lc_bin;

pub use lifecycle::batch::BatchState;

/// The `wait` hold's reason that makes it a timed snooze: `snooze-until:<epoch seconds>`.
pub const SNOOZE_PREFIX: &str = "snooze-until:";

pub fn snooze_reason(until: i64) -> String {
    format!("{SNOOZE_PREFIX}{until}")
}

/// The `wait` hold's reason for a dwarf checkpointed across a world stop:
/// `checkpoint-until:<epoch seconds>`. It keeps the bead unclaimable like a snooze, and
/// `world start` lifts it; the expiry only bounds a stop that is never followed by a start.
pub const CHECKPOINT_PREFIX: &str = "checkpoint-until:";

pub fn checkpoint_reason(until: i64) -> String {
    format!("{CHECKPOINT_PREFIX}{until}")
}

pub fn is_checkpoint(reason: &str) -> bool {
    reason.starts_with(CHECKPOINT_PREFIX)
}

pub fn snooze_until(reason: &str) -> Option<i64> {
    let rest = reason.strip_prefix(SNOOZE_PREFIX).or_else(|| reason.strip_prefix(CHECKPOINT_PREFIX))?;
    rest.split_whitespace().next()?.parse().ok()
}

/// Whether a wait hold's reason names a timed directive anywhere, so a malformed one can be refused.
pub fn names_directive(reason: &str) -> bool {
    reason.contains(SNOOZE_PREFIX) || reason.contains(CHECKPOINT_PREFIX)
}

/// One `spira_lifecycle.bead` row, as `spira-lc list` / `show` print it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub bead_id: String,
    pub state: String,
    pub holder: Option<String>,
    pub lease_until: Option<i64>,
    pub holds: Vec<String>,
    pub express: bool,
    /// The prerequisite bead ids this bead's current work is stacked on.
    pub stack: Vec<String>,
    pub phase: Option<String>,
    pub disposition: Option<String>,
    pub disposition_note: Option<String>,
}

impl Row {
    pub fn terminal(&self) -> bool {
        is_terminal(&self.state)
    }
    pub fn past_builder(&self) -> bool {
        past_builder(&self.state)
    }
    pub fn working(&self) -> bool {
        is_working(&self.state)
    }
    pub fn claimable(&self) -> bool {
        is_claimable(&self.state)
    }
    pub fn held(&self, kind: &str) -> bool {
        self.holds.iter().any(|h| h == kind)
    }
}

/// No outgoing transition, for any actor (`lifecycle::bead::BeadState::is_terminal`).
pub fn is_terminal(state: &str) -> bool {
    matches!(state, "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE")
}

/// The builder has handed the bead on (or it is over): what bd `closed` (with or without the
/// submitted label) meant for a work bead. Everything but READY, WORKING and REWORK.
pub fn past_builder(state: &str) -> bool {
    !state.is_empty() && !matches!(state, "READY" | "WORKING" | "REWORK")
}

/// A dwarf holds the bead: what bd `in_progress` meant.
pub fn is_working(state: &str) -> bool {
    state == "WORKING"
}

/// Nobody holds it and it waits for a builder: what bd `open` meant for a work bead.
pub fn is_claimable(state: &str) -> bool {
    matches!(state, "READY" | "REWORK")
}

fn scalar(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        other => Some(other.to_string()),
    }
}

/// The `holds` column: a JSON array of hold names, as an array or as JSON text (dolt hands a
/// JSON column back as text). Absent, null or empty text is no holds; anything else that is not
/// an array of strings is an error, never an empty list (sp-cxfgj5: a false empty reads as
/// "not held").
fn holds(v: Option<&Value>) -> Result<Vec<String>, String> {
    let arr = match v {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) if s.trim().is_empty() => return Ok(Vec::new()),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => a,
            Ok(Value::Null) => return Ok(Vec::new()),
            _ => return Err(format!("holds is not a JSON array: {s:?}")),
        },
        Some(other) => return Err(format!("holds is not a JSON array: {other}")),
    };
    arr.into_iter().map(|x| x.as_str().map(str::to_string).ok_or_else(|| format!("a hold is not a name: {x}"))).collect()
}

/// The `stack` column: a JSON object keyed by prerequisite id, as an object or as JSON text.
/// Absent, null or empty text is no stack; anything else that is not an object is an error.
fn stack(v: Option<&Value>) -> Result<Vec<String>, String> {
    let obj = match v {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(s)) if s.trim().is_empty() => return Ok(Vec::new()),
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).map_err(|_| format!("stack is not a JSON object: {s:?}"))?,
        Some(o) => o.clone(),
    };
    match obj {
        Value::Object(m) => Ok(m.keys().cloned().collect()),
        Value::Null => Ok(Vec::new()),
        other => Err(format!("stack is not a JSON object: {other}")),
    }
}

fn lease(v: Option<&Value>) -> Result<Option<i64>, String> {
    match scalar(v).filter(|x| !x.trim().is_empty()) {
        None => Ok(None),
        Some(x) => x.trim().parse::<f64>().map(|f| Some(f as i64)).map_err(|_| format!("lease_until is not a number: {x:?}")),
    }
}

fn express(v: Option<&Value>) -> Result<bool, String> {
    match scalar(v).as_deref() {
        None | Some("" | "0" | "false") => Ok(false),
        Some("1" | "true") => Ok(true),
        Some(x) => Err(format!("express is not a flag: {x:?}")),
    }
}

fn row_of(r: &Value) -> Result<Row, String> {
    let id = scalar(r.get("bead_id")).unwrap_or_default();
    let at = |e: String| format!("{id}: {e}");
    Ok(Row {
        state: scalar(r.get("state")).unwrap_or_default(),
        holder: scalar(r.get("holder")).filter(|h| !h.is_empty()),
        lease_until: lease(r.get("lease_until")).map_err(at)?,
        holds: holds(r.get("holds")).map_err(at)?,
        express: express(r.get("express")).map_err(at)?,
        stack: stack(r.get("stack")).map_err(at)?,
        phase: scalar(r.get("dwarf_phase")).filter(|x| !x.is_empty()),
        disposition: scalar(r.get("disposition")).filter(|x| !x.is_empty()),
        disposition_note: scalar(r.get("disposition_note")),
        bead_id: id,
    })
}

/// `spira-lc list`'s JSON array. A row whose columns do not parse is an error for the whole
/// answer, never a row with its holds or stack read as empty.
pub fn parse_rows(text: &str) -> Result<Vec<Row>, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("spira-lc list: not JSON: {e}"))?;
    let Value::Array(a) = v else {
        return Err("spira-lc list: not a JSON array".into());
    };
    let rows = a.iter().map(row_of).collect::<Result<Vec<_>, _>>().map_err(|e| format!("spira-lc list: {e}"))?;
    Ok(rows.into_iter().filter(|r| !r.bead_id.is_empty()).collect())
}

/// `spira-lc show <id>`'s `{"bead": {...}, "delivery": ...}`, as printed with exit 0. An
/// answer that carries no bead is an error: "no row" is exit 1, never a shape of exit 0's text.
pub fn parse_show(text: &str) -> Result<Option<Row>, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("spira-lc show: not JSON: {e}"))?;
    let Some(b) = v.get("bead").filter(|b| b.is_object()) else {
        return Err("spira-lc show: the answer carries no bead".into());
    };
    let r = row_of(b).map_err(|e| format!("spira-lc show: {e}"))?;
    if r.bead_id.is_empty() {
        return Err("spira-lc show: the bead has no id".into());
    }
    Ok(Some(r))
}

/// Exit 0 or 1 is an answer (1: no such row); anything else is an error naming the call.
fn run_on(c: &LcClient, args: &[&str]) -> Result<(i32, String), String> {
    let r = c.call(args)?;
    if r.code != 0 && r.code != 1 {
        return Err(format!("spira-lc {} exited {}: {}", args.join(" "), r.code, r.why()));
    }
    Ok((r.code, r.out))
}

fn client(bin: &str, secs: u32) -> Result<LcClient, String> {
    // batch-job: the interactive default is 5 s (the call-deadline cap); a batch caller passes more.
    Ok(LcClient::for_bin(bin)?.within(Duration::from_secs(u64::from(secs))))
}

/// Every lifecycle row (`spira-lc list`). Err when the machine cannot answer: a caller that
/// cannot read the state must not decide as if it had (law-a-control-that-cannot-check-must-refuse).
pub fn list_with(bin: &str) -> Result<Vec<Row>, String> {
    list_within(bin, 5)
}

/// [`list_with`] through `c`.
pub fn list_on(c: &LcClient) -> Result<Vec<Row>, String> {
    parse_rows(&list_raw_on(c)?)
}

/// The rows in one state (`spira-lc list --state <STATE>`): what a reader that wants only
/// the WORKING dwarves asks, instead of every row.
pub fn list_state_with(bin: &str, state: &str) -> Result<Vec<Row>, String> {
    list_state_on(&client(bin, 5)?, state)
}

pub fn list_state_on(c: &LcClient, state: &str) -> Result<Vec<Row>, String> {
    match run_on(c, &["list", "--state", state])? {
        (0, out) => parse_rows(&out),
        (rc, _) => Err(format!("spira-lc list --state {state} exited {rc}")),
    }
}

/// `list_with` for a batch caller that can wait out a loaded store: `secs` bounds the read.
pub fn list_within(bin: &str, secs: u32) -> Result<Vec<Row>, String> {
    list_on(&client(bin, secs)?)
}

/// `spira-lc list`'s stdout, unparsed, for a caller that keeps the last good answer.
pub fn list_raw_with(bin: &str) -> Result<String, String> {
    list_raw_on(&client(bin, 5)?)
}

pub fn list_raw_on(c: &LcClient) -> Result<String, String> {
    match run_on(c, &["list"])? {
        (0, out) => Ok(out),
        (rc, _) => Err(format!("spira-lc list exited {rc}")),
    }
}

pub fn list_raw() -> Result<String, String> {
    list_raw_with(&lc_bin()?)
}

pub fn list() -> Result<Vec<Row>, String> {
    list_with(&lc_bin()?)
}

/// One bead's row (`spira-lc show <id>`): Ok(None) when the machine has no row for it.
pub fn row_with(bin: &str, id: &str) -> Result<Option<Row>, String> {
    row_on(&client(bin, 5)?, id)
}

pub fn row_on(c: &LcClient, id: &str) -> Result<Option<Row>, String> {
    match run_on(c, &["show", id])? {
        (0, out) => parse_show(&out),
        _ => Ok(None),
    }
}

pub fn row(id: &str) -> Result<Option<Row>, String> {
    row_with(&lc_bin()?, id)
}

/// The rows keyed by bead id.
pub fn index(rows: Vec<Row>) -> HashMap<String, Row> {
    rows.into_iter().map(|r| (r.bead_id.clone(), r)).collect()
}

// ---- Typed answers (sp-cxfgj5, law-lifecycle-state-is-not-text) ----------------------------
//
// A decision about a bead's state or holds reads one of these: the machine's own enums, and a
// `Lookup` whose "cannot tell" is a variant, never a substring of what a call printed.

use lifecycle::bead::{BeadState, DispositionStatus, DwarfPhase, HoldKind};

/// One typed read of one object: the object, the machine's "no such row", or that it could not
/// tell (unreachable, past its deadline, exit 2, or an answer that does not parse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup<T> {
    Found(T),
    NoRow,
    CannotTell(String),
}

impl<T> Lookup<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Lookup<U> {
        match self {
            Lookup::Found(t) => Lookup::Found(f(t)),
            Lookup::NoRow => Lookup::NoRow,
            Lookup::CannotTell(why) => Lookup::CannotTell(why),
        }
    }

    pub fn found(self) -> Option<T> {
        match self {
            Lookup::Found(t) => Some(t),
            _ => None,
        }
    }

    pub fn is_cannot_tell(&self) -> bool {
        matches!(self, Lookup::CannotTell(_))
    }
}

/// A hold name as the machine stores it. `operator` is the retired spelling of `manual`.
pub fn hold_kind(name: &str) -> Option<HoldKind> {
    HoldKind::from_str(name).or((name == "operator").then_some(HoldKind::Manual))
}

/// One `spira_lifecycle.bead` row, typed: every column a decision reads is the machine's enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bead {
    pub bead_id: String,
    pub state: BeadState,
    pub holder: Option<String>,
    pub lease_until: Option<i64>,
    pub holds: Vec<HoldKind>,
    pub express: bool,
    /// The prerequisite bead ids this bead's current work is stacked on.
    pub stack: Vec<String>,
    pub phase: Option<DwarfPhase>,
    pub disposition: Option<DispositionStatus>,
    /// The row's compare-and-swap version.
    pub version: Option<String>,
}

impl Bead {
    pub fn held(&self, kind: HoldKind) -> bool {
        self.holds.contains(&kind)
    }

    /// The holds that keep a claim off the bead: every hold but `wait`, which a claim passes.
    pub fn blocking_holds(&self) -> Vec<HoldKind> {
        self.holds.iter().copied().filter(|h| *h != HoldKind::Wait).collect()
    }
}

/// A row's JSON, typed. A state, hold, phase or disposition the machine does not define is an
/// error, never a default.
pub fn bead_of(r: &Value) -> Result<Bead, String> {
    let row = row_of(r)?;
    let at = |e: String| format!("{}: {e}", row.bead_id);
    if row.bead_id.is_empty() {
        return Err("a row has no bead_id".into());
    }
    let state = BeadState::from_str(&row.state).ok_or_else(|| at(format!("unknown state {:?}", row.state)))?;
    let holds = row.holds.iter().map(|h| hold_kind(h).ok_or_else(|| at(format!("unknown hold {h:?}")))).collect::<Result<Vec<_>, _>>()?;
    let phase = row.phase.as_deref().map(|p| DwarfPhase::from_str(p).ok_or_else(|| at(format!("unknown dwarf phase {p:?}")))).transpose()?;
    let disposition = row.disposition.as_deref().map(|d| DispositionStatus::from_str(d).ok_or_else(|| at(format!("unknown disposition {d:?}")))).transpose()?;
    Ok(Bead {
        version: scalar(r.get("version")).filter(|v| !v.is_empty()),
        bead_id: row.bead_id,
        state,
        holder: row.holder,
        lease_until: row.lease_until,
        holds,
        express: row.express,
        stack: row.stack,
        phase,
        disposition,
    })
}

/// `spira-lc show <id>`'s answer, typed by its exit code first: 0 is a row (and must parse as
/// one), 1 is no row, anything else is "cannot tell" whatever it printed.
pub fn bead_from(code: i32, stdout: &str, why: &str) -> Lookup<Bead> {
    match code {
        0 => {
            let v: Value = match serde_json::from_str(stdout.trim()) {
                Ok(v) => v,
                Err(e) => return Lookup::CannotTell(format!("spira-lc show: not JSON: {e}")),
            };
            match v.get("bead").filter(|b| b.is_object()).map(bead_of) {
                Some(Ok(b)) => Lookup::Found(b),
                Some(Err(e)) => Lookup::CannotTell(format!("spira-lc show: {e}")),
                None => Lookup::CannotTell("spira-lc show: exit 0 carried no bead".into()),
            }
        }
        1 => Lookup::NoRow,
        rc => Lookup::CannotTell(format!("spira-lc show exited {rc}: {why}")),
    }
}

/// One bead, typed, through `c`.
pub fn bead_on(c: &LcClient, id: &str) -> Lookup<Bead> {
    match c.call(&["show", id]) {
        Ok(r) => bead_from(r.code, &r.out, &r.why()),
        Err(e) => Lookup::CannotTell(e.to_string()),
    }
}

/// One bead, typed, through the configured client.
pub fn bead(id: &str) -> Lookup<Bead> {
    match LcClient::configured() {
        Ok(c) => bead_on(&c, id),
        Err(e) => Lookup::CannotTell(e),
    }
}

/// A bead's holds, typed: what `spira-lc holds` answers, with "cannot tell" a variant.
pub fn holds_on(c: &LcClient, id: &str) -> Lookup<Vec<HoldKind>> {
    bead_on(c, id).map(|b| b.holds)
}

/// `spira-lc list`'s JSON array, typed. One row that does not parse fails the whole answer.
pub fn beads_of(text: &str) -> Result<Vec<Bead>, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("spira-lc list: not JSON: {e}"))?;
    let Value::Array(a) = v else {
        return Err("spira-lc list: not a JSON array".into());
    };
    a.iter().map(bead_of).collect::<Result<Vec<_>, _>>().map_err(|e| format!("spira-lc list: {e}"))
}

/// Every row (or those in `state`), typed, through `c`. Err is "cannot tell": a caller that
/// cannot read the state must not decide as if it had.
pub fn beads_on(c: &LcClient, state: Option<BeadState>) -> Result<Vec<Bead>, String> {
    let argv: Vec<&str> = match state {
        Some(s) => vec!["list", "--state", s.as_str()],
        None => vec!["list"],
    };
    let r = c.call(&argv)?;
    if r.code != 0 {
        return Err(format!("spira-lc {} exited {}: {}", argv.join(" "), r.code, r.why()));
    }
    beads_of(&r.out)
}

/// The `--json` answer of `spira-lc holds <id> --json` (and `state`, `held`): the bead and its
/// typed columns, hold names in the machine's own spelling. Exit 1 is no row, exit 2 cannot
/// tell; neither prints this.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HoldsAnswer {
    pub bead_id: String,
    pub state: String,
    pub holds: Vec<String>,
}

impl HoldsAnswer {
    pub fn of(b: &Bead) -> Self {
        HoldsAnswer { bead_id: b.bead_id.clone(), state: b.state.as_str().into(), holds: b.holds.iter().map(|h| h.as_str().to_string()).collect() }
    }

    /// The answer typed back: an unknown state or hold is an error, never dropped.
    pub fn typed(&self) -> Result<(BeadState, Vec<HoldKind>), String> {
        let state = BeadState::from_str(&self.state).ok_or_else(|| format!("unknown state {:?}", self.state))?;
        let holds = self.holds.iter().map(|h| hold_kind(h).ok_or_else(|| format!("unknown hold {h:?}"))).collect::<Result<_, _>>()?;
        Ok((state, holds))
    }
}

/// `spira-lc holds <id> --json`'s (exit code, stdout), typed: the shape a caller that runs the
/// CLI (rather than holding an `LcClient`) reads. Exit 1 is no row, anything else but 0 is
/// cannot tell, and exit 0 must parse.
pub fn holds_answer_from(code: i32, stdout: &str, why: &str) -> Lookup<(BeadState, Vec<HoldKind>)> {
    match code {
        0 => match serde_json::from_str::<HoldsAnswer>(stdout.trim()).map_err(|e| e.to_string()).and_then(|a| a.typed()) {
            Ok(t) => Lookup::Found(t),
            Err(e) => Lookup::CannotTell(format!("spira-lc holds --json: {e}")),
        },
        1 => Lookup::NoRow,
        rc => Lookup::CannotTell(format!("spira-lc holds exited {rc}: {why}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkpoint_reason_snoozes_like_a_timed_wait_and_is_told_apart() {
        let r = checkpoint_reason(1_900_000_000);
        assert_eq!(snooze_until(&r), Some(1_900_000_000));
        assert!(is_checkpoint(&r) && !is_checkpoint(&snooze_reason(5)));
        assert_eq!(snooze_until(&snooze_reason(5)), Some(5));
        assert_eq!(snooze_until("snooze-until:7 because the upstream lands"), Some(7));
        assert_eq!(snooze_until("snooze-until:soon"), None);
        assert!(names_directive("see snooze-until:7") && !names_directive("blocker unlanded"));
    }

    #[test]
    fn predicates_name_the_old_bd_decisions() {
        for s in ["SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED", "SUPERSEDED", "DROPPED", "DONE"] {
            assert!(past_builder(s), "{s}");
        }
        for s in ["READY", "WORKING", "REWORK", ""] {
            assert!(!past_builder(s), "{s}");
        }
        assert!(is_terminal("LANDED") && !is_terminal("SUBMITTED"));
        assert!(is_working("WORKING") && !is_working("READY"));
        assert!(is_claimable("READY") && is_claimable("REWORK") && !is_claimable("WORKING"));
    }

    #[test]
    fn list_and_show_parse_holds_either_way() {
        let rows = parse_rows(
            r#"[{"bead_id":"sp-a","state":"WORKING","holder":"dwarf-1","lease_until":"17.0","holds":"[\"wait\"]"},
                {"bead_id":"sp-b","state":"READY","holder":null,"holds":[]}]"#,
        )
        .unwrap();
        assert_eq!(rows[0].holds, vec!["wait"]);
        assert_eq!(rows[0].lease_until, Some(17));
        assert_eq!(rows[1].holder, None);
        let one = parse_show(r#"{"bead":{"bead_id":"sp-c","state":"LANDED","holds":["poison"]},"delivery":null}"#).unwrap().unwrap();
        assert!(one.terminal() && one.held("poison"));
        assert!(parse_rows("{}").is_err());
    }

    /// sp-cxfgj5: a holds column that does not parse is "cannot tell", never "not held".
    #[test]
    fn a_holds_column_that_does_not_parse_is_an_error_never_no_holds() {
        let bad = r#"[{"bead_id":"sp-a","state":"READY","holds":"cannot tell: deadline"}]"#;
        assert!(parse_rows(bad).is_err(), "{:?}", parse_rows(bad));
        assert!(parse_show(r#"{"bead":{"bead_id":"sp-a","state":"READY","holds":{"poison":1}}}"#).is_err());
        assert!(parse_rows(r#"[{"bead_id":"sp-a","state":"READY","holds":null},{"bead_id":"sp-b","state":"READY","holds":""}]"#).is_ok());
    }

    /// sp-cxfgj5: exit 0 with no bead in it is not "no row" (that is exit 1).
    #[test]
    fn a_show_answer_with_no_bead_is_an_error_never_no_row() {
        assert!(parse_show("{}").is_err());
        assert!(parse_show(r#"{"bead":null,"delivery":null}"#).is_err());
    }

    /// sp-cxfgj5: a stack or lease that does not parse is an error, not empty.
    #[test]
    fn a_stack_or_lease_that_does_not_parse_is_an_error() {
        assert!(parse_rows(r#"[{"bead_id":"sp-a","state":"WORKING","stack":"[oops"}]"#).is_err());
        assert!(parse_rows(r#"[{"bead_id":"sp-a","state":"WORKING","lease_until":"soon"}]"#).is_err());
        assert!(parse_rows(r#"[{"bead_id":"sp-a","state":"WORKING","express":"maybe"}]"#).is_err());
    }

    #[test]
    fn the_typed_read_is_enums_and_cannot_tell_is_a_variant() {
        let ok = r#"{"bead":{"bead_id":"sp-a","state":"WORKING","holder":"d1","holds":"[\"poison\",\"wait\"]","dwarf_phase":"building","version":"7"},"delivery":null}"#;
        let Lookup::Found(b) = bead_from(0, ok, "") else { panic!("{:?}", bead_from(0, ok, "")) };
        assert_eq!(b.state, BeadState::Working);
        assert_eq!(b.holds, vec![HoldKind::Poison, HoldKind::Wait]);
        assert_eq!(b.blocking_holds(), vec![HoldKind::Poison]);
        assert_eq!(b.phase, Some(DwarfPhase::Building));
        assert_eq!(b.version.as_deref(), Some("7"));
        assert_eq!(bead_from(1, "{}", ""), Lookup::NoRow);
        assert!(bead_from(2, "", "cannot tell: deadline").is_cannot_tell());
        assert!(bead_from(0, "usage: spira-lc show <id>", "").is_cannot_tell(), "usage text is never a row");
        assert!(bead_from(0, r#"{"bead":{"bead_id":"sp-a","state":"WEIRD"}}"#, "").is_cannot_tell(), "an unknown state is never a default");
        assert!(bead_from(0, r#"{"bead":{"bead_id":"sp-a","state":"READY","holds":["frozen"]}}"#, "").is_cannot_tell(), "an unknown hold is never dropped");
        assert_eq!(hold_kind("operator"), Some(HoldKind::Manual));
        let a = HoldsAnswer::of(&b);
        assert_eq!(serde_json::to_string(&a).unwrap(), r#"{"bead_id":"sp-a","state":"WORKING","holds":["poison","wait"]}"#);
        assert_eq!(holds_answer_from(0, &serde_json::to_string(&a).unwrap(), ""), Lookup::Found((BeadState::Working, vec![HoldKind::Poison, HoldKind::Wait])));
        assert_eq!(holds_answer_from(1, "", ""), Lookup::NoRow);
        assert!(holds_answer_from(0, "poison\nwait", "").is_cannot_tell(), "the text form is never read as the typed one");
        assert!(holds_answer_from(2, "", "x").is_cannot_tell());
        assert!(beads_of(r#"[{"bead_id":"sp-a","state":"READY"},{"bead_id":"sp-b","state":"NOPE"}]"#).is_err());
    }
}
