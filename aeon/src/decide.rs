//! The pure decisions (DESIGN.md §4): each was a lib.sh function aeon.sh called with
//! gathered inputs, each is now a Rust function with its table as a test.

use crate::bd;
use spira_config::nonwork;

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
    NoProgress,
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
    /// Did THIS session's own turn move the branch tip (sp-1zxru-2)? Distinct from
    /// `committed` (a commit naming this bead ANYWHERE in the window — the right question
    /// for the close/SOP/groom/eviction checks, the wrong one here): a branch an earlier
    /// session already moved reads `committed` forever, so the Unlanded/NoProgress split
    /// needs its own, narrower answer or a bead resumed enough times after a single real
    /// commit never again sees a no-progress exit (sp-iku03, sp-al5ng).
    pub tip_moved: bool,
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

/// `outcome_charges`: only a session that ran to its own end (`unlanded`) may charge —
/// and even then, only once [`disposition`] also finds a commit (see [`NoteKey::NoProgress`]).
pub fn outcome_charges(outcome: &str) -> bool {
    outcome == CHARGING_OUTCOME
}

/// The one outcome that charges; census reads it to rank only `unjudged-<it>` among the
/// `unjudged-*` causes, every other of which `disposition` declares free.
pub const CHARGING_OUTCOME: &str = "unlanded";

/// The exempt requeue cause an `unlanded`-but-uncommitted exit writes (sp-1zxru):
/// `events::is_legacy_exempt` reads the `unjudged-` prefix the same way every other
/// never-judged disposition's cause already does, so this return is never counted toward
/// the poison threshold and CHECK 4 never files its "N attempts" ask over it.
pub const NO_PROGRESS_CAUSE: &str = "unjudged-no-progress";

/// Did THIS session's own turn move the branch tip (sp-1zxru-2)? Both reads are full
/// hashes; `""` (never read) and `"?"` (the git read failed) are both "cannot tell", which
/// this treats as NOT moved — fail closed toward the hold, not toward silently resuming
/// through a signal that never answered the question. A branch an earlier session already
/// advanced, with nothing added since, answers `false` here even though a commit naming
/// the bead sits on it somewhere — that is exactly the case `committed` (`verdict_committed`)
/// cannot distinguish from real progress this session made (sp-iku03, sp-al5ng).
pub fn tip_moved(start: &str, now: &str) -> bool {
    !start.is_empty() && start != "?" && !now.is_empty() && now != "?" && start != now
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
        // sp-1zxru-2: THIS session's own tip movement, not `committed` (which answers
        // "is there a commit for this bead anywhere in the window" — true forever once any
        // session ever commits, even if every session since has moved nothing).
        if i.tip_moved {
            d(status, true, None, Unlanded)
        } else {
            // sp-1zxru: the session ran to its own end and left the bead in_progress, but
            // nothing it did moved the branch — no new commit naming this bead. That is
            // the no-progress exit (law-attempts-count-the-harness: a retry that cannot
            // change an input is the harness's loop, not the work's failure), not a
            // judged attempt. teardown.rs holds it for a backoff instead of resuming it.
            d(status, false, Some(NO_PROGRESS_CAUSE), NoProgress)
        }
    } else {
        Disposition { ledger_status: status.to_string(), charge: false, requeue_cause: Some(format!("unjudged-{outcome}")), note: NotJudged }
    }
}

/// Did the session's builder hand its bead on by closing it — the legacy close path the
/// verdict fences (sp-mve9i; teardown's own closed branch is deleted, sp-v62vn)? Read from the bead's
/// lifecycle row, never bd's `status` (design §3.4: bd status is inert for work beads).
///
/// A restricted session (every session the aeon launches) hands its bead on only through the work verbs
/// (`work submit`/`done`), which teardown's disposition reads as `submitted`
/// (`lc_bead_verified`); bd's status never moved for it, so it never took this path and
/// still does not. An unrestricted session's close is the row past the builder
/// (`lc_state::past_builder`). No row is not a close: the conservative answer, which sends
/// the bead through the disposition (release, never a reopen).
pub fn builder_closed(restricted: bool, row: Option<&spira_config::lc_state::Row>) -> bool {
    !restricted && row.is_some_and(|r| r.past_builder())
}

/// The status word the aeon ledger has always recorded (`done … status=<word>`), from the
/// lifecycle row: `?` when there is none.
pub fn ledger_word(row: Option<&spira_config::lc_state::Row>) -> &'static str {
    match row {
        None => "?",
        Some(r) if r.working() => "in_progress",
        Some(r) if r.claimable() => "open",
        Some(r) if r.past_builder() => "closed",
        Some(_) => "?",
    }
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
        // An ask is not a work bead: bd's status is its only state (spira_config::nonwork).
        let open = !nonwork::is_closed(nonwork::Kind::Ask, d.status.as_deref().unwrap_or(""));
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

/// The session's own last word (sp-1zxru): the last non-empty assistant text block in the
/// trace segment, trimmed. Used only to label a no-progress exit with the aeon's own
/// one-line reason — never to decide anything (that stays [`session_outcome`]'s).
/// `None` for a segment with no readable assistant text at all (missing trace, pure tool
/// noise, or nothing parses).
pub fn last_assistant_text(segment: Option<&str>) -> Option<String> {
    let seg = segment?;
    let mut last = String::new();
    for line in seg.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if e.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let content: Vec<serde_json::Value> = e.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()).cloned().unwrap_or_default();
        for c in &content {
            if c.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = c.get("text").and_then(|t| t.as_str()) {
                    if !t.trim().is_empty() {
                        last = t.trim().to_string();
                    }
                }
            }
        }
    }
    (!last.is_empty()).then_some(last)
}

/// The no-progress backoff window (sp-1zxru): starts at 30 minutes and doubles with every
/// consecutive no-progress exit the streak counts at the same branch tip ([`thrash_streak_bump`
/// in counters.rs] drives the streak; this is pure arithmetic over its result, capped at
/// 2^6 so a runaway streak before the poison cap still returns a plain number). The caller
/// stops calling this at all once the streak reaches `SPIRA_THRASH_STREAK_CAP` — see
/// teardown.rs's `NoteKey::NoProgress`.
pub fn no_progress_backoff_minutes(streak: i64) -> i64 {
    let shift = streak.saturating_sub(1).clamp(0, 6) as u32;
    30i64 << shift
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
        assert_eq!(disposition(&i), d("open", false, Some(NO_PROGRESS_CAUSE), NoteKey::NoProgress), "unlanded with no commit is a no-progress exit, not charged (sp-1zxru)");
        i.committed = true; // ambient: a commit exists somewhere in the window, but not from THIS session
        assert_eq!(disposition(&i), d("open", false, Some(NO_PROGRESS_CAUSE), NoteKey::NoProgress), "sp-1zxru-2: committed alone (an earlier session's work) is still no-progress — tip_moved is the question, not committed");
        i.tip_moved = true;
        assert_eq!(disposition(&i), d("open", true, None, NoteKey::Unlanded), "a real failed attempt — THIS session moved the tip, still open — still counts");
        i.committed = false;
        i.tip_moved = false;
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
        let r = DispositionIn { requeue_cause: Some("-".into()), tip_moved: true, ..base() };
        assert_eq!(disposition(&r).note, NoteKey::Unlanded, "`-` is no cause");
    }

    // test-aeon-disposition.sh's open_ask_blocker table.
    #[test]
    fn open_ask_blocker_table() {
        // literal-ok: a test fixture's own ask label, substituted into every row below so the literal appears exactly once, here.
        let ask = "needs-operator";
        let row = |status: &str, label: &str, kind: &str, title: &str| format!(r#"[{{"dependencies":[{{"status":"{status}","labels":["{label}"],"dependency_type":"{kind}","title":"{title}"}}]}}]"#);
        assert!(open_ask_blocker(&row("open", ask, "blocks", "decide"), "sp-x", ask), "positive control: an open ask-labelled blocks dep IS a blocker");
        assert!(!open_ask_blocker(&row("open", ask, "relates-to", "decide"), "sp-x", ask), "sp-dvsqc: relates-to is not a blocker");
        assert!(!open_ask_blocker(&row("closed", ask, "blocks", "decide"), "sp-x", ask), "a closed ask dep is not a blocker");
        assert!(!open_ask_blocker(&row("open", "plan", "blocks", "decide"), "sp-x", ask), "an open blocks dep with no ask label is not a blocker");
        assert!(!open_ask_blocker(&row("open", ask, "blocks", "Close GitHub issue 5 for bead sp-x"), "sp-x", ask), "sp-2a4hd: this bead's own gh-closeout ask is not its own blocker");
        assert!(open_ask_blocker(&row("open", ask, "blocks", "Close GitHub issue 5 for bead sp-OTHER"), "sp-x", ask), "a gh-closeout ask for a DIFFERENT bead IS still a blocker");
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

    // ---- sp-1zxru: no-progress exits ----------------------------------------------------

    #[test]
    fn last_assistant_text_takes_the_last_nonempty_block() {
        assert_eq!(last_assistant_text(None), None, "no trace at all");
        assert_eq!(last_assistant_text(Some("")), None, "empty trace");
        assert_eq!(last_assistant_text(Some("not json\nnot json either")), None);
        let seg = "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first\"}]}}\n\
                   {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\"}]}}\n\
                   {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Nothing has changed since the last check, so I'm leaving the bead open with a note\"}]}}\n";
        assert_eq!(last_assistant_text(Some(seg)), Some("Nothing has changed since the last check, so I'm leaving the bead open with a note".to_string()));
        let blank = "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"kept\"}]}}\n\
                     {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"   \"}]}}\n";
        assert_eq!(last_assistant_text(Some(blank)), Some("kept".to_string()), "a later blank text block does not overwrite a real one");
    }

    #[test]
    fn no_progress_backoff_minutes_doubles_from_thirty() {
        assert_eq!(no_progress_backoff_minutes(1), 30, "first no-progress exit: 30m");
        assert_eq!(no_progress_backoff_minutes(2), 60);
        assert_eq!(no_progress_backoff_minutes(3), 120);
        assert_eq!(no_progress_backoff_minutes(0), 30, "a streak of 0 (or less) still floors at 30m");
        assert_eq!(no_progress_backoff_minutes(-5), 30);
        assert_eq!(no_progress_backoff_minutes(100), 30 * (1 << 6), "the shift is capped so a huge streak is still a plain number");
    }

    // sp-1zxru-2: THIS session's own tip movement — a branch an earlier session already
    // advanced, with nothing added since, must answer false (sp-iku03, sp-al5ng), not
    // "cannot tell" folded into true the way a bare string-equality check would if either
    // side were simply unset.
    #[test]
    fn tip_moved_table() {
        assert!(tip_moved("abc123", "def456"), "a real move");
        assert!(!tip_moved("abc123", "abc123"), "same tip start to end — nothing landed this session");
        assert!(!tip_moved("", "def456"), "start never read — cannot tell, not moved");
        assert!(!tip_moved("abc123", ""), "now never read — cannot tell, not moved");
        assert!(!tip_moved("?", "def456"), "start read failed — cannot tell, not moved");
        assert!(!tip_moved("abc123", "?"), "now read failed — cannot tell, not moved");
        assert!(!tip_moved("", ""), "neither read — cannot tell, not moved");
        assert!(!tip_moved("?", "?"), "both reads failed — cannot tell, not moved");
    }
}
