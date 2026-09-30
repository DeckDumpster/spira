//! The pure decisions (DESIGN.md §4): each was a lib.sh function aeon.sh called with
//! gathered inputs, each is now a Rust function with its table as a test.

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

/// `close_verdict`'s "outcome|reason|msg".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseVerdict {
    pub outcome: String,
    pub reason: String,
    pub msg: String,
}

pub fn parse_close_verdict(s: &str) -> CloseVerdict {
    let s = s.trim_end_matches('\n');
    let (outcome, rest) = s.split_once('|').unwrap_or((s, s));
    let (reason, msg) = rest.split_once('|').unwrap_or((rest, rest));
    CloseVerdict { outcome: outcome.into(), reason: reason.into(), msg: msg.into() }
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

    #[test]
    fn close_verdict_parses() {
        let v = parse_close_verdict("reopen|delivers-mismatch|no note found\n");
        assert_eq!((v.outcome.as_str(), v.reason.as_str(), v.msg.as_str()), ("reopen", "delivers-mismatch", "no note found"));
        let k = parse_close_verdict("keep|superseded|");
        assert_eq!((k.outcome.as_str(), k.reason.as_str()), ("keep", "superseded"));
    }
}
