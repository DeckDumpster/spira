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

use std::collections::BTreeMap;

use lifecycle::bead::{BeadEventKind, BeadState, HoldKind};
use lifecycle::delivery::DeliveryEventKind;
use lifecycle::reason::{DropReason, GateRedReason, HoldCause, ReturnedReason};
use serde_json::Value;

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
    "renew",
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
            if !need(2) {
                return usage("hold <bead-id> <poison|ask|wait|operator> [cause] [actor]");
            }
            let actor = actor_or(args.get(3), "sentinel");
            hold(m, &a(0), &a(1), &a(2), &actor)
        }
        "unhold" => {
            if !need(2) {
                return usage("unhold <bead-id> <kind> [actor]");
            }
            let kind = a(1);
            let actor = actor_or(args.get(2), "sentinel");
            with_row(m, &a(0), &actor, |_| match HoldKind::from_str(&kind) {
                Some(k) => Ok(BeadEventKind::Unhold { kind: k }),
                None => Err(unknown_kind(&kind)),
            })
        }
        "reply" => {
            if !need(2) {
                return usage("reply <bead-id> <message-id> [actor]");
            }
            let message_id = a(1);
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
            list(m, &["list".into(), "--hold".into(), a(0)], |r| s(r, "bead_id"))
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
        "renew" => renew(m, args),
        other => usage(&format!("unknown caller verb {other:?}")),
    }
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
fn with_row(
    m: &mut dyn Machine,
    id: &str,
    actor: &str,
    build: impl FnOnce(&Value) -> Result<BeadEventKind, Answer>,
) -> Answer {
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
    Answer::code(event(m, "bead", id, &state, &version, actor, &serde_json::to_string(&kind).unwrap_or_default()).0)
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
        HoldKind::Operator => HoldCause::ManualHold,
    }
}

fn hold(m: &mut dyn Machine, id: &str, kind: &str, cause: &str, actor: &str) -> Answer {
    with_row(m, id, actor, |_| match HoldKind::from_str(kind) {
        Some(k) => Ok(BeadEventKind::Hold { kind: k, cause: hold_cause(k), detail: Some(cause.to_string()) }),
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

/// Move the row as close to SUBMITTED-at-<tip> as an event can: WORKING/REWORK advance by
/// Submit, and a CERTIFIED row at another tip is voided by Submit (the tip invariant, as
/// bead.rs's own Certified+Submit arm has it). None: the row is unreadable.
fn reach_submitted(m: &mut dyn Machine, id: &str, tip: &str, actor: &str) -> Option<(String, String)> {
    let (mut st, mut ver, row) = state_version(m, id)?;
    let resubmit = match st.as_str() {
        "WORKING" | "REWORK" => true,
        "CERTIFIED" => bead_field(&row, "tip") != tip,
        _ => false,
    };
    if resubmit {
        let _ = event(m, "bead", id, &st, &ver, actor, &submit_kind(tip));
        (st, ver, _) = state_version(m, id)?;
    }
    Some((st, ver))
}

fn certify(m: &mut dyn Machine, id: &str, tip: &str, outcome: &str, detail: &str, actor: &str) -> Answer {
    let Some((state, version)) = reach_submitted(m, id, tip, actor) else {
        return Answer::cert(CANNOT_TELL, "cannot-tell", "no lifecycle row yet".into());
    };
    // A pass on a row already CERTIFIED at this tip (reach_submitted resubmits any other
    // tip) is the same verdict again: idempotent success, no event. Answering "skip" made a
    // fail-closed caller treat a certified bead as uncertified (sp-e9o2y's CHECK6).
    if state == "CERTIFIED" && outcome == "pass" {
        return Answer::cert(APPLIED, "already", format!("pass tip={tip} — already CERTIFIED"));
    }
    if state != "SUBMITTED" {
        return Answer::cert(REFUSED, "skip", format!("state={state} tip={tip} outcome={outcome} — not SUBMITTED"));
    }
    let kind = match outcome {
        "pass" => BeadEventKind::GatePass { tip: tip.into(), gate_key: detail.into() },
        "red" => BeadEventKind::GateRed { tip: tip.into(), reason: gate_red_reason(detail) },
        "infra" => BeadEventKind::GateInfra { tip: tip.into() },
        other => return Answer::cert(CANNOT_TELL, "cannot-tell", format!("unknown outcome {other}")),
    };
    if event(m, "bead", id, "SUBMITTED", &version, actor, &serde_json::to_string(&kind).unwrap_or_default()).0 == 0 {
        return Answer::cert(APPLIED, "applied", format!("{outcome} tip={tip}"));
    }
    Answer::cert(REFUSED, "refused", format!("{outcome} tip={tip}"))
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
        return Answer::cert(REFUSED, "skip", format!("resubmit: state={state} not eligible for tip={tip}"));
    }
    if event(m, "bead", id, &state, &version, actor, &submit_kind(tip)).0 == 0 {
        return Answer::cert(APPLIED, "applied", format!("resubmit tip={tip}"));
    }
    Answer::cert(REFUSED, "refused", format!("resubmit tip={tip}"))
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

/// The ReturnedReason a reopen's free-text cause earns: an eject is the batch's own, anything
/// else a CERTIFIED or in-delivery bead is handed back for is the base having moved.
fn reopen_returned_reason(cause: &str) -> ReturnedReason {
    match cause {
        "eject" | "batch-eject" => ReturnedReason::BatchEjected,
        "base-withdrawn" => ReturnedReason::BaseWithdrawn,
        _ => ReturnedReason::PushRejected,
    }
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
        "IN_DELIVERY" => send(m, &state, &version, returned).0,
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
    /// The store half of a reopen: bd status open, no assignee, no submitted label — so a bead
    /// the machine handed back reads open wherever bd is still read (sp-swh8b8). Idempotent.
    fn reopen(&mut self, id: &str) -> Result<(), String>;
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

/// `reconcile-epics [--apply] [<id>...]` — move every non-terminal epic's row off READY (and any
/// other non-OPEN state the classifier would not give a container) to OPEN. Dry run unless
/// `--apply`. Only rows the epic rule may correct are touched, and only the READY/OPEN rows
/// are asked of bd, so the cost is one `issue_type` read per READY row, never a full roster.
///
/// Exit: 0 done (or listed) · 2 cannot tell · 3 an event was refused.
pub fn reconcile_epics(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let apply = args.iter().any(|a| a == "--apply");
    if let Some(bad) = args.iter().find(|a| a.starts_with('-') && *a != "--apply") {
        return usage(&format!("reconcile-epics [--apply] [<bead-id>...] (unknown flag {bad})"));
    }
    let only: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let (rc, out) = m.call(&["list".into(), "--state".into(), "READY".into()]);
    if rc != 0 {
        return Answer { code: rc.max(CANNOT_TELL), stderr: format!("spira-lc reconcile-epics: lifecycle list failed: {}\n", out.trim()), ..Default::default() };
    }
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(out.trim()) else {
        return Answer { code: CANNOT_TELL, stderr: "spira-lc reconcile-epics: lifecycle list is not a JSON array\n".into(), ..Default::default() };
    };
    let (mut lines, mut code, mut done) = (Vec::new(), APPLIED, 0usize);
    for r in rows.iter().filter(|r| s(r, "state") == "READY" && (only.is_empty() || only.iter().any(|i| **i == s(r, "bead_id")))) {
        let id = s(r, "bead_id");
        match bd.issue_type(&id) {
            Ok(t) if t == "epic" => {}
            Ok(_) => continue,
            Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc reconcile-epics: bd unreadable for {id}: {}\n", e.trim()), ..Default::default() },
        }
        if !apply {
            lines.push(format!("would move {id} READY -> OPEN (epic-container)"));
            done += 1;
            continue;
        }
        let kind = BeadEventKind::Reclassify { state: BeadState::Open, rule: "epic-container".into() };
        let (version, kind_json) = (s(r, "version"), serde_json::to_string(&kind).unwrap_or_default());
        match event(m, "bead", &id, "READY", &version, "classifier", &kind_json).0 {
            APPLIED => {
                lines.push(format!("moved {id} READY -> OPEN (epic-container)"));
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

/// `close-epic <id> <reason>` — close an EPIC bead in bd (pilgrimage.sh's completion close,
/// sp-hyo5e). An epic is a grouping, never a lifecycle bead: it is excluded from every claim
/// (`--exclude-type epic`), so it has no READY..LANDED path and bd's open/closed is its only
/// state. This verb is the one door for that close, and it refuses anything that is not an
/// epic — a work bead's end is the machine's `done`/delivery, never a bd close.
///
/// Exit: 0 closed · 2 cannot tell (bd unreadable, or the close failed) · 3 refused (not an epic).
pub fn close_epic(args: &[String], bd: &mut dyn Bd) -> Answer {
    let (Some(id), Some(reason)) = (args.first().filter(|s| !s.is_empty()), args.get(1)) else {
        return usage("close-epic <bead-id> <reason>");
    };
    match bd.issue_type(id) {
        Err(e) => Answer { code: CANNOT_TELL, stderr: format!("spira-lc close-epic: cannot read {id}: {}\n", e.trim()), ..Default::default() },
        Ok(t) if t != "epic" => Answer {
            code: REFUSED,
            stderr: format!("spira-lc close-epic: {id} is a {t}, not an epic — a work bead ends through the lifecycle machine, never a bd close\n"),
            ..Default::default()
        },
        Ok(_) => match bd.close(id, reason) {
            Ok(()) => Answer::code(APPLIED),
            Err(e) => Answer { code: CANNOT_TELL, stderr: format!("spira-lc close-epic: bd close {id} failed: {}\n", e.trim()), ..Default::default() },
        },
    }
}

/// `close <id> (--reason R | --reason-file F|-) [--superseded-by X | --landing] [--actor A]` — the one
/// door for closing a bead (sp-3fue0j). The lifecycle machine records the end FIRST — an open
/// ask is withdrawn, then SUPERSEDED (a successor named by `--superseded-by` or in the reason)
/// or DROPPED — and only then is the store closed, so a closed bead can never sit READY on
/// its row again (237 did on 2026-10-06, closed by raw `bd close` from ten callers). A row
/// already terminal (a landing recorded LANDED before its close) gets no event; a bead with
/// no row has no state to diverge, and is closed in the store alone. `--landing` is the landing
/// path's close: the delivery records LANDED itself (it may close first), so the row gets no
/// event — and a row that is not in delivery or LANDED is refused, so no other caller can use it
/// to close around the machine. `reason_file` reads
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
    const USE: &str = "close <bead-id> (--reason <text> | --reason-file <file|->) [--superseded-by <id> | --landing] [--actor <name>]";
    let mut id = None;
    let (mut reason, mut by, mut actor, mut landing) = (None, None, "close".to_string(), false);
    let mut i = 0;
    while i < args.len() {
        let val = args.get(i + 1).cloned();
        match args[i].as_str() {
            "--landing" => {
                landing = true;
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
    if actor.is_empty() || (landing && by.is_some()) {
        return usage(USE);
    }
    match show(m, &id) {
        Err(NO_ROW) => {}
        Err(rc) => return Answer { code: rc, stderr: format!("spira-lc close: {id}: the lifecycle row is unreadable — nothing closed\n"), ..Default::default() },
        Ok(v) => {
            let (mut state, mut version) = (bead_field(&v, "state"), bead_field(&v, "version"));
            if state.is_empty() || version.is_empty() {
                return Answer { code: CANNOT_TELL, stderr: format!("spira-lc close: {id}: unreadable row\n"), ..Default::default() };
            }
            let terminal = matches!(state.as_str(), "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE");
            if landing {
                if !matches!(state.as_str(), "SUBMITTED" | "CERTIFIED" | "IN_DELIVERY" | "LANDED") {
                    return Answer {
                        code: REFUSED,
                        stderr: format!("spira-lc close --landing: {id} is {state}, not in delivery — nothing closed\n"),
                        ..Default::default()
                    };
                }
            } else if !terminal {
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
                let kind = match by.as_deref().filter(|b| !b.is_empty()) {
                    Some(b) => BeadEventKind::Supersede { by: b.to_string() },
                    None => terminal_event_for(&reason),
                };
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
        Ok(()) => Answer::code(APPLIED),
        Err(e) => Answer {
            code: CANNOT_TELL,
            stderr: format!("spira-lc close: {id}: the lifecycle row is terminal but the store close failed: {}\n", e.trim()),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests;
