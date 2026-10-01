//! The pure decisions (DESIGN.md §4): each was a lib.sh function aeon.sh called with
//! gathered inputs, each is now a Rust function with its table as a test.

use crate::bd;

/// `world_stop_decide`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldStop {
    None,
    Refuse,
    Stop,
}

pub fn world_stop(has_label: bool, live: &str, skip: bool) -> WorldStop {
    if !has_label {
        WorldStop::None
    } else if !live.is_empty() && !skip {
        WorldStop::Refuse
    } else {
        WorldStop::Stop
    }
}

/// `hb_tick`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HbTick {
    Ok,
    Renew,
    Lapse,
    Thrash,
}

/// Trace growth renews unconditionally; only a still trace can lapse; only a trace that
/// neither grew nor lapsed can thrash. A fuse that is not a plain integer never trips.
pub fn hb_tick(prev_mtime: i64, cur_mtime: i64, now: i64, deadline: i64, fuse: &str, wall_min: i64, session_start: i64) -> HbTick {
    if cur_mtime != prev_mtime {
        return HbTick::Renew;
    }
    if now >= deadline {
        return HbTick::Lapse;
    }
    let dsess = (now - session_start) / 60;
    if let Ok(f) = fuse.parse::<i64>() {
        if !fuse.is_empty() && fuse.chars().all(|c| c.is_ascii_digit()) && f >= wall_min && dsess >= wall_min {
            return HbTick::Thrash;
        }
    }
    HbTick::Ok
}

/// `aeon_disposition`'s note keys (DESIGN.md §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKey {
    Capacity,
    Slain,
    ThrashCharged,
    Thrash,
    Lapsed,
    GateUnfinished,
    DecisionBlocked,
    Timeout,
    Requeue,
    OperatorWait,
    Submitted,
    YieldHeadless,
    PreSession,
    Unlanded,
    NotJudged,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispositionIn {
    /// bd status at teardown (`?` when unreadable).
    pub status: String,
    pub capacity: bool,
    pub slain: bool,
    pub thrash: bool,
    pub thrash_charged: bool,
    pub lapsed: bool,
    pub gate_unfinished: bool,
    pub decision_blocked: bool,
    pub session_rc: i32,
    pub committed: bool,
    pub requeue_cause: Option<String>,
    pub operator_wait: bool,
    pub yield_headless: bool,
    pub session_started: bool,
    pub outcome: Option<String>,
    pub submitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disposition {
    pub ledger_status: String,
    pub charge: bool,
    pub requeue_cause: Option<String>,
    pub note: NoteKey,
}

fn d(status: &str, charge: bool, cause: Option<&str>, note: NoteKey) -> Disposition {
    Disposition { ledger_status: status.to_string(), charge, requeue_cause: cause.map(|s| s.to_string()), note }
}

/// `outcome_charges`: only a session that ran to its own end (`unlanded`) may charge.
pub fn outcome_charges(outcome: &str) -> bool {
    outcome == "unlanded"
}

pub fn disposition(i: &DispositionIn) -> Disposition {
    use NoteKey::*;
    let status = if i.status.is_empty() { "?" } else { i.status.as_str() };
    if i.capacity {
        return d("capacity", false, Some("unjudged-capacity"), Capacity);
    }
    if i.slain {
        return d("slain", false, Some("unjudged-slain"), Slain);
    }
    if i.thrash {
        return if i.thrash_charged {
            d("requeue-thrash-charged", true, Some("thrash-stale"), ThrashCharged)
        } else {
            d("requeue-thrash", false, Some("thrash"), Thrash)
        };
    }
    if i.lapsed {
        return d("lapsed", true, None, Lapsed);
    }
    if i.gate_unfinished {
        return d("gate-unfinished", false, Some("unjudged-gate-unfinished"), GateUnfinished);
    }
    if i.decision_blocked {
        return d("decision-blocked", false, Some("unjudged-decision-blocked"), DecisionBlocked);
    }
    if i.session_rc == 124 && !i.committed {
        return d("timeout", false, Some("unjudged-timeout"), Timeout);
    }
    if let Some(c) = i.requeue_cause.as_deref().filter(|c| !c.is_empty() && *c != "-") {
        return Disposition { ledger_status: format!("requeue-{c}"), charge: false, requeue_cause: Some(c.to_string()), note: Requeue };
    }
    if i.operator_wait {
        return d("operator-wait", false, Some("unjudged-operator-wait"), OperatorWait);
    }
    if i.submitted {
        return d("submitted", false, None, Submitted);
    }
    if i.yield_headless {
        return d("yield-headless", true, None, YieldHeadless);
    }
    if !i.session_started {
        return d("pre-session", true, None, PreSession);
    }
    let outcome = i.outcome.clone().filter(|o| o != "-").unwrap_or_default();
    if outcome_charges(&outcome) {
        d(status, true, None, Unlanded)
    } else {
        Disposition { ledger_status: status.to_string(), charge: false, requeue_cause: Some(format!("unjudged-{outcome}")), note: NotJudged }
    }
}

/// `eviction_reopen` (the pure half; the caller reads land_state and the prior count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eviction {
    None,
    Stale,
    Cap,
    Reopen,
}

/// `ls` is land_state's line: "<state> <tip> <at> [reason]". `eviction_reasons` is
/// LAND_EVICTION_REASONS (space-separated).
pub fn eviction_reopen(ls: &str, cur_tip: &str, recent: i64, eviction_reasons: &str, escalate_at: i64) -> Eviction {
    let mut it = ls.split_whitespace();
    let state = it.next().unwrap_or("");
    let tip = it.next().unwrap_or("");
    let _at = it.next();
    let reason = it.collect::<Vec<_>>().join(" ");
    let is_eviction = state == "EJECTED" || (state == "RED" && eviction_reasons.split_whitespace().any(|r| r == reason));
    if !is_eviction {
        return Eviction::None;
    }
    if !tip.is_empty() && tip != "none" && !cur_tip.is_empty() && tip != cur_tip {
        return Eviction::Stale;
    }
    if recent >= escalate_at {
        return Eviction::Cap;
    }
    Eviction::Reopen
}

/// `sop_rule_verdict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SopVerdict {
    Satisfied,
    Decline,
    Poison,
}

/// Returns (wrote: yes|no|unreadable, verdict). `applied` is `sop log`'s exit code:
/// 0 recorded, 1 read and absent, anything else unreadable.
pub fn sop_rule_verdict(before_ok: bool, before: &str, after_ok: bool, after: &str, applied: i32) -> (&'static str, SopVerdict) {
    let wrote = if before_ok && after_ok {
        let b: Vec<&str> = before.lines().collect();
        if after.lines().filter(|l| !l.is_empty()).any(|l| !b.contains(&l)) {
            "yes"
        } else {
            "no"
        }
    } else {
        "unreadable"
    };
    let v = if wrote == "yes" || applied == 0 {
        SopVerdict::Satisfied
    } else if wrote == "unreadable" || applied != 1 {
        SopVerdict::Decline
    } else {
        SopVerdict::Poison
    };
    (wrote, v)
}

/// `open_ask_blocker <bd-show-json> <bead-id> <ask-label>`: does the bead carry an open,
/// ask-labelled `blocks` dependency? (the teardown's decision_blocked input). Unparseable or
/// empty JSON fails closed — not blocked, proceeds (sp-eq8a4.2.1).
///
/// A RELATES-TO EDGE IS NOT A BLOCKER (sp-dvsqc): mail wires a non-decision cited bead's ask
/// via `dep relate`, dependency_type "relates-to", not "blocks".
///
/// THIS BEAD'S OWN gh-closeout ASK IS NOT ITS OWN BLOCKER (sp-2a4hd): a "Close GitHub issue
/// ... for bead <id>" ask targeting this same bead is excluded, or a reopened bead would
/// decision-block on the ask it is itself waiting to close.
pub fn open_ask_blocker(json: &str, bead_id: &str, ask_label: &str) -> bool {
    let Some(row) = bd::first_row(json) else { return false };
    let close_sfx = (!bead_id.is_empty()).then(|| format!(" for bead {bead_id}"));
    row.dependencies.as_deref().unwrap_or(&[]).iter().any(|d| {
        let open = d.status.as_deref() != Some("closed");
        let has_ask = d.labels.as_deref().unwrap_or(&[]).iter().any(|l| l == ask_label);
        let blocks = d.kind() == Some("blocks");
        let title = d.title.as_deref().unwrap_or("");
        let own_closeout = close_sfx.as_deref().is_some_and(|sfx| title.contains("Close GitHub issue ") && title.contains(sfx));
        open && has_ask && blocks && !own_closeout
    })
}

/// `session_outcome`'s classification over an already-extracted trace segment (lib.sh's
/// "WHAT ENDED THIS SESSION" doc). `None` means the trace file itself is missing or
/// unreadable — `unknown`, never guessed at as a verdict about the work
/// (law-absence-needs-a-positive-control).
///
/// Deterministic and cheap: the presence of one tool call and of a terminal `result` record
/// answers the whole question. Only `unlanded` may charge an attempt (`outcome_charges`).
pub fn session_outcome(segment: Option<&str>) -> &'static str {
    let Some(seg) = segment else { return "unknown" };
    if !seg.lines().any(|l| l.starts_with('{')) {
        return "refused";
    }
    let acted = seg.lines().any(|l| l.contains("\"type\":\"tool_use\""));
    let last = seg.lines().filter(|l| l.contains("\"type\":\"result\"")).last();
    match last {
        None => {
            if acted {
                "killed"
            } else {
                "refused"
            }
        }
        Some(l) if l.contains("\"api_error_status\":null") => {
            if acted {
                "unlanded"
            } else {
                "refused"
            }
        }
        Some(l) if l.contains("\"api_error_status\"") || l.contains("\"error\":\"rate_limit\"") => "refused",
        Some(_) => {
            if acted {
                "unlanded"
            } else {
                "refused"
            }
        }
    }
}

/// `session_yield_headless`: did the session's last turn end waiting for a background task
/// notification with no channel for the wakeup to arrive on (headless has none)? Two ways
/// in: the model's own closing prose says so, OR the trace shows a tool call the harness
/// moved to the background with no later `tool_result` for that same `tool_use_id` (sp-47d49
/// — most sessions cut off this way just end their turn on unrelated text).
pub fn session_yield_headless(segment: &str) -> bool {
    const PATTERNS: &[&str] = &[
        r"background task notification",
        r"background.{0,30}(wait|waiting|woken|wake|notification)",
        r"(wait|waiting).{0,40}background.{0,30}task",
        r"will be woken",
        r"run_in_background",
    ];
    let backgrounded = regex::Regex::new(r"(?i)moved to the background").unwrap();
    let mut last_text = String::new();
    let mut pending: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    for line in segment.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let etype = e.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let content: Vec<serde_json::Value> = e.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()).cloned().unwrap_or_default();
        match etype {
            "assistant" => {
                for c in &content {
                    match c.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = c.get("text").and_then(|t| t.as_str()) {
                                if !t.trim().is_empty() {
                                    last_text = t.to_string();
                                }
                            }
                        }
                        Some("tool_use") => {
                            if let (Some(id), Some(input)) = (c.get("id").and_then(|i| i.as_str()), c.get("input")) {
                                if truthy(input.get("run_in_background")) {
                                    pending.insert(id.to_string(), true);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                for c in &content {
                    if c.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                        continue;
                    }
                    let Some(tid) = c.get("tool_use_id").and_then(|t| t.as_str()) else { continue };
                    let text = match c.get("content") {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(v) => v.to_string(),
                        None => "null".to_string(),
                    };
                    if backgrounded.is_match(&text) {
                        pending.insert(tid.to_string(), true);
                    } else {
                        pending.remove(tid);
                    }
                }
            }
            _ => {}
        }
    }
    if !pending.is_empty() {
        return true;
    }
    if last_text.is_empty() {
        return false;
    }
    PATTERNS.iter().any(|p| regex::Regex::new(&format!("(?is){p}")).unwrap().is_match(&last_text))
}

/// Python truthiness over a JSON value (`None`/`false`/`0`/`""`/`[]`/`{}` are the only falsy
/// shapes) — `run_in_background`'s own test in the original lib.sh helper.
fn truthy(v: Option<&serde_json::Value>) -> bool {
    match v {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(serde_json::Value::Array(a)) => !a.is_empty(),
        Some(serde_json::Value::Object(o)) => !o.is_empty(),
    }
}

/// `rapid_recur_streak <threshold>`: the count of trailing `done` lines (already
/// grep/tail-limited by the caller to one bead's last `<threshold>`) whose `wall_s` reads
/// `?` or under 10 seconds, reset to 0 by any line that does not.
pub fn rapid_recur_streak(lines: &[&str]) -> i64 {
    let re = regex::Regex::new(r"wall_s=(\?|[0-9.]+)").unwrap();
    let mut count = 0i64;
    for line in lines {
        match re.captures(line) {
            Some(m) if &m[1] == "?" || m[1].parse::<f64>().is_ok_and(|f| f < 10.0) => count += 1,
            _ => count = 0,
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_stop_table() {
        assert_eq!(world_stop(false, "aeon-x", false), WorldStop::None);
        assert_eq!(world_stop(true, "aeon-x", false), WorldStop::Refuse);
        assert_eq!(world_stop(true, "aeon-x", true), WorldStop::Stop);
        assert_eq!(world_stop(true, "", false), WorldStop::Stop);
    }

    // test-aeon-lease.sh / test-thrash.sh's hb_tick rows.
    #[test]
    fn hb_tick_table() {
        let t0 = 1_000_000;
        assert_eq!(hb_tick(1, 2, t0, t0 + 600, "0", 20, t0), HbTick::Renew);
        assert_eq!(hb_tick(1, 2, t0 + 700, t0, "99", 20, t0 - 9999), HbTick::Renew, "grew, deadline past, fuse past wall → still renew");
        assert_eq!(hb_tick(1, 1, t0, t0 + 10, "0", 20, t0), HbTick::Ok);
        assert_eq!(hb_tick(1, 1, t0 + 10, t0 + 10, "0", 20, t0), HbTick::Lapse, "now at the deadline");
        assert_eq!(hb_tick(1, 1, t0 + 11, t0 + 10, "0", 20, t0), HbTick::Lapse);
        assert_eq!(hb_tick(1, 1, t0, t0 - 1, "99", 20, t0 - 9999), HbTick::Lapse, "lapse wins over thrash");
        let start = t0 - 20 * 60;
        assert_eq!(hb_tick(1, 1, t0, t0 + 600, "20", 20, start), HbTick::Thrash);
        assert_eq!(hb_tick(1, 1, t0, t0 + 600, "20", 20, t0 - 19 * 60), HbTick::Ok, "session just under the wall");
        assert_eq!(hb_tick(1, 1, t0, t0 + 600, "5", 20, t0 - 30 * 60), HbTick::Ok, "fuse below the wall");
        assert_eq!(hb_tick(1, 1, t0, t0 + 600, "?", 20, t0 - 30 * 60), HbTick::Ok, "probe failed never trips");
        assert_eq!(hb_tick(1, 1, t0, t0 + 600, "gate", 20, t0 - 30 * 60), HbTick::Ok);
        assert_eq!(hb_tick(1, 1, t0, t0 + 600, "43", 20, t0), HbTick::Ok, "old fuse, fresh session");
        assert_eq!(hb_tick(1, 1, t0, t0 + 600, "43", 20, t0 - 21 * 60), HbTick::Thrash);
    }

    fn base() -> DispositionIn {
        DispositionIn { status: "open".into(), session_started: true, outcome: Some("unlanded".into()), ..Default::default() }
    }

    // test-aeon-disposition.sh: one row per precedence level, and precedence itself.
    #[test]
    fn disposition_table() {
        let mut i = base();
        assert_eq!(disposition(&i), d("open", true, None, NoteKey::Unlanded));
        i.outcome = Some("killed".into());
        assert_eq!(disposition(&i), d("open", false, Some("unjudged-killed"), NoteKey::NotJudged));
        i.session_started = false;
        assert_eq!(disposition(&i), d("pre-session", true, None, NoteKey::PreSession));
        i.yield_headless = true;
        assert_eq!(disposition(&i).note, NoteKey::YieldHeadless);
        i.submitted = true;
        assert_eq!(disposition(&i), d("submitted", false, None, NoteKey::Submitted));
        i.operator_wait = true;
        assert_eq!(disposition(&i).note, NoteKey::OperatorWait);
        i.requeue_cause = Some("rebase-conflict".into());
        assert_eq!(disposition(&i), d("requeue-rebase-conflict", false, Some("rebase-conflict"), NoteKey::Requeue));
        i.session_rc = 124;
        assert_eq!(disposition(&i).note, NoteKey::Timeout);
        i.committed = true;
        assert_eq!(disposition(&i).note, NoteKey::Requeue, "a timeout with work committed is not a timeout");
        i.decision_blocked = true;
        assert_eq!(disposition(&i).note, NoteKey::DecisionBlocked);
        i.gate_unfinished = true;
        assert_eq!(disposition(&i).note, NoteKey::GateUnfinished);
        i.lapsed = true;
        assert_eq!(disposition(&i), d("lapsed", true, None, NoteKey::Lapsed));
        i.thrash = true;
        assert_eq!(disposition(&i), d("requeue-thrash", false, Some("thrash"), NoteKey::Thrash));
        i.thrash_charged = true;
        assert_eq!(disposition(&i), d("requeue-thrash-charged", true, Some("thrash-stale"), NoteKey::ThrashCharged));
        i.slain = true;
        assert_eq!(disposition(&i), d("slain", false, Some("unjudged-slain"), NoteKey::Slain));
        i.capacity = true;
        assert_eq!(disposition(&i), d("capacity", false, Some("unjudged-capacity"), NoteKey::Capacity));
    }

    #[test]
    fn disposition_unreadable_status_renders_question_mark() {
        let i = DispositionIn { status: String::new(), ..base() };
        assert_eq!(disposition(&i).ledger_status, "?");
        let r = DispositionIn { requeue_cause: Some("-".into()), ..base() };
        assert_eq!(disposition(&r).note, NoteKey::Unlanded, "`-` is no cause");
    }

    #[test]
    fn eviction_table() {
        let r = "gate-red base_withdrawn";
        assert_eq!(eviction_reopen("CERTIFIED abc 1", "abc", 0, r, 3), Eviction::None);
        assert_eq!(eviction_reopen("EJECTED abc 1 x", "abc", 0, r, 3), Eviction::Reopen);
        assert_eq!(eviction_reopen("RED abc 1 base_withdrawn", "abc", 0, r, 3), Eviction::Reopen);
        assert_eq!(eviction_reopen("RED abc 1 no-rebase@x", "abc", 0, r, 3), Eviction::None);
        assert_eq!(eviction_reopen("EJECTED abc 1", "def", 0, r, 3), Eviction::Stale);
        assert_eq!(eviction_reopen("EJECTED none 1", "def", 0, r, 3), Eviction::Reopen);
        assert_eq!(eviction_reopen("EJECTED abc 1", "", 0, r, 3), Eviction::Reopen, "unknown current tip is not stale");
        assert_eq!(eviction_reopen("EJECTED abc 1", "abc", 3, r, 3), Eviction::Cap);
    }

    #[test]
    fn sop_table() {
        assert_eq!(sop_rule_verdict(true, "a\nb", true, "a\nb\nc", 1), ("yes", SopVerdict::Satisfied));
        assert_eq!(sop_rule_verdict(true, "a\nb", true, "a", 1), ("no", SopVerdict::Poison), "a retirement is not a write");
        assert_eq!(sop_rule_verdict(true, "a", true, "a", 0), ("no", SopVerdict::Satisfied));
        assert_eq!(sop_rule_verdict(false, "", true, "a", 1), ("unreadable", SopVerdict::Decline));
        assert_eq!(sop_rule_verdict(true, "a", true, "a", 2), ("no", SopVerdict::Decline));
    }

    // test-aeon-disposition.sh's open_ask_blocker table.
    #[test]
    fn open_ask_blocker_table() {
        let ask = "needs-operator";
        assert!(open_ask_blocker(r#"[{"dependencies":[{"status":"open","labels":["needs-operator"],"dependency_type":"blocks","title":"decide"}]}]"#, "sp-x", ask), "positive control: an open ask-labelled blocks dep IS a blocker");
        assert!(!open_ask_blocker(r#"[{"dependencies":[{"status":"open","labels":["needs-operator"],"dependency_type":"relates-to","title":"decide"}]}]"#, "sp-x", ask), "sp-dvsqc: relates-to is not a blocker");
        assert!(!open_ask_blocker(r#"[{"dependencies":[{"status":"closed","labels":["needs-operator"],"dependency_type":"blocks","title":"decide"}]}]"#, "sp-x", ask), "a closed ask dep is not a blocker");
        assert!(!open_ask_blocker(r#"[{"dependencies":[{"status":"open","labels":["plan"],"dependency_type":"blocks","title":"decide"}]}]"#, "sp-x", ask), "an open blocks dep with no ask label is not a blocker");
        assert!(!open_ask_blocker(r#"[{"dependencies":[{"status":"open","labels":["needs-operator"],"dependency_type":"blocks","title":"Close GitHub issue 5 for bead sp-x"}]}]"#, "sp-x", ask), "sp-2a4hd: this bead's own gh-closeout ask is not its own blocker");
        assert!(open_ask_blocker(r#"[{"dependencies":[{"status":"open","labels":["needs-operator"],"dependency_type":"blocks","title":"Close GitHub issue 5 for bead sp-OTHER"}]}]"#, "sp-x", ask), "a gh-closeout ask for a DIFFERENT bead IS still a blocker");
        assert!(!open_ask_blocker("", "sp-x", ask), "unparseable JSON fails closed");
    }

    // test-attempts.sh's session_outcome table.
    #[test]
    fn session_outcome_table() {
        assert_eq!(session_outcome(None), "unknown", "a missing/unreadable trace is not classified at all");
        assert_eq!(session_outcome(Some("")), "refused", "an empty trace is a session that never ran");
        assert_eq!(
            session_outcome(Some(
                "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"x\"}\n\
                 {\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"session limit reached\"}]},\"error\":\"rate_limit\"}\n\
                 {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"api_error_status\":429,\"result\":\"session limit reached\"}\n"
            )),
            "refused",
            "a rate-limited session is not an attempt"
        );
        assert_eq!(
            session_outcome(Some(
                "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"x\"}\n\
                 {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{}}]}}\n\
                 {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"api_error_status\":429}\n"
            )),
            "refused",
            "a session refused after it acted is still not an attempt"
        );
        assert_eq!(
            session_outcome(Some(
                "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"x\"}\n\
                 {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{}}]}}\n\
                 {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":40}\n"
            )),
            "unlanded",
            "a session that ran to its own end and left the bead open IS an attempt"
        );
        assert_eq!(
            session_outcome(Some(
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{}}]}}\n\
                 {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"api_error_status\":null}\n"
            )),
            "unlanded",
            "api_error_status:null is an attempt, not a refusal (sp-1g37h)"
        );
        assert_eq!(
            session_outcome(Some(
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{}}]}}\n\
                 {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Now I will\"}]}}\n"
            )),
            "killed",
            "a session killed mid-work is not an attempt"
        );
        assert_eq!(
            session_outcome(Some("{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Let me look.\"}]}}\n")),
            "refused",
            "a truncated trace with no tool call is not an attempt"
        );
    }

    // test-session-yield-headless.sh's phrasing table.
    #[test]
    fn session_yield_headless_table() {
        let msg = |t: &str| format!("{{\"type\":\"assistant\",\"message\":{{\"id\":\"m\",\"content\":[{{\"type\":\"text\",\"text\":{}}}]}}}}\n", serde_json::to_string(t).unwrap());
        assert!(session_yield_headless(&msg("The build is running. I will wait for the background task notification to continue.")), "positive control: the exact phrase yields");
        assert!(!session_yield_headless(&msg("I ran the command. The output looks fine.")), "ordinary unlanded text does not yield");
        assert!(session_yield_headless(&msg("kicked off the job; background is waiting on the runner now")));
        assert!(session_yield_headless(&msg("waiting for a slow background disk task to finish up")));
        assert!(session_yield_headless(&msg("Kicking off the long build now — will be woken when it lands.")));
        assert!(session_yield_headless(&msg("Started it with run_in_background so I can keep going.")));
        assert!(session_yield_headless(&msg("WILL BE WOKEN when the CI run finishes.")), "matching is case-insensitive");
        let two = format!("{}{}", msg("I will wait for the background task notification."), msg("Never mind — I finished the work myself just now."));
        assert!(!session_yield_headless(&two), "only the LAST text governs: an earlier yield phrase is overridden");
        let two_b = format!("{}{}", msg("Kicking off the build now."), msg("I will wait for the background task notification to continue."));
        assert!(session_yield_headless(&two_b), "only the LAST text governs: a later yield phrase fires");
        assert!(!session_yield_headless("{\"type\":\"assistant\",\"message\":{\"id\":\"m\",\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"true\"}}]}}\n"), "an assistant message with no text content does not yield");
        assert!(!session_yield_headless(""), "an empty trace does not yield");

        // sp-47d49: a backgrounded tool call with no later tool_result yields even when the
        // model's closing prose never mentions waiting.
        let backgrounded = "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"content\":[{\"type\":\"text\",\"text\":\"Running the test suite now.\"},{\"type\":\"tool_use\",\"id\":\"toolu_bg1\",\"name\":\"Bash\",\"input\":{\"command\":\"x\",\"timeout\":1800000}}]}}\n\
             {\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_bg1\",\"content\":\"Command running in the background with ID bash_1. Command was moved to the background because it exceeded the 120000ms timeout. You will be notified when it completes.\"}]}}\n";
        assert!(session_yield_headless(backgrounded), "trace-based detector catches a backgrounded tool_use with no later tool_result");

        let resolved = "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_bg2\",\"name\":\"Bash\",\"input\":{\"command\":\"x\",\"timeout\":1800000}}]}}\n\
             {\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_bg2\",\"content\":\"Command running in the background with ID bash_2. Moved to the background.\"}]}}\n\
             {\"type\":\"assistant\",\"message\":{\"id\":\"m2\",\"content\":[{\"type\":\"text\",\"text\":\"Finished up.\"}]}}\n\
             {\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_bg2\",\"content\":\"exit 0\\nall tests passed\"}]}}\n";
        assert!(!session_yield_headless(resolved), "a backgrounded call later resolved by a tool_result for the same id does not yield");
    }

    // test-rapid-recur.sh's rapid_recur_streak table.
    #[test]
    fn rapid_recur_streak_table() {
        let done = |wall_s: &str| format!("2026-09-27T00:00:00Z done f b rc=0 status=unlanded wall_s={wall_s} api_s=1 turns=1 in_tok=1 cache_read_tok=0 out_tok=1 think_tok=0 cost_usd=0.01");
        let (a, b, c) = (done("1"), done("2.5"), done("9.9"));
        assert_eq!(rapid_recur_streak(&[&a, &b, &c]), 3, "three consecutive sub-10s lines streak to 3");
        let q = done("?");
        assert_eq!(rapid_recur_streak(&[&q, &q]), 2, "a wall_s=? line still counts (missing spend, never a free pass)");
        let real = done("90");
        assert_eq!(rapid_recur_streak(&[&a, &real]), 0, "a real run (wall_s>=10) resets the streak to 0");
        assert_eq!(rapid_recur_streak(&[&real, &a]), 1, "the streak resets AFTER the real run, not before it");
    }
}
