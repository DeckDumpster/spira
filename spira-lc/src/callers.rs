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

use lifecycle::bead::{BeadEventKind, HoldKind};
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

/// The bd half a verb needs: an epic's own close. Behind a trait so the compositions below
/// are unit-testable without bd.
pub trait Bd {
    /// The bead's bd `issue_type` (`task`, `epic`, ...).
    fn issue_type(&mut self, id: &str) -> Result<String, String>;
    /// `bd close <id> --reason <reason>`.
    fn close(&mut self, id: &str, reason: &str) -> Result<(), String>;
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

#[cfg(test)]
mod tests;
