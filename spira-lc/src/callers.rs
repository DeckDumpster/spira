//! The caller verbs (DESIGN.md §2): the one-call operations every harness caller used to get
//! from three sourced shell libraries — `lc.sh` (holds, releases, drops, the bulk lists),
//! `lc-delivery.sh` (the pr/push delivery exits) and `lifecycle-cert.sh` (a gate verdict
//! onto the bead machine). Each verb is a composition of the primitive `show` / `list` /
//! `event` verbs, run CLIENT-SIDE: it reads the row, reasons from that one read, and applies
//! one event under the row's own compare-and-swap, exactly as the shell did.
//!
//! Client-side on purpose. The primitives are what the `serve` daemon speaks; composing here
//! means a daemon from an older release still answers a newer client, and the one verb that
//! needs git (`deliver pr-merged`'s merge-tree proof) runs as the caller, in the caller's
//! checkout — never as the service user.
//!
//! There is no off mode (sp-v62vn): every verb reaches the machine.

use std::collections::{BTreeMap, BTreeSet};

use lifecycle::bead::{AeonPhase, BeadEventKind, BeadState, BreakGlass, DispositionStatus, HoldKind};
use lifecycle::delivery::DeliveryEventKind;
use lifecycle::reason::{DropReason, GateRedReason, HoldCause, ReturnedReason};
use serde_json::Value;
use spira_config::lc_state;

/// Exit codes, the shell library's own: 0 applied · 1 no row · 2 cannot tell · 3 refused.
pub const APPLIED: i32 = 0;
pub const NO_ROW: i32 = 1;
pub const CANNOT_TELL: i32 = 2;
pub const REFUSED: i32 = 3;

/// The primitive verbs, however they are reached (socket, direct connection, a test's
/// in-memory machine). `args` is exactly the argv `spira-lc` would be given.
pub trait Machine {
    fn call(&mut self, args: &[String]) -> (i32, String);
}

/// What a caller verb answers.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    /// `(verb, detail)` for `$SPIRA_RUN/lifecycle-cert.log` — certify/resubmit only, the
    /// log lifecycle-cert.sh kept. Written by `main`, which owns the clock and the path.
    pub cert_log: Option<(String, String)>,
}

impl Answer {
    fn code(code: i32) -> Self {
        Answer { code, ..Default::default() }
    }
    fn out(code: i32, stdout: String) -> Self {
        Answer { code, stdout, ..Default::default() }
    }
    fn cert(code: i32, verb: &str, detail: String) -> Self {
        Answer { code, cert_log: Some((verb.to_string(), detail)), ..Default::default() }
    }
}

/// Every caller verb. `main` routes these here, never to `dispatch`.
pub const VERBS: &[&str] = &[
    "hold",
    "unhold",
    "reply",
    "withdraw-ask",
    "release",
    "holder-dead",
    "drop",
    "returned",
    "content-on-base",
    "supersede",
    "state",
    "holds",
    "held",
    "list-held",
    "list-state",
    "list-all",
    "deliver",
    "certify",
    "resubmit",
    "retip",
    "absolve",
    "renew",
    "phase",
    "disposition",
    "express",
    "unexpress",
];

pub fn is_verb(v: &str) -> bool {
    VERBS.contains(&v)
}

/// Run one caller verb.
pub fn run(verb: &str, args: &[String], m: &mut dyn Machine) -> Answer {
    let a = |i: usize| args.get(i).cloned().unwrap_or_default();
    let need = |n: usize| args.len() >= n && args[..n].iter().all(|s| !s.is_empty());
    match verb {
        "hold" => {
            let (pos, reason, review_by) = match hold_flags(args) {
                Ok(x) => x,
                Err(a) => return a,
            };
            if pos.len() < 2 || pos[..2].iter().any(|s| s.is_empty()) {
                return usage("hold <bead-id> <poison|ask|wait|manual> [cause] [actor] [--reason <text>] [--review-by <ISO date>]\n  a timed snooze is: hold <bead-id> wait \"snooze-until:<epoch seconds>[ explanation]\"\n  with --reason the cause is the flag and the third positional is the actor");
            }
            let at = |i: usize| pos.get(i).cloned().unwrap_or_default();
            let (cause, actor_ix) = match reason {
                Some(r) => (r, 2),
                None => (at(2), 3),
            };
            let cause = match review_by {
                Some(d) if cause.is_empty() => format!("{REVIEW_BY_PREFIX}{d}"),
                Some(d) => format!("{REVIEW_BY_PREFIX}{d} {cause}"),
                None => cause,
            };
            hold(m, &at(0), &at(1), &cause, &actor_or(pos.get(actor_ix), "sentinel"))
        }
        "unhold" => {
            let (pos, glass) = match unhold_flags(args) {
                Ok(x) => x,
                Err(a) => return a,
            };
            if pos.len() < 2 || pos[..2].iter().any(|s| s.is_empty()) {
                return usage("unhold <bead-id> <kind> [actor] [--reason <why> --evidence <checks run on the tip> --evidence-tip <tip>]\n  lifting the manual hold on a REWORK bead whose tip a gate or round ejected as red is break-glass: it needs all three flags");
            }
            let actor = actor_or(pos.get(2), "sentinel");
            with_row(m, &pos[0], &actor, |_| match HoldKind::from_str(&pos[1]) {
                Some(k) => Ok(BeadEventKind::Unhold { kind: k, break_glass: glass }),
                None => Err(unknown_kind(&pos[1])),
            })
        }
        "reply" => {
            if !need(2) {
                return usage("reply <bead-id> <message-id|seq:<hold event seq>> [actor]");
            }
            let message_id = a(1);
            if let Some(seq) = message_id.strip_prefix("seq:") {
                if !hold_event_exists(m, &a(0), seq) {
                    return Answer { code: REFUSED, stderr: format!("lc: {} has no Hold event with seq {seq} — nothing sent\n", a(0)), ..Default::default() };
                }
            }
            with_row(m, &a(0), &actor_or(args.get(2), "sentinel"), |_| Ok(BeadEventKind::Reply { message_id: message_id.clone() }))
        }
        "withdraw-ask" => {
            if !need(1) {
                return usage("withdraw-ask <bead-id> [actor]");
            }
            with_row(m, &a(0), &actor_or(args.get(1), "sentinel"), |_| Ok(BeadEventKind::AskWithdrawn))
        }
        "release" => {
            if !need(1) {
                return usage("release <bead-id> [actor]");
            }
            with_row(m, &a(0), &actor_or(args.get(1), "sentinel"), |_| Ok(BeadEventKind::Release))
        }
        "express" | "unexpress" => {
            if !need(1) {
                return usage(&format!("{verb} <bead-id> [actor]"));
            }
            let kind = if verb == "express" { BeadEventKind::Express } else { BeadEventKind::Unexpress };
            with_row(m, &a(0), &actor_or(args.get(1), "bead"), |_| Ok(kind.clone()))
        }
        "holder-dead" => {
            if !need(1) {
                return usage("holder-dead <bead-id> [actor]");
            }
            with_row(m, &a(0), &actor_or(args.get(1), "sentinel"), |_| Ok(BeadEventKind::HolderDead))
        }
        // DropReason is a closed enum: the one caller (slay.sh --close) is always the
        // operator's own policy call. The caller's prose stays in its own bd note.
        "drop" => {
            if !need(2) {
                return usage("drop <bead-id> <reason> [actor]");
            }
            with_row(m, &a(0), &actor_or(args.get(2), "sentinel"), |_| Ok(BeadEventKind::Drop { reason: DropReason::Unwanted }))
        }
        // ReturnedReason likewise: the one caller (queue eject) is always batch-ejected.
        "returned" => {
            if !need(2) {
                return usage("returned <bead-id> <reason> [actor]");
            }
            with_row(m, &a(0), &actor_or(args.get(2), "sentinel"), |_| {
                Ok(BeadEventKind::Returned { reason: ReturnedReason::BatchEjected })
            })
        }
        "content-on-base" => {
            if !need(2) {
                return usage("content-on-base <bead-id> <proof> [actor]");
            }
            let proof = a(1);
            with_row(m, &a(0), &actor_or(args.get(2), "sending"), |_| Ok(BeadEventKind::ContentOnBase { proof: proof.clone() }))
        }
        "supersede" => {
            if !need(2) {
                return usage("supersede <bead-id> <successor-id> [actor]");
            }
            let by = a(1);
            with_row(m, &a(0), &actor_or(args.get(2), "groomer"), |_| Ok(BeadEventKind::Supersede { by: by.clone() }))
        }
        "state" => {
            if !need(1) {
                return usage("state <bead-id>");
            }
            match show(m, &a(0)) {
                Err(rc) => Answer::code(rc),
                Ok(v) => match bead_field(&v, "state") {
                    s if s.is_empty() => Answer::code(NO_ROW),
                    s => Answer::out(0, s),
                },
            }
        }
        "holds" => {
            if !need(1) {
                return usage("holds <bead-id>");
            }
            match show(m, &a(0)) {
                Err(_) => Answer::code(0),
                Ok(v) => Answer::out(0, holds_of(v.get("bead").and_then(|b| b.get("holds"))).join("\n")),
            }
        }
        "held" => {
            if !need(2) {
                return usage("held <bead-id> <kind>");
            }
            let kind = a(1);
            match show(m, &a(0)) {
                Ok(v) if holds_of(v.get("bead").and_then(|b| b.get("holds"))).contains(&kind) => Answer::code(0),
                _ => Answer::code(1),
            }
        }
        "list-held" => {
            if !need(1) {
                return usage("list-held <kind>");
            }
            list(m, &["list".into(), "--live".into(), "--hold".into(), a(0)], |r| s(r, "bead_id"))
        }
        "list-state" => {
            if !need(1) {
                return usage("list-state <state>");
            }
            list(m, &["list".into(), "--state".into(), a(0)], |r| {
                format!("{}\t{}\t{}", s(r, "bead_id"), s(r, "lease_until"), holds_of(r.get("holds")).join(","))
            })
        }
        "list-all" => list(m, &["list".into()], |r| format!("{}\t{}\t{}", s(r, "bead_id"), s(r, "state"), s(r, "holder"))),
        "deliver" => deliver(args, m),
        "certify" => {
            if !need(3) {
                return usage("certify <bead-id> <tip> <pass|red|infra> [detail] [actor]");
            }
            certify(m, &a(0), &a(1), &a(2), &a(3), &actor_or(args.get(4), "lifecycle-cert"))
        }
        "resubmit" => {
            if !need(2) {
                return usage("resubmit <bead-id> <tip> [actor]");
            }
            resubmit(m, &a(0), &a(1), &actor_or(args.get(2), "lifecycle-cert"))
        }
        "retip" => {
            if !need(2) {
                return usage("retip <bead-id> <tip> <actor> <reason> <evidence> <evidence-tip>");
            }
            let glass = BreakGlass { reason: a(3), evidence: a(4), evidence_tip: a(5) };
            retip(m, &a(0), &a(1), &a(2), glass)
        }
        "absolve" => {
            if !need(3) {
                return usage("absolve <bead-id> <tip> <actor> <reason> <evidence> <evidence-tip>");
            }
            let glass = BreakGlass { reason: a(3), evidence: a(4), evidence_tip: a(5) };
            absolve(m, &a(0), &a(1), &a(2), glass)
        }
        "renew" => renew(m, args),
        "phase" => phase(m, args),
        "disposition" => disposition(m, args),
        other => usage(&format!("unknown caller verb {other:?}")),
    }
}

/// The reason prefix the bead-sweep reads as a manual hold's review date.
const REVIEW_BY_PREFIX: &str = "review-by:";

/// Split `--reason` / `--evidence` / `--evidence-tip` out of an unhold's argv.
fn unhold_flags(args: &[String]) -> Result<(Vec<String>, BreakGlass), Answer> {
    let (mut pos, mut glass) = (Vec::new(), BreakGlass::default());
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--reason" | "--evidence" | "--evidence-tip" => {
                let Some(v) = it.next().filter(|v| !v.is_empty()) else {
                    return Err(usage(&format!("unhold: {x} needs a value")));
                };
                match x.as_str() {
                    "--reason" => glass.reason = v.clone(),
                    "--evidence" => glass.evidence = v.clone(),
                    _ => glass.evidence_tip = v.clone(),
                }
            }
            _ => pos.push(x.clone()),
        }
    }
    Ok((pos, glass))
}

type HoldArgs = (Vec<String>, Option<String>, Option<String>);

/// Split `--reason` / `--review-by` out of a hold's argv; what remains is positional.
fn hold_flags(args: &[String]) -> Result<HoldArgs, Answer> {
    let (mut pos, mut reason, mut review_by) = (Vec::new(), None, None);
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--reason" | "--review-by" => {
                let Some(v) = it.next().filter(|v| !v.is_empty()) else {
                    return Err(usage(&format!("hold: {x} needs a value")));
                };
                if x == "--reason" {
                    reason = Some(v.clone());
                } else if !is_iso_date(v) {
                    return Err(Answer { code: REFUSED, stderr: format!("lc: --review-by {v:?} is not an ISO date (YYYY-MM-DD[THH:MM:SSZ]); no hold was applied\n"), ..Default::default() });
                } else {
                    review_by = Some(v.clone());
                }
            }
            _ => pos.push(x.clone()),
        }
    }
    Ok((pos, reason, review_by))
}

fn is_iso_date(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() >= 10
        && b[..10].iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
        && !v.contains(char::is_whitespace)
}

fn hold_event_exists(m: &mut dyn Machine, id: &str, seq: &str) -> bool {
    let (rc, out) = m.call(&["history".into(), id.into(), "--machine".into(), "bead".into()]);
    if rc != 0 {
        return false;
    }
    let v: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
    v.as_array().is_some_and(|rows| rows.iter().any(|r| s(r, "seq") == seq && s(r, "event") == "Hold"))
}

fn usage(what: &str) -> Answer {
    Answer { code: CANNOT_TELL, stderr: format!("usage: spira-lc {what}\n"), ..Default::default() }
}

fn actor_or(v: Option<&String>, default: &str) -> String {
    match v {
        Some(s) if !s.is_empty() => s.clone(),
        _ => default.to_string(),
    }
}

fn unknown_kind(kind: &str) -> Answer {
    Answer { code: CANNOT_TELL, stderr: format!("lc: unknown hold kind '{kind}'\n"), ..Default::default() }
}

/// A string field of a `dolt -r json` row: strings as they are, numbers as their text,
/// null or absent as empty.
fn s(row: &Value, k: &str) -> String {
    match row.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(x) => x.to_string(),
    }
}

fn bead_field(v: &Value, k: &str) -> String {
    v.get("bead").map(|b| s(b, k)).unwrap_or_default()
}

/// `holds` is a JSON column, which `dolt -r json` hands back as JSON TEXT (rows.rs's own
/// `json_col` double-parses it too); a directly-nested array is accepted as well.
fn holds_of(v: Option<&Value>) -> Vec<String> {
    let parsed = match v {
        Some(Value::String(t)) if !t.is_empty() => serde_json::from_str(t).unwrap_or(Value::Null),
        Some(x) => x.clone(),
        None => Value::Null,
    };
    parsed.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// `spira-lc show <id>` parsed. Err(rc): 1 no row, 2 cannot tell (or an unparseable answer).
fn show(m: &mut dyn Machine, id: &str) -> Result<Value, i32> {
    let (rc, out) = m.call(&["show".into(), id.into()]);
    if rc != 0 {
        return Err(rc);
    }
    serde_json::from_str(out.trim()).map_err(|_| CANNOT_TELL)
}

fn list(m: &mut dyn Machine, args: &[String], line: impl Fn(&Value) -> String) -> Answer {
    let (rc, out) = m.call(args);
    if rc != 0 {
        return Answer::code(0);
    }
    let v: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
    let lines: Vec<String> = v.as_array().map(|a| a.iter().map(&line).collect()).unwrap_or_default();
    Answer::out(0, lines.join("\n"))
}

/// Apply one bead event under the CAS of the row as read. The event may depend on the row
/// (certify's tip invariant); `build` returning Err is a caller error, answered as given.
const ACTOR_MAX: usize = 128;

fn with_row(
    m: &mut dyn Machine,
    id: &str,
    actor: &str,
    build: impl FnOnce(&Value) -> Result<BeadEventKind, Answer>,
) -> Answer {
    if actor.chars().count() > ACTOR_MAX || actor.chars().any(char::is_whitespace) {
        return Answer {
            code: REFUSED,
            stderr: format!("lc: the actor must be one word of at most {ACTOR_MAX} characters, not free text (the verb carries no message text) — nothing sent\n"),
            ..Default::default()
        };
    }
    let v = match show(m, id) {
        Ok(v) => v,
        Err(rc) => return Answer::code(rc),
    };
    let (state, version) = (bead_field(&v, "state"), bead_field(&v, "version"));
    if state.is_empty() || version.is_empty() {
        return Answer::code(NO_ROW);
    }
    let kind = match build(&v) {
        Ok(k) => k,
        Err(a) => return a,
    };
    if let Some(why) = not_admitted(&kind, &state, &holds_of(v.get("bead").and_then(|b| b.get("holds")))) {
        return Answer { code: REFUSED, stderr: format!("lc: {id} is {state}; {why} — nothing sent\n"), ..Default::default() };
    }
    let (code, text) = event(m, "bead", id, &state, &version, actor, &serde_json::to_string(&kind).unwrap_or_default());
    Answer { code, stderr: if code == 0 { String::new() } else { text }, ..Default::default() }
}

/// Why the state just read cannot take `kind`, for the events whose writers act on a read
/// that may predate a move; None when the machine admits it (or the writer is not one of those).
fn not_admitted(kind: &BeadEventKind, state: &str, holds: &[String]) -> Option<&'static str> {
    match kind {
        BeadEventKind::Reply { .. } | BeadEventKind::AskWithdrawn if !holds.iter().any(|h| h == "ask") => Some("it holds no ask"),
        BeadEventKind::HolderDead if state != "WORKING" => Some("it has no holder to reap"),
        BeadEventKind::Phase { .. } if BeadState::from_str(state).is_some_and(BeadState::is_terminal) => Some("it is already closed"),
        _ => None,
    }
}

fn event(m: &mut dyn Machine, machine: &str, id: &str, expect: &str, version: &str, actor: &str, kind: &str) -> (i32, String) {
    m.call(&[
        "event".into(),
        machine.into(),
        id.into(),
        "--expect".into(),
        expect.into(),
        "--version".into(),
        version.into(),
        "--actor".into(),
        actor.into(),
        "--kind".into(),
        kind.into(),
    ])
}

/// HoldCause is a closed enum — the category a hold kind already implies; the caller's own
/// prose reaches the row as the event's `detail`.
fn hold_cause(k: HoldKind) -> HoldCause {
    match k {
        HoldKind::Poison => HoldCause::AttemptsExhausted,
        HoldKind::Ask => HoldCause::OperatorQuestion,
        HoldKind::Wait => HoldCause::UnlandedBlocker,
        HoldKind::Manual => HoldCause::ManualHold,
    }
}

/// A timed snooze is a backoff, not a blocker: only a wait the caller gave no snooze is
/// `unlanded-blocker`.
fn hold_cause_for(k: HoldKind, reason: &str) -> HoldCause {
    if k == HoldKind::Wait && reason.starts_with(lc_state::SNOOZE_PREFIX) {
        return HoldCause::NoProgressBackoff;
    }
    hold_cause(k)
}

fn hold(m: &mut dyn Machine, id: &str, kind: &str, cause: &str, actor: &str) -> Answer {
    with_row(m, id, actor, |_| match HoldKind::from_str(kind) {
        Some(HoldKind::Wait) if lc_state::names_directive(cause) && lc_state::snooze_until(cause).is_none() => Err(Answer {
            code: REFUSED,
            stderr: format!("lc: wait hold reason {cause:?} is not understood; a timed snooze is \"snooze-until:<epoch seconds>[ explanation]\"; no hold was applied\n"),
            ..Default::default()
        }),
        Some(k) => Ok(BeadEventKind::Hold { kind: k, cause: hold_cause_for(k, cause), detail: Some(cause.to_string()) }),
        None => Err(unknown_kind(kind)),
    })
}

// ---- delivery exits (lc-delivery.sh) -------------------------------------------------

fn deliver_actor(sub: &str) -> &'static str {
    match sub {
        "pr-merged" | "pr-closed" => "pr-pass-branch",
        _ => "landing.sh",
    }
}

/// lib.sh `log`: the line format every harness log reader already parses.
pub fn log_line(msg: &str) -> String {
    format!("{} spira: {msg}\n", utc_now())
}

fn utc_now() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    fmt_utc(t)
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` for an epoch second.
pub fn fmt_utc(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, (secs % 3600) / 60, secs % 60)
}

fn deliver(args: &[String], m: &mut dyn Machine) -> Answer {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let a = |i: usize| args.get(i).cloned().unwrap_or_default();
    let (id, want, kind) = match sub {
        // The proof is a merge-tree comparison against the merge commit, never the branch's
        // own ancestry: a squash merge rewrites every SHA the branch carried.
        "pr-merged" if args.len() >= 5 => {
            let (repo, id, br, sha) = (a(1), a(2), a(3), a(4));
            let proof = if crate::git_evidence::content_on_base(std::path::Path::new(&repo), &br, &sha) { "merge-tree" } else { "gh-merged" };
            (id, "PR_OPEN", DeliveryEventKind::Delivered { merge_sha: sha, proof: proof.into() })
        }
        "pr-closed" if args.len() >= 2 => (a(1), "PR_OPEN", DeliveryEventKind::Returned { reason: ReturnedReason::PrClosedUnmerged }),
        // Push mode never rewrites the commit it landed: the proof is ancestry.
        "push-delivered" if args.len() >= 3 => {
            (a(1), "PUSHING", DeliveryEventKind::Delivered { merge_sha: a(2), proof: "ancestry".into() })
        }
        "push-requeued" if args.len() >= 3 => (a(1), "PUSHING", DeliveryEventKind::Requeued { tip: a(2) }),
        "push-returned" if args.len() >= 2 => (a(1), "PUSHING", DeliveryEventKind::Returned { reason: ReturnedReason::PushRejected }),
        _ => {
            return usage(
                "deliver pr-merged <repo> <bead-id> <branch> <merge-sha> | pr-closed <bead-id> [reason] | push-delivered <bead-id> <sha> | push-requeued <bead-id> <tip> | push-returned <bead-id> [reason]",
            )
        }
    };
    let actor = deliver_actor(sub);
    // _lc_deliver: no row, or a row not in the state this exit applies to, is logged and
    // skipped rather than forced.
    let rec = show(m, &id).ok().and_then(|v| {
        let d = v.get("delivery").filter(|d| !d.is_null())?.clone();
        Some((s(&d, "state"), match d.get("version") {
            Some(Value::Null) | None => "0".to_string(),
            Some(_) => s(&d, "version"),
        }))
    });
    let Some((state, version)) = rec else {
        // PUSH MODE HAS NO DELIVERY ROUND (sp-51lgh). A CERTIFIED bead landed by push has no
        // delivery row to carry the landing, and the sending sweep's ContentOnBase fires only
        // for a branch still ahead of the base — never for a fast-forward — so the row stayed
        // CERTIFIED with its commit on the base. Record the landing on the bead itself, with
        // the ancestry proof; every other no-row case is unchanged.
        if sub == "push-delivered" {
            let certified = show(m, &id).ok().is_some_and(|v| bead_field(&v, "state") == "CERTIFIED");
            if certified {
                let proof = format!("ancestry:{}", a(2));
                let ans = with_row(m, &id, actor, |_| Ok(BeadEventKind::ContentOnBase { proof: proof.clone() }));
                let line = if ans.code == 0 {
                    format!("lc: {id} landed by push — CERTIFIED -> LANDED (no delivery round; {actor}, {proof})")
                } else {
                    format!("lc: {id} landed by push, but the LANDED event was refused or unreachable ({actor}, exit {})", ans.code)
                };
                return Answer::out(ans.code, log_line(&line));
            }
        }
        // A pr-mode bead has no delivery round either. A merged PR lands it from any live state (a lost
        // write leaves READY) and only on content proof from the landing ref: a merge since
        // reverted proves nothing.
        if sub == "pr-merged" {
            let waiting = show(m, &id).ok().is_some_and(|v| !matches!(bead_field(&v, "state").as_str(), "" | "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE"));
            let repo_arg = a(1);
            let repo = std::path::Path::new(&repo_arg);
            // A deleted branch leaves no patch to test; the merge commit's place in the landing ref is the proof.
            let proof = if !waiting {
                None
            } else if crate::git_evidence::change_in_force(repo, &a(3), &a(4)) {
                Some(format!("merge-tree:{}", a(4)))
            } else if !crate::git_evidence::ref_resolves(repo, &a(3)) && crate::git_evidence::is_ancestor(repo, &a(4), "HEAD") {
                Some(format!("ancestry:{}", a(4)))
            } else {
                None
            };
            if let Some(proof) = proof {
                let ans = with_row(m, &id, actor, |_| Ok(BeadEventKind::ContentOnBase { proof: proof.clone() }));
                let line = if ans.code == 0 {
                    format!("lc: {id} landed by merged pull request — LANDED (no delivery round; {actor}, {proof})")
                } else {
                    format!("lc: {id} merged pull request, but the LANDED event was refused or unreachable ({actor}, exit {})", ans.code)
                };
                return Answer::out(ans.code, log_line(&line));
            }
        }
        return Answer::out(
            NO_ROW,
            log_line(&format!("lc: no delivery row for {id} — not recording {actor}'s event (inert until the delivery round lands)")),
        );
    };
    if state != want {
        return Answer::out(NO_ROW, log_line(&format!("lc: {id} delivery is {state}, not {want} — not recording {actor}'s event")));
    }
    let (rc, out) = event(m, "delivery", &id, want, &version, actor, &serde_json::to_string(&kind).unwrap_or_default());
    let mut stdout = String::new();
    if !out.is_empty() {
        stdout.push_str(&out);
        stdout.push('\n');
    }
    if rc == 0 {
        stdout.push_str(&log_line(&format!("lc: {id} delivery {want} -> applied ({actor})")));
    } else {
        stdout.push_str(&log_line(&format!("lc: {id} delivery event refused or unreachable ({actor}, exit {rc})")));
    }
    Answer::out(rc, stdout)
}

// ---- certification onto events (lifecycle-cert.sh) -----------------------------------

/// gate.sh's verdict reasons onto GateRedReason's closed set, once, so a reason spira-lc
/// cannot parse never turns an applicable event into a silent "cannot tell".
pub fn gate_red_reason(raw: &str) -> GateRedReason {
    match raw {
        "syntax" => GateRedReason::Syntax,
        "beads-data" | "foreign-harness" => GateRedReason::PolicyViolation,
        "no-rebase" => GateRedReason::NoRebase,
        "timeout" => GateRedReason::Timeout,
        "confine" => GateRedReason::Confine,
        _ => GateRedReason::SuitesFailed,
    }
}

fn state_version(m: &mut dyn Machine, id: &str) -> Option<(String, String, Value)> {
    let v = show(m, id).ok()?;
    let (st, ver) = (bead_field(&v, "state"), bead_field(&v, "version"));
    (!st.is_empty() && !ver.is_empty()).then_some((st, ver, v))
}

fn submit_kind(tip: &str) -> String {
    serde_json::to_string(&BeadEventKind::Submit { tip: tip.to_string() }).unwrap_or_default()
}

/// Move the row as close to SUBMITTED-at-<tip> as an event can: WORKING advances by Submit (a
/// REWORK row has no Submit, so a verdict for a bead that moved on is skipped, not refused),
/// and a CERTIFIED row at another tip is voided by Submit (the tip invariant, as bead.rs's
/// own Certified+Submit arm has it). None: the row is unreadable.
/// Only `queue submit` (which gated the branch tip itself) may move a SUBMITTED row's tip; a keyed
/// worker pass at another tip stays a refused stale verdict.
const BRANCH_TIP_DETAIL: &str = "queue-submit";

fn reach_submitted(m: &mut dyn Machine, id: &str, tip: &str, outcome: &str, detail: &str, actor: &str) -> Option<(String, String)> {
    let (mut st, mut ver, row) = state_version(m, id)?;
    let resubmit = match st.as_str() {
        "WORKING" => true,
        "CERTIFIED" => bead_field(&row, "tip") != tip,
        "SUBMITTED" => outcome == "pass" && detail == BRANCH_TIP_DETAIL && bead_field(&row, "tip") != tip,
        _ => false,
    };
    if resubmit {
        let _ = event(m, "bead", id, &st, &ver, actor, &submit_kind(tip));
        (st, ver, _) = state_version(m, id)?;
    }
    Some((st, ver))
}

fn certify(m: &mut dyn Machine, id: &str, tip: &str, outcome: &str, detail: &str, actor: &str) -> Answer {
    let Some((state, version)) = reach_submitted(m, id, tip, outcome, detail, actor) else {
        return Answer::cert(CANNOT_TELL, "cannot-tell", "no lifecycle row yet".into());
    };
    // A pass on a row already CERTIFIED at this tip (reach_submitted resubmits any other
    // tip) is the same verdict again: idempotent success, no event. Answering "skip" made a
    // fail-closed caller treat a certified bead as uncertified (sp-e9o2y's CHECK6).
    if state == "CERTIFIED" && outcome == "pass" {
        return Answer::cert(APPLIED, "already", format!("pass tip={tip} — already CERTIFIED"));
    }
    let in_round = state == "IN_DELIVERY" && outcome == "red";
    if state != "SUBMITTED" && !in_round {
        let mut a = Answer::cert(REFUSED, "skip", format!("state={state} tip={tip} outcome={outcome} — not SUBMITTED"));
        a.stderr = format!("spira-lc certify: {id} is {state}, not SUBMITTED; gated {tip} — nothing recorded\n");
        return a;
    }
    if outcome == "infra" && state == "SUBMITTED" {
        if let Some((_, _, row)) = state_version(m, id) {
            let row_tip = bead_field(&row, "tip");
            if row_tip != tip {
                return Answer::cert(REFUSED, "skip", format!("infra tip={tip} — stale, row is at {row_tip}"));
            }
        }
    }
    let kind = match outcome {
        "pass" => BeadEventKind::GatePass { tip: tip.into(), gate_key: detail.into() },
        "red" => BeadEventKind::GateRed { tip: tip.into(), reason: gate_red_reason(detail) },
        "infra" => BeadEventKind::GateInfra { tip: tip.into() },
        other => return Answer::cert(CANNOT_TELL, "cannot-tell", format!("unknown outcome {other}")),
    };
    if event(m, "bead", id, &state, &version, actor, &serde_json::to_string(&kind).unwrap_or_default()).0 == 0 {
        let marked = if in_round { " (in delivery — marked for eject)" } else { "" };
        return Answer::cert(APPLIED, "applied", format!("{outcome} tip={tip}{marked}"));
    }
    let held = state_version(m, id).map(|(_, _, row)| bead_field(&row, "tip")).unwrap_or_default();
    let mut a = Answer::cert(REFUSED, "refused", format!("{outcome} tip={tip}"));
    a.stderr = format!("spira-lc certify: {id} refused: tip mismatch: held {held}, gated {tip}\n");
    a
}

/// Record a moved tip with NO verdict — the tip invariant voiding a stale certification.
fn resubmit(m: &mut dyn Machine, id: &str, tip: &str, actor: &str) -> Answer {
    let v = match show(m, id) {
        Ok(v) => v,
        Err(_) => return Answer::cert(CANNOT_TELL, "cannot-tell", "spira-lc show failed".into()),
    };
    let (state, version) = (bead_field(&v, "state"), bead_field(&v, "version"));
    if state.is_empty() || version.is_empty() {
        return Answer::cert(CANNOT_TELL, "cannot-tell", "no lifecycle row yet".into());
    }
    if !matches!(state.as_str(), "WORKING" | "REWORK" | "CERTIFIED") {
        let mut a = Answer::cert(REFUSED, "skip", format!("resubmit: state={state} not eligible for tip={tip}"));
        a.stderr = format!("spira-lc resubmit: {id} is {state}; resubmit moves the tip of a WORKING or CERTIFIED bead — nothing changed\n");
        return a;
    }
    let (rc, out) = event(m, "bead", id, &state, &version, actor, &submit_kind(tip));
    if rc == 0 {
        return Answer::cert(APPLIED, "applied", format!("resubmit tip={tip}"));
    }
    let mut a = Answer::cert(REFUSED, "refused", format!("resubmit tip={tip}"));
    a.stderr = format!(
        "spira-lc resubmit: {id} is {state} and the machine has no Submit from it ({}); claim the bead, then send Submit with `spira-lc event` — nothing changed\n",
        out.trim()
    );
    a

}

/// Move a REWORK bead held by hand, or a READY bead nobody has claimed, to SUBMITTED at a new tip
/// with no gate verdict: the round it is admitted to certifies it. The machine refuses a REWORK
/// row with no manual hold.
fn retip(m: &mut dyn Machine, id: &str, tip: &str, actor: &str, glass: BreakGlass) -> Answer {
    let Some((state, version, _)) = state_version(m, id) else {
        return Answer::cert(CANNOT_TELL, "cannot-tell", "no lifecycle row yet".into());
    };
    if !matches!(state.as_str(), "REWORK" | "READY") {
        let mut a = Answer::cert(REFUSED, "skip", format!("retip: state={state} not eligible for tip={tip}"));
        a.stderr = format!("spira-lc retip: {id} is {state}, not REWORK or READY — nothing changed\n");
        return a;
    }
    if let Some(missing) = glass.gap(actor, tip) {
        let mut a = Answer::cert(REFUSED, "refused", format!("retip tip={tip}"));
        a.stderr = format!("spira-lc retip: {id} refused: break-glass needs {missing} — nothing changed\n");
        return a;
    }
    let kind = serde_json::to_string(&BeadEventKind::Resubmit { tip: tip.to_string(), break_glass: glass }).unwrap_or_default();
    let (rc, out) = event(m, "bead", id, &state, &version, actor, &kind);
    if rc == 0 {
        return Answer::cert(APPLIED, "applied", format!("retip tip={tip}"));
    }
    let mut a = Answer::cert(REFUSED, "refused", format!("retip tip={tip}"));
    a.stderr = format!("spira-lc retip: {id} refused: {} — nothing changed\n", out.trim());
    a
}

/// Withdraw a red recorded against `tip` that was not the tree's fault. The machine refuses a row
/// whose recorded red is not `tip`, and a move without its provenance.
fn absolve(m: &mut dyn Machine, id: &str, tip: &str, actor: &str, glass: BreakGlass) -> Answer {
    let Some((state, version, _)) = state_version(m, id) else {
        return Answer::cert(CANNOT_TELL, "cannot-tell", "no lifecycle row yet".into());
    };
    let kind = serde_json::to_string(&BeadEventKind::Absolve { tip: tip.to_string(), break_glass: glass }).unwrap_or_default();
    let (rc, out) = event(m, "bead", id, &state, &version, actor, &kind);
    if rc == 0 {
        return Answer::cert(APPLIED, "applied", format!("absolve tip={tip}"));
    }
    let mut a = Answer::cert(REFUSED, "refused", format!("absolve tip={tip}"));
    a.stderr = format!("spira-lc absolve: {id} refused: {} — nothing changed\n", out.trim());
    a
}

/// `renew <id> <holder> <lease-until>` — a working aeon extends its own lease (sp-2jf0a). The
/// Claim sets `lease_until` once; without this the stale-lease reaper clears every session
/// that outlives lease + reclaim_grace. Like `unclaim`, it acts only on a row WORKING under
/// `holder`, and sends the event AS the holder, so the machine's own holder check (a
/// `NotHolder` refusal) backs this read: a reaped aeon's late renewal can never extend the
/// lease of the aeon the bead was handed to. A holder that stops renewing still expires.
///
/// Exit: 0 renewed · 1 not this holder's WORKING row (held by another, past WORKING, no row)
/// · 2 cannot tell, or usage · 3 refused (a race, or a deadline that does not advance).
fn renew(m: &mut dyn Machine, args: &[String]) -> Answer {
    let (Some(id), Some(holder), Some(until)) = (
        args.first().filter(|s| !s.is_empty()),
        args.get(1).filter(|s| !s.is_empty()),
        args.get(2).and_then(|s| s.parse::<i64>().ok()),
    ) else {
        return usage("renew <bead-id> <holder> <lease-until-epoch>");
    };
    let not_held = |why: String| Answer { code: NO_ROW, stderr: format!("spira-lc renew: {id}: {why}\n"), ..Default::default() };
    let v = match show(m, id) {
        Ok(v) => v,
        Err(NO_ROW) => return not_held("no lifecycle row".into()),
        Err(rc) => return Answer::code(rc),
    };
    let (state, version, cur) = (bead_field(&v, "state"), bead_field(&v, "version"), bead_field(&v, "holder"));
    if state != "WORKING" {
        return not_held(format!("{state}, not WORKING — no lease to renew"));
    }
    if cur != *holder {
        return not_held(format!("held by {cur:?}, not {holder}"));
    }
    let kind = serde_json::to_string(&BeadEventKind::Renew { lease_until: until }).unwrap_or_default();
    match event(m, "bead", id, &state, &version, holder, &kind) {
        (APPLIED, _) => Answer::code(APPLIED),
        (rc, out) => Answer { code: rc, stderr: format!("spira-lc renew: {id}: {}\n", out.trim()), ..Default::default() },
    }
}

/// `phase <bead-id> <holder> <claimed|building|session|teardown>` — the
/// holder's move to its next phase. Exit: 0 applied · 1 no row · 2 cannot tell · 3 refused
/// (not WORKING, not the holder, or not strictly forward — the refusal names the state).
fn phase(m: &mut dyn Machine, args: &[String]) -> Answer {
    let (Some(id), Some(holder), Some(name)) = (args.first(), args.get(1), args.get(2)) else {
        return usage("phase <bead-id> <holder> <claimed|building|session|teardown>");
    };
    let Some(phase) = AeonPhase::from_str(name) else {
        return usage(&format!("phase: unknown phase {name:?}; one of {}", AeonPhase::ALL.map(AeonPhase::as_str).join(" ")));
    };
    with_row(m, id, holder, |_| Ok(BeadEventKind::Phase { phase }))
}

/// `disposition <bead-id> <lapsed|thrash|slain> [note] [actor]` — record how the session was
/// cut short on the WORKING row; the teardown reads it instead of a marker file.
fn disposition(m: &mut dyn Machine, args: &[String]) -> Answer {
    let (Some(id), Some(name)) = (args.first(), args.get(1)) else {
        return usage("disposition <bead-id> <lapsed|thrash|slain> [note] [actor]");
    };
    let Some(status) = DispositionStatus::from_str(name) else {
        return usage(&format!("disposition: unknown status {name:?}; one of {}", DispositionStatus::ALL.map(DispositionStatus::as_str).join(" ")));
    };
    let note = args.get(2).cloned().unwrap_or_default();
    with_row(m, id, &actor_or(args.get(3), "aeon"), |_| Ok(BeadEventKind::Disposition { status, note: note.clone() }))
}

/// The ReturnedReason a reopen's free-text cause earns: an eject is the batch's own, anything
/// else a CERTIFIED or in-delivery bead is handed back for is the base having moved.
fn reopen_returned_reason(cause: &str) -> ReturnedReason {
    match cause {
        "eject" | "batch-eject" => ReturnedReason::BatchEjected,
        "eject-red" => ReturnedReason::BatchEjectedRed,
        "base-withdrawn" => ReturnedReason::BaseWithdrawn,
        "pr-checks-red" => ReturnedReason::PrChangesRequested,
        _ => ReturnedReason::PushRejected,
    }
}

fn round_owned_cause(cause: &str) -> bool {
    reopen_returned_reason(cause) != ReturnedReason::PushRejected
}

/// `reopen <id> [cause] [actor]` — hand a bead back to its builder: the machine's
/// return-to-rework, recording the event the row's own state implies. SUBMITTED GateRed ·
/// CERTIFIED Deliver then Returned · IN_DELIVERY Returned · WORKING Release · READY or REWORK
/// nothing (already there) · terminal refused. A hold stays on the row: reopening is not an
/// unhold.
///
/// Exit: 0 reopened, or already open · 1 no row · 2 cannot tell · 3 refused (terminal, or the
/// event lost its CAS).
fn reopen(m: &mut dyn Machine, id: &str, cause: &str, actor: &str) -> Answer {
    let refuse = |why: String| Answer { code: REFUSED, stderr: format!("spira-lc reopen: {id}: {why}\n"), ..Default::default() };
    let v = match show(m, id) {
        Ok(v) => v,
        Err(rc) => return Answer::code(rc),
    };
    let (state, version) = (bead_field(&v, "state"), bead_field(&v, "version"));
    if state.is_empty() || version.is_empty() {
        return Answer::code(NO_ROW);
    }
    let send = |m: &mut dyn Machine, expect: &str, version: &str, kind: BeadEventKind| {
        event(m, "bead", id, expect, version, actor, &serde_json::to_string(&kind).unwrap_or_default())
    };
    let returned = BeadEventKind::Returned { reason: reopen_returned_reason(cause) };
    let rc = match state.as_str() {
        "READY" | "REWORK" => return Answer::code(APPLIED),
        "WORKING" => send(m, &state, &version, BeadEventKind::Release).0,
        "SUBMITTED" => {
            let kind = BeadEventKind::GateRed { tip: bead_field(&v, "tip"), reason: gate_red_reason(cause) };
            send(m, &state, &version, kind).0
        }
        "IN_DELIVERY" => {
            let batched = v.get("delivery").is_some_and(|d| s(d, "state") == "BATCHED");
            if batched && !round_owned_cause(cause) {
                return refuse("a live round holds it (BATCHED); the round's verdict decides — only an eject returns it".into());
            }
            send(m, &state, &version, returned).0
        }
        "CERTIFIED" => match send(m, &state, &version, BeadEventKind::Deliver).0 {
            APPLIED => match state_version(m, id) {
                Some((st, ver, _)) if st == "IN_DELIVERY" => send(m, &st, &ver, returned).0,
                _ => CANNOT_TELL,
            },
            rc => rc,
        },
        other => return refuse(format!("{other} is terminal — a finished bead is not reopened")),
    };
    match rc {
        APPLIED => Answer::code(APPLIED),
        REFUSED => refuse("the row moved under the reopen (lost the compare-and-swap)".into()),
        rc => Answer::code(rc),
    }
}

/// The bd half a verb needs: an epic's own close. Behind a trait so the compositions below
/// are unit-testable without bd.
pub trait Bd {
    /// The bead's bd `issue_type` (`task`, `epic`, ...).
    fn issue_type(&mut self, id: &str) -> Result<String, String>;
    /// `bd close <id> --reason <reason>`.
    fn close(&mut self, id: &str, reason: &str) -> Result<(), String>;
    /// `(id, close_reason)` for each of `ids` whose bd status is closed.
    fn closed(&mut self, ids: &[String]) -> Result<Vec<(String, String)>, String>;
    /// The subset of `ids` that bd has a bead for, in any status.
    fn known(&mut self, ids: &[String]) -> Result<Vec<String>, String>;
    /// The subset of `ids` whose bd status is not closed.
    fn open_among(&mut self, ids: &[String]) -> Result<Vec<String>, String>;
    /// The store half of a reopen: bd status open, no assignee, no submitted label — so a bead
    /// the machine handed back reads open wherever bd is still read (sp-swh8b8). Idempotent.
    fn reopen(&mut self, id: &str) -> Result<(), String>;
    /// The work beads an ask names on its `work-bead:<id>` labels; empty for any other bead.
    fn work_beads(&mut self, id: &str) -> Result<Vec<String>, String>;
    /// Whether the bead came from outside: its bd `external_ref` is set.
    fn external(&mut self, id: &str) -> Result<bool, String>;
    /// The open beads that `blocks`-depend on `id`.
    fn blocking_dependents(&mut self, id: &str) -> Result<Vec<String>, String>;
    /// The beads whose `parent-child` edge names `id` as their parent.
    fn children(&mut self, id: &str) -> Result<Vec<String>, String>;
}

/// An epic's children by lifecycle state; `pending` names those not yet terminal. A child with
/// no lifecycle row counts as `CLOSED` when bd holds it closed.
struct Tally {
    counts: BTreeMap<String, usize>,
    pending: Vec<String>,
}

impl Tally {
    fn summary(&self) -> String {
        self.counts.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", ")
    }
}

fn tally_children(m: &mut dyn Machine, bd: &mut dyn Bd, id: &str) -> Result<Tally, String> {
    let kids = bd.children(id)?;
    let mut t = Tally { counts: BTreeMap::new(), pending: Vec::new() };
    if kids.is_empty() {
        return Ok(t);
    }
    let (rc, out) = m.call(&["list".into(), "--ids".into(), kids.join(",")]);
    if rc != 0 {
        return Err(format!("lifecycle list failed: {}", out.trim()));
    }
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(out.trim()) else {
        return Err("lifecycle list is not a JSON array".into());
    };
    let states: BTreeMap<String, String> = rows.iter().map(|r| (s(r, "bead_id"), s(r, "state"))).collect();
    let mut rowless = Vec::new();
    for k in &kids {
        match states.get(k) {
            Some(st) if BeadState::from_str(st).is_some_and(|b| b.is_terminal()) => *t.counts.entry(st.clone()).or_default() += 1,
            Some(_) => t.pending.push(k.clone()),
            None => rowless.push(k.clone()),
        }
    }
    if !rowless.is_empty() {
        let closed: Vec<String> = bd.closed(&rowless)?.into_iter().map(|(i, _)| i).collect();
        for k in rowless {
            if closed.contains(&k) {
                *t.counts.entry("CLOSED".into()).or_default() += 1;
            } else {
                t.pending.push(k);
            }
        }
    }
    Ok(t)
}

/// Why `kind` may not end the external bead `id`, as an answer; `None` when it may go ahead or
/// the bead is not external. A bd that cannot say refuses too — an unreadable origin is not
/// permission to drop.
fn external_refusal(bd: &mut dyn Bd, id: &str, kind: &BeadEventKind, reason: &str, verb: &str) -> Option<Answer> {
    match bd.external(id) {
        Ok(false) => None,
        Ok(true) => lifecycle::provenance::refuse_external(kind, reason)
            .map(|why| Answer { code: REFUSED, stderr: format!("spira-lc {verb}: {id}: {why} — nothing closed\n"), ..Default::default() }),
        Err(e) => Some(Answer { code: CANNOT_TELL, stderr: format!("spira-lc {verb}: {id}: its origin is unreadable — nothing closed: {}\n", e.trim()), ..Default::default() }),
    }
}

/// `drop <bead-id> <reason> [actor]` — the operator's drop, which for an externally-originated
/// bead must carry a named outcome in `<reason>` (see `provenance::parse_close_reason`).
pub fn drop_cmd(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let kind = BeadEventKind::Drop { reason: DropReason::Unwanted };
    if let (Some(id), Some(reason)) = (args.first().filter(|s| !s.is_empty()), args.get(1).filter(|s| !s.is_empty())) {
        if let Some(a) = external_refusal(bd, id, &kind, reason, "drop") {
            return a;
        }
    }
    run("drop", args, m)
}

/// `drop-bead <bead-id> --reason <text> [--actor <name>]` — end a work bead as won't-do. Refuses
/// while an open bead still depends on it, takes it out of every unfinished batch, then closes it
/// through [`close`] as DROPPED with the reason. Terminal and rowless beads are refused.
pub fn drop_bead(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    const USE: &str = "drop-bead <bead-id> --reason <text> [--actor <name>]";
    let (mut id, mut reason, mut actor) = (None, None, "drop".to_string());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reason" => reason = args.get(i + 1).cloned(),
            "--actor" => actor = args.get(i + 1).cloned().unwrap_or_default(),
            a if !a.starts_with('-') && id.is_none() => {
                id = Some(a.to_string());
                i += 1;
                continue;
            }
            _ => return usage(USE),
        }
        i += 2;
    }
    let (Some(id), Some(reason)) = (id.filter(|s| !s.is_empty()), reason.map(|r| r.trim().to_string()).filter(|r| !r.is_empty())) else {
        return usage(USE);
    };
    if actor.is_empty() {
        return usage(USE);
    }
    let refused = |why: String| Answer { code: REFUSED, stderr: format!("spira-lc drop-bead: {id}: {why} — nothing dropped\n"), ..Default::default() };
    match show(m, &id) {
        Err(NO_ROW) => return refused("has no lifecycle row, so it is not a work bead".into()),
        Err(rc) => return Answer { code: rc, stderr: format!("spira-lc drop-bead: {id}: the lifecycle row is unreadable — nothing dropped\n"), ..Default::default() },
        Ok(v) => {
            let state = bead_field(&v, "state");
            if matches!(state.as_str(), "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE") {
                return refused(format!("is already {state}"));
            }
        }
    }
    match bd.blocking_dependents(&id) {
        Ok(d) if !d.is_empty() => return refused(format!("{} would be released without a decision: {}", if d.len() == 1 { "an open dependent" } else { "open dependents" }, d.join(", "))),
        Ok(_) => {}
        Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc drop-bead: {id}: its dependents are unreadable — nothing dropped: {}\n", e.trim()), ..Default::default() },
    }
    let (rc, out) = m.call(&["batches-of".into(), id.clone()]);
    if rc != 0 {
        return Answer { code: CANNOT_TELL, stderr: format!("spira-lc drop-bead: {id}: its batches are unreadable — nothing dropped\n"), ..Default::default() };
    }
    let batches: Vec<Value> = serde_json::from_str(out.trim()).unwrap_or_default();
    for b in &batches {
        let (batch, state, version) = (s(b, "batch_id"), s(b, "state"), s(b, "version"));
        let (rc, out) = m.call(&[
            "eject-member".into(),
            batch.clone(),
            "--bead-id".into(),
            id.clone(),
            "--expect".into(),
            state.clone(),
            "--version".into(),
            version,
            "--actor".into(),
            actor.clone(),
            "--reason".into(),
            format!("dropped: {reason}"),
        ]);
        if rc != 0 {
            return Answer { code: if rc == REFUSED { REFUSED } else { CANNOT_TELL }, stderr: format!("spira-lc drop-bead: {id}: could not leave batch {batch} ({state}): {} — nothing dropped\n", out.trim()), ..Default::default() };
        }
    }
    let close_args = vec![id.clone(), "--reason".into(), format!("dropped: {reason}"), "--dropped".into(), "--actor".into(), actor];
    close(&close_args, m, bd, &mut |_| Err("no reason file".into()))
}

/// `reopen <id> [cause] [actor]` — the one door for handing a bead back (sp-swh8b8), the mirror of
/// [`close`]: the row records the transition its state implies FIRST (see the caller verb's
/// table), and only then is the store reopened, so bd never says open while the row says done.
/// A bead with no row (an alert, an ask — rowless, bd is its only state) is reopened in the
/// store alone. Terminal, refused or unreadable: nothing is written to the store.
pub fn reopen_cmd(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let Some(id) = args.first().filter(|s| !s.is_empty()) else {
        return usage("reopen <bead-id> [cause] [actor]");
    };
    let cause = args.get(1).cloned().unwrap_or_default();
    let actor = actor_or(args.get(2), "reopen");
    match show(m, id) {
        Err(NO_ROW) => {}
        Err(rc) => return Answer { code: rc, stderr: format!("spira-lc reopen: {id}: the lifecycle row is unreadable — nothing reopened\n"), ..Default::default() },
        Ok(_) => {
            let a = reopen(m, id, &cause, &actor);
            if a.code != APPLIED {
                return a;
            }
        }
    }
    match bd.reopen(id) {
        Ok(()) => Answer::code(APPLIED),
        Err(e) => Answer {
            code: CANNOT_TELL,
            stderr: format!("spira-lc reopen: {id}: the row is handed back but the store reopen failed: {}\n", e.trim()),
            ..Default::default()
        },
    }
}

/// The terminal event a bead closed in bd with `reason` earns on its lifecycle row:
/// SUPERSEDED when the reason names a successor, DROPPED otherwise. Never LANDED — only
/// the landing path may claim content reached the base.
pub fn terminal_event_for(reason: &str) -> BeadEventKind {
    let lower = reason.to_lowercase();
    if let Some(at) = lower.find("superseded by ") {
        let rest = &reason[at + "superseded by ".len()..];
        let by = rest.split_whitespace().next().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric());
        if by.contains('-') {
            return BeadEventKind::Supersede { by: by.to_string() };
        }
    }
    BeadEventKind::Drop { reason: DropReason::ClosedNoBranch }
}

const RECONCILE_CHUNK: usize = 100;

/// `reconcile-closed [--apply] [<id>...]` — move the READY lifecycle row of every bead bd
/// has closed to the terminal state its close reason earns. Dry run unless `--apply`. With
/// ids, only those; the closers' own hook passes the id it just closed. Only READY rows are
/// touched: a bead that was claimed or submitted ends through the delivery path, which
/// records its own outcome, and is reported, not rewritten.
///
/// Exit: 0 done (or listed) · 2 cannot tell · 3 an event was refused (an ask hold, a lost race).
pub fn reconcile_closed(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let apply = args.iter().any(|a| a == "--apply");
    if let Some(bad) = args.iter().find(|a| a.starts_with('-') && *a != "--apply") {
        return usage(&format!("reconcile-closed [--apply] [<bead-id>...] (unknown flag {bad})"));
    }
    let only: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let (rc, out) = m.call(&["list".into()]);
    if rc != 0 {
        return Answer { code: rc.max(CANNOT_TELL), stderr: format!("spira-lc reconcile-closed: lifecycle list failed: {}\n", out.trim()), ..Default::default() };
    }
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(out.trim()) else {
        return Answer { code: CANNOT_TELL, stderr: "spira-lc reconcile-closed: lifecycle list is not a JSON array\n".into(), ..Default::default() };
    };
    let ready: Vec<&Value> = rows.iter().filter(|r| s(r, "state") == "READY" && (only.is_empty() || only.iter().any(|i| **i == s(r, "bead_id")))).collect();
    let ids: Vec<String> = ready.iter().map(|r| s(r, "bead_id")).collect();
    let mut closed = BTreeMap::new();
    for chunk in ids.chunks(RECONCILE_CHUNK) {
        match bd.closed(chunk) {
            Ok(v) => closed.extend(v),
            Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc reconcile-closed: bd unreadable: {}\n", e.trim()), ..Default::default() },
        }
    }
    let (mut lines, mut code, mut done) = (Vec::new(), APPLIED, 0usize);
    for r in ready {
        let id = s(r, "bead_id");
        let Some(reason) = closed.get(&id) else { continue };
        let kind = terminal_event_for(reason);
        if apply {
            if let Some(a) = external_refusal(bd, &id, &kind, reason, "reconcile-closed") {
                lines.push(format!("refused {id}: {}", a.stderr.trim()));
                code = REFUSED;
                continue;
            }
        }
        let to = if matches!(kind, BeadEventKind::Supersede { .. }) { "SUPERSEDED" } else { "DROPPED" };
        let why = reason.lines().next().unwrap_or("").trim();
        if !apply {
            lines.push(format!("would move {id} READY -> {to} ({why})"));
            done += 1;
            continue;
        }
        let (version, kind_json) = (s(r, "version"), serde_json::to_string(&kind).unwrap_or_default());
        match event(m, "bead", &id, "READY", &version, "reconcile-closed", &kind_json).0 {
            APPLIED => {
                lines.push(format!("moved {id} READY -> {to} ({why})"));
                done += 1;
            }
            rc => {
                lines.push(format!("refused {id}: event exit {rc}"));
                code = REFUSED;
            }
        }
    }
    lines.push(format!("reconcile-closed: {done} {}", if apply { "moved" } else { "would move (dry run; --apply to write)" }));
    Answer::out(code, lines.join("\n"))
}

/// `reconcile-epics [--apply] [<id>...]` — move every non-terminal epic's row off READY to
/// OPEN (the epic rule), and every OPEN epic whose children are all terminal to DONE
/// (`epic-complete`, the counts by state in the reason). Dry run unless `--apply`. Only
/// READY/OPEN rows are asked of bd: one `issue_type` read per row, never a full roster.
///
/// Exit: 0 done (or listed) · 2 cannot tell · 3 an event was refused.
pub fn reconcile_epics(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let apply = args.iter().any(|a| a == "--apply");
    if let Some(bad) = args.iter().find(|a| a.starts_with('-') && *a != "--apply") {
        return usage(&format!("reconcile-epics [--apply] [<bead-id>...] (unknown flag {bad})"));
    }
    let only: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let (rc, out) = m.call(&["list".into(), "--state".into(), "READY,OPEN".into()]);
    if rc != 0 {
        return Answer { code: rc.max(CANNOT_TELL), stderr: format!("spira-lc reconcile-epics: lifecycle list failed: {}\n", out.trim()), ..Default::default() };
    }
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(out.trim()) else {
        return Answer { code: CANNOT_TELL, stderr: "spira-lc reconcile-epics: lifecycle list is not a JSON array\n".into(), ..Default::default() };
    };
    let (mut lines, mut code, mut done) = (Vec::new(), APPLIED, 0usize);
    for r in rows.iter().filter(|r| matches!(s(r, "state").as_str(), "READY" | "OPEN") && (only.is_empty() || only.iter().any(|i| **i == s(r, "bead_id")))) {
        let (id, from) = (s(r, "bead_id"), s(r, "state"));
        match bd.issue_type(&id) {
            Ok(t) if t == "epic" => {}
            Ok(_) => continue,
            Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc reconcile-epics: bd unreadable for {id}: {}\n", e.trim()), ..Default::default() },
        }
        let (to, rule) = if from == "READY" {
            ("OPEN", "epic-container".to_string())
        } else {
            let t = match tally_children(m, bd, &id) {
                Ok(t) => t,
                Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc reconcile-epics: children of {id} unreadable: {}\n", e.trim()), ..Default::default() },
            };
            if t.counts.is_empty() || !t.pending.is_empty() {
                continue;
            }
            ("DONE", format!("epic-complete: {}", t.summary()))
        };
        if !apply {
            lines.push(format!("would move {id} {from} -> {to} ({rule})"));
            done += 1;
            continue;
        }
        let state = if to == "DONE" { BeadState::Done } else { BeadState::Open };
        let kind = BeadEventKind::Reclassify { state, rule: rule.clone() };
        let (version, kind_json) = (s(r, "version"), serde_json::to_string(&kind).unwrap_or_default());
        match event(m, "bead", &id, &from, &version, "classifier", &kind_json).0 {
            APPLIED => {
                lines.push(format!("moved {id} {from} -> {to} ({rule})"));
                done += 1;
            }
            rc => {
                lines.push(format!("refused {id}: event exit {rc}"));
                code = REFUSED;
            }
        }
    }
    lines.push(format!("reconcile-epics: {done} {}", if apply { "moved" } else { "would move (dry run; --apply to write)" }));
    Answer::out(code, lines.join("\n"))
}

/// `reconcile-content [--apply] [<id>...]` — close in the store every bead whose lifecycle row
/// is terminal (LANDED, DONE, DROPPED, SUPERSEDED) and which bd still holds open, with a close
/// reason citing the transition. The row is never written: it already says the bead is over,
/// and the store is made to agree. Dry run unless `--apply`. Without ids, a bd that reports
/// none of the terminal rows open is the normal steady state; a bd that cannot answer is not.
///
/// `--since-seq N` reads only the beads with a bead-machine event after seq N, plus any ids
/// named, and prints `through-seq: M` (the newest seq) before the summary: a pass over an
/// unchanged store reads no content rows at all.
///
/// Exit: 0 done (or listed) · 2 cannot tell · 3 a store close failed.
pub fn reconcile_content(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let apply = args.iter().any(|a| a == "--apply");
    let since = args.iter().position(|a| a == "--since-seq").map(|i| args.get(i + 1).cloned().unwrap_or_default());
    let mut only: Vec<&String> = Vec::new();
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--apply" => {}
            "--since-seq" => {
                rest.next();
            }
            f if f.starts_with('-') => return usage(&format!("reconcile-content [--apply] [--since-seq N] [<bead-id>...] (unknown flag {f})")),
            _ => only.push(a),
        }
    }
    let cannot = |why: String| Answer { code: CANNOT_TELL, stderr: format!("spira-lc reconcile-content: {why}\n"), ..Default::default() };
    let (rc, out) = m.call(&["list".into()]);
    if rc != 0 {
        return cannot(format!("lifecycle list failed: {}", out.trim()));
    }
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(out.trim()) else {
        return cannot("lifecycle list is not a JSON array".into());
    };
    let mut through = None;
    let mut scope: Option<BTreeSet<String>> = None;
    if let Some(since) = &since {
        if since.parse::<u64>().is_err() {
            return usage("reconcile-content [--apply] [--since-seq N] [<bead-id>...] (--since-seq wants a number)");
        }
        let (rc, out) = m.call(&["event-seq".into(), "--since".into(), since.clone()]);
        let tail = if rc == 0 { serde_json::from_str::<Value>(out.trim()).ok() } else { None };
        let Some(tail) = tail else {
            return cannot(format!("event-seq unreadable: {}", out.trim()));
        };
        through = tail["seq"].as_u64();
        let mut keys: BTreeSet<String> = tail["keys"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
        keys.extend(only.iter().map(|i| i.to_string()));
        scope = Some(keys);
    }
    let reasons: BTreeMap<String, String> = rows.iter().map(|r| (s(r, "bead_id"), s(r, "reason"))).collect();
    let terminal: BTreeMap<String, String> = rows
        .iter()
        .filter(|r| {
            let id = s(r, "bead_id");
            matches!(s(r, "state").as_str(), "LANDED" | "DONE" | "DROPPED" | "SUPERSEDED")
                && match &scope {
                    Some(keys) => keys.contains(&id),
                    None => only.is_empty() || only.iter().any(|i| **i == id),
                }
        })
        .map(|r| (s(r, "bead_id"), s(r, "state")))
        .collect();
    let ids: Vec<String> = terminal.keys().cloned().collect();
    let mut open = Vec::new();
    for chunk in ids.chunks(RECONCILE_CHUNK) {
        let (rc, out) = m.call(&["content-rows".into(), "--open".into(), "--ids".into(), chunk.join(",")]);
        if rc != 0 {
            return cannot(format!("content store unreadable: {}", out.trim()));
        }
        let Ok(Value::Array(found)) = serde_json::from_str::<Value>(out.trim()) else {
            return cannot("content-rows is not a JSON array".into());
        };
        open.extend(found.iter().map(|r| s(r, "id")).filter(|i| !i.is_empty()));
    }
    let (mut lines, mut code, mut done) = (Vec::new(), APPLIED, 0usize);
    for id in open {
        let state = &terminal[&id];
        if !apply {
            lines.push(format!("would close {id} (lifecycle {state})"));
            done += 1;
            continue;
        }
        let why = reasons.get(&id).map(|r| r.trim()).filter(|r| !r.is_empty()).map(|r| format!(" Row reason: {r}\n")).unwrap_or_default();
        let outcome = if state == "DONE" { "done-without-landing (no commit landed; the row says delivered by hand)" } else { &state.to_lowercase() };
        let reason = format!("OUTCOME: {outcome}\nClosed by reconcile-content: the lifecycle row is {state}, so the content bead ends with it.\n{why}");
        match bd.close(&id, &reason) {
            Ok(()) => {
                lines.push(format!("closed {id} (lifecycle {state})"));
                done += 1;
            }
            Err(e) => {
                lines.push(format!("failed {id}: {}", e.trim()));
                code = REFUSED;
            }
        }
    }
    if let Some(seq) = through {
        lines.push(format!("through-seq: {seq}"));
    }
    lines.push(format!("reconcile-content: {done} {}", if apply { "closed" } else { "would close (dry run; --apply to write)" }));
    Answer::out(code, lines.join("\n"))
}

/// `drop-orphans [--apply] [<id>...]` — move the READY lifecycle row of every key bd has no
/// bead for to DROPPED. Dry run unless `--apply`. A row is never deleted (spira_lc holds no
/// DELETE); DROPPED takes it off READY and keeps its history. Without ids, a bd that knows
/// none of the READY rows is read as bd misreading, not as every row being an orphan.
///
/// Exit: 0 done (or listed) · 2 cannot tell · 3 an event was refused.
pub fn drop_orphans(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let apply = args.iter().any(|a| a == "--apply");
    if let Some(bad) = args.iter().find(|a| a.starts_with('-') && *a != "--apply") {
        return usage(&format!("drop-orphans [--apply] [<bead-id>...] (unknown flag {bad})"));
    }
    let only: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let cannot = |why: String| Answer { code: CANNOT_TELL, stderr: format!("spira-lc drop-orphans: {why}\n"), ..Default::default() };
    let (rc, out) = m.call(&["list".into()]);
    if rc != 0 {
        return cannot(format!("lifecycle list failed: {}", out.trim()));
    }
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(out.trim()) else {
        return cannot("lifecycle list is not a JSON array".into());
    };
    let ready: Vec<&Value> = rows.iter().filter(|r| s(r, "state") == "READY" && (only.is_empty() || only.iter().any(|i| **i == s(r, "bead_id")))).collect();
    let ids: Vec<String> = ready.iter().map(|r| s(r, "bead_id")).collect();
    let mut known = std::collections::BTreeSet::new();
    for chunk in ids.chunks(RECONCILE_CHUNK) {
        match bd.known(chunk) {
            Ok(v) => known.extend(v),
            Err(e) => return cannot(format!("bd unreadable: {}", e.trim())),
        }
    }
    if only.is_empty() && !ids.is_empty() && known.is_empty() {
        return cannot("bd knows none of the READY rows; refusing to drop them all".into());
    }
    let (mut lines, mut code, mut done) = (Vec::new(), APPLIED, 0usize);
    for r in ready {
        let id = s(r, "bead_id");
        if known.contains(&id) {
            continue;
        }
        if !apply {
            lines.push(format!("would move {id} READY -> DROPPED (no bead in the store)"));
            done += 1;
            continue;
        }
        let (version, kind_json) = (s(r, "version"), serde_json::to_string(&BeadEventKind::Drop { reason: DropReason::ClosedNoBranch }).unwrap_or_default());
        match event(m, "bead", &id, "READY", &version, "drop-orphans", &kind_json).0 {
            APPLIED => {
                lines.push(format!("moved {id} READY -> DROPPED (no bead in the store)"));
                done += 1;
            }
            rc => {
                lines.push(format!("refused {id}: event exit {rc}"));
                code = REFUSED;
            }
        }
    }
    lines.push(format!("drop-orphans: {done} {}", if apply { "moved" } else { "would move (dry run; --apply to write)" }));
    Answer::out(code, lines.join("\n"))
}

/// `unclaim <id> <actor>` — an aeon hands back a bead it still holds (lib.sh
/// `release_own_claim`).
///
/// The lifecycle row IS the claim and the only record touched: a row WORKING under `actor`
/// gets `Release`; a row WORKING under anyone else is refused, because a sweep that reaped
/// this aeon may already have handed the bead on; a row past WORKING means the claim is
/// already over. bd is not written — nothing reads its assignee.
///
/// Exit: 0 released, or nothing held · 1 not this actor's claim, or no such bead · 2 cannot
/// tell, or usage.
pub fn unclaim(args: &[String], m: &mut dyn Machine) -> Answer {
    let (Some(id), Some(actor)) = (args.first().filter(|s| !s.is_empty()), args.get(1).filter(|s| !s.is_empty())) else {
        return usage("unclaim <bead-id> <actor>");
    };
    let refuse = |why: String| Answer { code: NO_ROW, stderr: format!("spira-lc unclaim: {id}: {why}\n"), ..Default::default() };
    let v = match show(m, id) {
        Ok(v) => v,
        Err(NO_ROW) => return refuse("no lifecycle row".into()),
        Err(rc) => return Answer::code(rc),
    };
    let (state, version, holder) = (bead_field(&v, "state"), bead_field(&v, "version"), bead_field(&v, "holder"));
    if state != "WORKING" {
        return Answer::code(APPLIED);
    }
    if holder != *actor {
        return refuse(format!("held by {holder:?}, not {actor}"));
    }
    match event(m, "bead", id, &state, &version, actor, &serde_json::to_string(&BeadEventKind::Release).unwrap_or_default()).0 {
        APPLIED => Answer::code(APPLIED),
        REFUSED => refuse("the release lost a race with another writer".into()),
        rc => Answer::code(rc),
    }
}

/// `close-epic <id> <reason>` — close an EPIC bead: the lifecycle row first (DONE, rule
/// `epic-complete`, through the machine), then the store. Refuses anything that is not an
/// epic, and an epic with a child that is not terminal — a work bead's end is the machine's
/// `done`/delivery, never a bd close. An epic with no row is closed in the store alone.
///
/// Exit: 0 closed · 2 cannot tell (bd or the machine unreadable, or the close failed) · 3 refused
/// (not an epic, a child still open, or the event lost its CAS).
pub fn close_epic(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let (Some(id), Some(reason)) = (args.first().filter(|s| !s.is_empty()), args.get(1)) else {
        return usage("close-epic <bead-id> <reason>");
    };
    let cannot = |why: String| Answer { code: CANNOT_TELL, stderr: format!("spira-lc close-epic: {why}\n"), ..Default::default() };
    match bd.issue_type(id) {
        Err(e) => return cannot(format!("cannot read {id}: {}", e.trim())),
        Ok(t) if t != "epic" => {
            return Answer {
                code: REFUSED,
                stderr: format!("spira-lc close-epic: {id} is a {t}, not an epic — a work bead ends through the lifecycle machine, never a bd close\n"),
                ..Default::default()
            }
        }
        Ok(_) => {}
    }
    let t = match tally_children(m, bd, id) {
        Ok(t) => t,
        Err(e) => return cannot(format!("children of {id} unreadable: {}", e.trim())),
    };
    if !t.pending.is_empty() {
        return Answer {
            code: REFUSED,
            stderr: format!("spira-lc close-epic: {id} has children not yet terminal: {}\n", t.pending.join(", ")),
            ..Default::default()
        };
    }
    match show(m, id) {
        Err(NO_ROW) => {}
        Err(rc) => return Answer::code(rc),
        Ok(v) => {
            let (state, version) = (bead_field(&v, "state"), bead_field(&v, "version"));
            if !BeadState::from_str(&state).is_some_and(|b| b.is_terminal()) {
                let why = if t.counts.is_empty() { "epic-complete".to_string() } else { format!("epic-complete: {}", t.summary()) };
                let kind = BeadEventKind::Reclassify { state: BeadState::Done, rule: why };
                match event(m, "bead", id, &state, &version, "classifier", &serde_json::to_string(&kind).unwrap_or_default()).0 {
                    APPLIED => {}
                    REFUSED => return Answer { code: REFUSED, stderr: format!("spira-lc close-epic: {id}: the row move lost a race or was refused — nothing closed\n"), ..Default::default() },
                    rc => return Answer::code(rc),
                }
            }
        }
    }
    match bd.close(id, reason) {
        Ok(()) => Answer::code(APPLIED),
        Err(e) => cannot(format!("bd close {id} failed: {}", e.trim())),
    }
}

/// `close <id> (--reason R | --reason-file F|-) [--superseded-by X | --landing | --delivered] [--actor A]` — the one
/// door for closing a bead (sp-3fue0j). The lifecycle machine records the end FIRST — an open
/// ask is withdrawn, then SUPERSEDED (a successor named by `--superseded-by` or in the reason)
/// or DROPPED — and only then is the store closed, so a closed bead can never sit READY on
/// its row again (237 did on 2026-10-06, closed by raw `bd close` from ten callers). A row
/// already terminal (a landing recorded LANDED before its close) gets no event; a bead with
/// no row has no state to diverge, and is closed in the store alone. `--landing` is the landing
/// path's close: the delivery records LANDED itself (it may close first), so the row gets no
/// event — and a row that is not in delivery or LANDED is refused, so no other caller can use it
/// to close around the machine. `--delivered` ends work delivered by hand as DONE carrying the
/// reason (a READY or WORKING bead; a DROPPED one is corrected to DONE). `reason_file` reads
/// `--reason-file`'s argument (`-` is stdin) — injected so the composition is testable.
///
/// Exit: 0 closed · 2 cannot tell (usage, the machine unreadable, or the store close failed
/// after the event — the row is terminal either way) · 3 refused (the event lost its CAS:
/// nothing is closed, the store is left as it was).
pub fn close(
    args: &[String],
    m: &mut dyn Machine,
    bd: &mut dyn Bd,
    reason_file: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Answer {
    const USE: &str = "close <bead-id> (--reason <text> | --reason-file <file|->) [--superseded-by <id> | --landing | --delivered | --dropped] [--actor <name>]";
    let mut id = None;
    let (mut reason, mut by, mut actor, mut landing, mut delivered, mut dropped) = (None, None, "close".to_string(), false, false, false);
    let mut i = 0;
    while i < args.len() {
        let val = args.get(i + 1).cloned();
        match args[i].as_str() {
            "--landing" => {
                landing = true;
                i += 1;
                continue;
            }
            "--delivered" => {
                delivered = true;
                i += 1;
                continue;
            }
            "--dropped" => {
                dropped = true;
                i += 1;
                continue;
            }
            "--reason" => reason = val,
            "--reason-file" => match val.map(|f| reason_file(&f)) {
                Some(Ok(t)) => reason = Some(t),
                Some(Err(e)) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc close: --reason-file: {e}\n"), ..Default::default() },
                None => return usage(USE),
            },
            "--superseded-by" => by = val,
            "--actor" => actor = val.unwrap_or_default(),
            a if !a.starts_with('-') && id.is_none() => {
                id = Some(a.to_string());
                i += 1;
                continue;
            }
            _ => return usage(USE),
        }
        i += 2;
    }
    let (Some(id), Some(reason)) = (id.filter(|s| !s.is_empty()), reason.map(|r| r.trim_end().to_string()).filter(|r| !r.is_empty())) else {
        return usage(USE);
    };
    if actor.is_empty() || (landing && by.is_some()) || (delivered && (landing || by.is_some())) || (dropped && (landing || delivered || by.is_some())) {
        return usage(USE);
    }
    let asked_about = match bd.work_beads(&id) {
        Ok(w) => w,
        Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc close: {id}: its labels are unreadable — nothing closed: {}\n", e.trim()), ..Default::default() },
    };
    let mut rowless = false;
    match show(m, &id) {
        Err(NO_ROW) => rowless = true,
        Err(rc) => return Answer { code: rc, stderr: format!("spira-lc close: {id}: the lifecycle row is unreadable — nothing closed\n"), ..Default::default() },
        Ok(v) => {
            let (mut state, mut version) = (bead_field(&v, "state"), bead_field(&v, "version"));
            if state.is_empty() || version.is_empty() {
                return Answer { code: CANNOT_TELL, stderr: format!("spira-lc close: {id}: unreadable row\n"), ..Default::default() };
            }
            let terminal = matches!(state.as_str(), "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE");
            if delivered && state == "DROPPED" {
                let ev = serde_json::to_string(&BeadEventKind::Done { delivers: reason.clone() }).unwrap_or_default();
                match event(m, "bead", &id, &state, &version, lifecycle::bead::DELIVERED_CORRECTION_ACTOR, &ev) {
                    (APPLIED, _) => {}
                    (rc, out) => {
                        return Answer { code: rc, stderr: format!("spira-lc close: {id}: DROPPED -> DONE refused ({}) — nothing closed\n", out.trim()), ..Default::default() }
                    }
                }
            } else if landing {
                if !matches!(state.as_str(), "SUBMITTED" | "CERTIFIED" | "IN_DELIVERY" | "LANDED") {
                    return Answer {
                        code: REFUSED,
                        stderr: format!("spira-lc close --landing: {id} is {state}, not in delivery — nothing closed\n"),
                        ..Default::default()
                    };
                }
            } else if !terminal {
                if actor == "mail" && !reason.trim_start().to_lowercase().starts_with("drop") {
                    return Answer {
                        code: REFUSED,
                        stderr: format!("spira-lc close: {id}: a work bead in {state} is not closed by a mail reply — only an explicit DROP from the operator drops it\n"),
                        ..Default::default()
                    };
                }
                let kind = match by.as_deref().filter(|b| !b.is_empty()) {
                    Some(b) => BeadEventKind::Supersede { by: b.to_string() },
                    None if delivered => BeadEventKind::Done { delivers: reason.clone() },
                    None if dropped => BeadEventKind::Drop { reason: DropReason::Unwanted },
                    None => terminal_event_for(&reason),
                };
                if let Some(a) = external_refusal(bd, &id, &kind, &reason, "close") {
                    return a;
                }
                if holds_of(v.get("bead").and_then(|b| b.get("holds"))).iter().any(|h| h == "ask") {
                    let ev = serde_json::to_string(&BeadEventKind::AskWithdrawn).unwrap_or_default();
                    match event(m, "bead", &id, &state, &version, &actor, &ev).0 {
                        APPLIED => match show(m, &id) {
                            Ok(v) => (state, version) = (bead_field(&v, "state"), bead_field(&v, "version")),
                            Err(rc) => return Answer::code(rc),
                        },
                        rc => return Answer { code: rc, stderr: format!("spira-lc close: {id}: withdrawing its ask was refused — nothing closed\n"), ..Default::default() },
                    }
                }
                let ev = serde_json::to_string(&kind).unwrap_or_default();
                match event(m, "bead", &id, &state, &version, &actor, &ev) {
                    (APPLIED, _) => {}
                    (rc, out) => {
                        return Answer { code: rc, stderr: format!("spira-lc close: {id}: {state} -> terminal refused ({}) — nothing closed\n", out.trim()), ..Default::default() }
                    }
                }
            }
        }
    }
    match bd.close(&id, &reason) {
        Ok(()) => {
            if rowless {
                let a = close_ask_row(&id, &reason, &actor, m);
                if a.code != APPLIED {
                    return a;
                }
            }
            withdraw_named_asks(&id, &asked_about, &actor, m)
        }
        Err(e) => Answer {
            code: CANNOT_TELL,
            stderr: format!("spira-lc close: {id}: the lifecycle row is terminal but the store close failed: {}\n", e.trim()),
            ..Default::default()
        },
    }
}

/// A bead with no bead row may be an ask: its own machine records the close. Exit 1 (no ask
/// row either: an alert, an insight) and 3 (already closed) are nothing to record.
fn close_ask_row(id: &str, reason: &str, actor: &str, m: &mut dyn Machine) -> Answer {
    let args: Vec<String> = ["close-ask", id, "--exit", "withdrawn", "--quote", reason, "--actor", actor].iter().map(|x| x.to_string()).collect();
    match m.call(&args) {
        (0 | 1 | REFUSED, _) => Answer::code(APPLIED),
        (rc, out) => Answer { code: rc, stderr: format!("spira-lc close: {id} is closed but its ask row was not: {}\n", out.trim()), ..Default::default() },
    }
}

/// Closing an ask lifts the ask hold on each work bead it names and moves nothing else on
/// them: a work bead ends only through its own close.
fn withdraw_named_asks(ask: &str, work_beads: &[String], actor: &str, m: &mut dyn Machine) -> Answer {
    for w in work_beads {
        match with_row(m, w, actor, |_| Ok(BeadEventKind::AskWithdrawn)) {
            Answer { code: APPLIED | NO_ROW | REFUSED, .. } => {}
            a => {
                return Answer {
                    code: CANNOT_TELL,
                    stderr: format!("spira-lc close: {ask} is closed but the ask hold on {w} was not lifted: {}", a.stderr),
                    ..Default::default()
                }
            }
        }
    }
    Answer::code(APPLIED)
}

#[cfg(test)]
mod tests;
