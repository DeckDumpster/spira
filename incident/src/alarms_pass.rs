//! `incident alarms-pass`: the alarms component, once a minute on the observability plane. It
//! applies retry-delivery intents, pages from the goodput pass's record and from conditions
//! declared page-on-raise, keeps the floor's one standing ask, runs the dead-man check over the
//! goodput pass and pings the off-box heartbeat, then publishes the Alarms section. The reconciler
//! pages no one; every page is sent here (law-a-reconciler-redrives-it-does-not-alarm).
//!
//! Every effect is a port, and every attempt to reach someone is recorded with its outcome, so a
//! silent failure becomes the Alarms section's gap.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reconciler_engine::alert::{classify_escalation, EscalationClass};
use reconciler_engine::goodput_pass::Lc;
use reconciler_engine::pass_record::PassRecord;
use runtime::ports::Clock;
use serde_json::{json, Value};
use spira_desired_state::intent::{parse_intent, parse_key};
use spira_desired_state::section::{parse_rfc3339, parse_section, ConditionStatus, Freshness, Section, Windowed};
use spira_desired_state::sections::{self, alarms, lifecycle};
use spira_desired_state::store::format_rfc3339;
use tsd::alarm::{Attempt, Message, Outcome};

use crate::attempts;
use crate::page::{self, Candidate, Decision, Row, Source, Verdict};

pub const FROM: &str = "Alarms <alarms@spira>";
/// The attempt kinds this component records.
pub const PAGE: &str = "page";
pub const FLOOR: &str = "floor";
pub const PROBE: &str = "probe";
pub const CALLER: &str = "alarms";
pub const RETRY_CALLER: &str = "alarms-retry";
pub const PROBE_CALLER: &str = "alarms-probe";
pub const PAGES_STATE: &str = "alarms-pages.tsv";
pub const FLOOR_STATE: &str = "alarms-floor.tsv";
/// The identity a dead-man page is sent under.
pub const DEADMAN_SECTION: &str = "reconciler";
/// The goodput pass's unit base: its installed timer is `spira_config::unit`'s name for it.
pub const GOODPUT_UNIT: &str = "reconciler-goodput";
pub const OBSERVABILITY_HALT: &str = "world.halted.observability";
pub const NO_HEARTBEAT_URL: &str = "no heartbeat url configured";
pub const HEARTBEAT_DEADLINE: Duration = Duration::from_secs(5);
/// The predicate that moots the floor's standing ask.
pub const FLOOR_MOOT: &str = "incident floor-cleared";
const TARGET_LATENCY_S: u64 = 300;

/// The primary path: `mail send`, which files a tracking bead for an operator ask.
pub trait Primary {
    /// The id the alarm was delivered as.
    fn send(&self, m: &Message) -> Result<String, String>;
    /// A note on the ask an earlier page opened.
    fn note(&self, ask: &str, text: &str) -> Result<(), String>;
}

/// The fallback: a direct write into the mailbox.
pub trait Fallback {
    fn deliver(&self, m: &Message) -> Result<String, String>;
}

pub trait Http {
    fn get(&self, url: &str, deadline: Duration) -> Result<String, String>;
}

/// Whether the operator turned the goodput pass off: its plane is stopped, or its timer is not on.
pub trait Switch {
    fn observability_halted(&self) -> bool;
    fn goodput_timer_on(&self) -> bool;
}

/// The halt marker in the run directory, and the timer as the caller asks systemd about it.
pub struct FileSwitch<'a> {
    pub run_dir: &'a Path,
    pub timer_on: &'a dyn Fn() -> bool,
}

impl Switch for FileSwitch<'_> {
    fn observability_halted(&self) -> bool {
        self.run_dir.join(OBSERVABILITY_HALT).exists()
    }

    fn goodput_timer_on(&self) -> bool {
        (self.timer_on)()
    }
}

/// `mail_deliver` straight into `<mail_root>/<mailbox>`.
pub struct MaildirFallback {
    pub mail_root: PathBuf,
    pub mute: bool,
}

impl Fallback for MaildirFallback {
    fn deliver(&self, m: &Message) -> Result<String, String> {
        use mail::maildir;
        maildir::mailbox_valid(&m.mailbox)?;
        let dir = self.mail_root.join(&m.mailbox);
        maildir::mail_ensure(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let msgid = maildir::mint_msgid();
        let mut text = String::new();
        for (h, v) in [("From", FROM), ("Subject", m.subject.as_str()), ("X-Spira-Kind", m.kind.as_str()), ("X-Spira-Default", m.default.as_str())] {
            if !v.is_empty() {
                text.push_str(&mail::message::header(h, v)?);
            }
        }
        text.push_str(&format!("X-Spira-Delivery: fallback\nMessage-ID: <{msgid}@spira>\n\n{}\n", m.body));
        let tmp = dir.join("tmp").join(&msgid);
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        maildir::mail_deliver(&dir, &msgid, self.mute).map_err(|e| e.to_string())?;
        Ok(msgid)
    }
}

/// The primary path in-process: the same `send` the `mail` CLI runs.
pub struct MailPrimary {
    pub env: mail::env::Env,
    pub bd: mail::bead::BdCli,
}

impl Primary for MailPrimary {
    fn send(&self, m: &Message) -> Result<String, String> {
        let args = mail::cmds::SendArgs {
            mailbox: &m.mailbox,
            from: Some(FROM),
            subject: &m.subject,
            kind: &m.kind,
            default: &m.default,
            class: m.class.as_deref().unwrap_or(""),
            bead: "",
            urgent: false,
            digest: false,
            dry_run: false,
            requested_by: "",
        };
        let out = mail::cmds::send(&self.bd, &self.env, &args, m.body.clone())?;
        Ok(out.x_bead.unwrap_or_else(|| out.delivered_path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()))
    }

    fn note(&self, ask: &str, text: &str) -> Result<(), String> {
        if ask.is_empty() || !ask.contains('-') {
            return Ok(());
        }
        mail::bead::note(&self.bd, ask, text)
    }
}

/// One store call over `client`, its answer as JSON: a call with no answer is exit -1 and an
/// answer that is not JSON is a `cannot_tell` naming what it said instead.
pub fn lc_json(client: &spira_config::lc_call::LcClient, args: &[String]) -> (i32, Value) {
    match client.call(args) {
        Ok(r) => {
            let v = serde_json::from_str(r.out.trim()).unwrap_or_else(|_| json!({ "cannot_tell": format!("spira-lc answered non-JSON: {}", r.err.trim()) }));
            (r.code, v)
        }
        Err(e) => (-1, json!({ "cannot_tell": e.to_string() })),
    }
}

pub struct Ports<'a> {
    pub lc: &'a dyn Lc,
    pub primary: &'a dyn Primary,
    pub fallback: &'a dyn Fallback,
    pub http: &'a dyn Http,
    pub switch: &'a dyn Switch,
    pub clock: &'a dyn Clock,
}

pub struct Config {
    /// Where the attempt records and the page state live.
    pub run_dir: PathBuf,
    pub host: String,
    pub deadman_secs: u64,
    pub heartbeat_url: String,
    /// False is the forced mode: the primary path alone, so a failure is seen as one.
    pub fallback: bool,
    pub floor_proposal: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeadMan {
    /// The operator turned the pass off; nothing is judged.
    Waiting(String),
    Fresh { age_secs: u64 },
    /// `None`: no pass record at all.
    Stale { age_secs: Option<u64> },
}

#[derive(Debug, Default)]
pub struct Report {
    /// References of the pages sent, whatever path reached someone.
    pub sent: Vec<String>,
    pub noted: Vec<String>,
    /// Each retry-delivery intent and the ack it was given.
    pub retried: Vec<(String, String)>,
    /// The binding condition a floor ask was filed for.
    pub floor: Option<String>,
    pub deadman: Option<DeadMan>,
    pub heartbeat: Option<String>,
    pub heartbeat_state: Option<alarms::HeartbeatState>,
    pub undelivered_open: usize,
    pub published: bool,
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn now_secs(p: &Ports) -> u64 {
    p.clock.now_ms() / 1000
}

/// One attempt through the primary path, then — unless the forced mode is on — the fallback;
/// each attempt recorded. The id it was delivered as, or why it reached no one.
#[allow(clippy::too_many_arguments)]
pub fn deliver(p: &Ports, cfg: &Config, caller: &str, kind: &str, reference: &str, m: &Message, fallback: bool) -> Result<String, String> {
    let record = |outcome: Outcome, message: Option<Message>| {
        let a = Attempt { at: now_secs(p), caller: caller.into(), kind: kind.into(), reference: reference.into(), outcome, message };
        if let Err(e) = a.write(&cfg.run_dir, &cfg.host) {
            eprintln!("incident: alarm attempt not recorded: {e}");
        }
    };
    let reason = match p.primary.send(m) {
        Ok(id) if !id.trim().is_empty() => {
            record(Outcome::Delivered { id: id.clone() }, None);
            return Ok(id);
        }
        Ok(_) => "the primary path returned no id".to_string(),
        Err(e) => e,
    };
    record(Outcome::Failed { reason: reason.clone() }, Some(m.clone()));
    if !fallback {
        return Err(reason);
    }
    match p.fallback.deliver(m) {
        Ok(id) => {
            record(Outcome::DeliveredFallback, None);
            Ok(id)
        }
        Err(e) => {
            let reason = format!("{reason}; fallback: {e}");
            record(Outcome::Failed { reason: reason.clone() }, Some(m.clone()));
            Err(reason)
        }
    }
}

/// A section as the snapshot shows it, and its freshness.
struct Seen {
    section: Section,
    fresh: bool,
}

impl Seen {
    fn reference(&self) -> String {
        format!("{}/{}", self.section.kind, self.section.metadata.name)
    }

    fn open(&self) -> impl Iterator<Item = &spira_desired_state::section::Condition> {
        self.section.status.conditions.iter().filter(|c| c.status == ConditionStatus::True)
    }
}

fn seen(snap: &Value) -> Vec<Seen> {
    let items = snap.get("sections").and_then(Value::as_array).cloned().unwrap_or_default();
    items
        .iter()
        .filter_map(|i| {
            let section = parse_section(i.get("section")?).ok()?;
            let fresh = serde_json::from_value::<Freshness>(i.get("freshness")?.clone()).ok()? == Freshness::Fresh;
            Some(Seen { section, fresh })
        })
        .collect()
}

fn class_of(section: &str) -> Option<String> {
    let kind = sections::lookup(section.split_once('/').map_or(section, |(k, _)| k))?;
    Some(
        match classify_escalation(kind.escalation?)? {
            EscalationClass::Permissions => "permissions",
            EscalationClass::Policy => "policy",
            EscalationClass::Destructive => "destructive",
        }
        .into(),
    )
}

fn condition_facts(s: Option<&Seen>, now: u64) -> Vec<String> {
    let Some(s) = s else { return vec![] };
    s.open()
        .map(|c| {
            let age = parse_rfc3339(&c.last_transition_time).map_or("unknown".into(), |t| format!("{}s", now.saturating_sub(t)));
            format!("condition: {} — {} (open {age})", c.type_, c.reason)
        })
        .collect()
}

/// Each step the pass record shows for `section`, with its result, in a form that changes only
/// when what was tried does.
fn steps_tried(rec: &PassRecord, section: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in rec.steps.iter().filter(|s| s.section == section) {
        out.push(format!("{} on {}: {} -> {} written", s.action, s.target, s.from, s.to));
    }
    for s in rec.would_step.iter().filter(|s| s.section == section) {
        out.push(format!("{} on {}: would step ({})", s.action, s.target, s.held.as_deref().unwrap_or("held")));
    }
    for r in rec.refused.iter().filter(|r| r.section == section) {
        out.push(format!("{}: refused ({})", r.action, r.code));
    }
    for j in &rec.judged {
        if parse_key(&j.intent).is_some_and(|(k, n, _)| format!("{k}/{n}") == section) {
            out.push(format!("{}: judged {:?}", j.intent, j.result));
        }
    }
    out
}

fn record_facts(rec: &PassRecord, section: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(g) = rec.gaps.iter().find(|g| g.section == section) {
        out.push(format!("distance: {}", g.distance));
    }
    if let Some(a) = rec.ages.get(section).and_then(|a| a.age_secs) {
        out.push(format!("section age: {a}s"));
    }
    out.push(format!("pass: {} at {}", rec.n, rec.at));
    out
}

fn from_pass(rec: &PassRecord, seen: &[Seen], now: u64) -> Vec<Candidate> {
    let find = |r: &str| seen.iter().find(|s| s.reference() == r);
    let mut out = Vec::new();
    for nl in &rec.no_lever {
        let decision = Decision {
            question: format!("{} has open conditions no declared action addresses ({}): which lever, if any?", nl.section, nl.conditions.join(", ")),
            default: "No step is taken; the owning component re-drives its own work.".into(),
        };
        out.push(with_evidence(Candidate::new(&nl.section, Source::NoLever, nl.conditions.clone(), decision), rec, find(&nl.section), now));
    }
    for s in &rec.exhausted {
        let mut conds: Vec<String> = find(s).map(|x| x.open().map(|c| c.type_.clone()).collect()).unwrap_or_default();
        if conds.is_empty() {
            conds.push("Exhausted".into());
        }
        let decision = Decision {
            question: format!("Every action for {s} failed inside its back-off: what should change?"),
            default: "The reconciler keeps backing off; no new kind of step is taken.".into(),
        };
        out.push(with_evidence(Candidate::new(s, Source::Exhausted, conds, decision), rec, find(s), now));
    }
    out
}

fn with_evidence(mut c: Candidate, rec: &PassRecord, s: Option<&Seen>, now: u64) -> Candidate {
    c.class = class_of(&c.section);
    c.facts = record_facts(rec, &c.section);
    c.facts.extend(condition_facts(s, now));
    c.steps = steps_tried(rec, &c.section);
    c
}

fn raised(seen: &[Seen], now: u64) -> Vec<Candidate> {
    let mut out = Vec::new();
    for s in seen.iter().filter(|s| s.section.kind != alarms::NAME) {
        let Some(kind) = sections::lookup(&s.section.kind) else { continue };
        for c in s.open().filter(|c| kind.condition(&c.type_).is_some_and(|d| d.page_on_raise)) {
            let r = s.reference();
            let decision = Decision {
                question: format!("{} is raised on {r}: what should be done?", c.type_),
                default: "The Concierge works it from the runbook entry.".into(),
            };
            let mut cand = Candidate::new(&r, Source::Raised { since: c.last_transition_time.clone() }, vec![c.type_.clone()], decision);
            cand.class = class_of(&r);
            cand.facts = condition_facts(Some(s), now);
            if !s.fresh {
                cand.facts.push("the section is stale".into());
            }
            out.push(cand);
        }
    }
    out
}

pub fn deadman(switch: &dyn Switch, pass: Option<&PassRecord>, now: u64, deadman_secs: u64) -> DeadMan {
    if switch.observability_halted() {
        return DeadMan::Waiting(format!("{OBSERVABILITY_HALT}: the observability plane is stopped"));
    }
    if !switch.goodput_timer_on() {
        return DeadMan::Waiting(format!("the {GOODPUT_UNIT} timer is not installed or not enabled"));
    }
    match pass.and_then(|r| parse_rfc3339(&r.at)).map(|at| now.saturating_sub(at)) {
        Some(age_secs) if age_secs <= deadman_secs => DeadMan::Fresh { age_secs },
        age_secs => DeadMan::Stale { age_secs },
    }
}

fn deadman_candidate(pass: Option<&PassRecord>, age: Option<u64>, deadman_secs: u64) -> Candidate {
    let decision = Decision {
        question: "The goodput pass has stopped recording passes: restart it, or stop its timer if it is meant to be off?".into(),
        default: "The Concierge restarts the goodput pass and reads its log.".into(),
    };
    let mut c = Candidate::new(DEADMAN_SECTION, Source::DeadMan, vec![alarms::RECONCILER_PASS_STALE.into()], decision);
    c.facts.push(match age {
        Some(a) => format!("last pass record: {a}s old, past the {deadman_secs}s dead-man"),
        None => "no pass record exists".into(),
    });
    if let Some(r) = pass {
        c.facts.push(format!("pass: {} at {} by {}", r.n, r.at, r.holder));
    }
    c
}

fn read_rows(path: &Path) -> Vec<Row> {
    page::parse(&std::fs::read_to_string(path).unwrap_or_default())
}

fn write_rows(path: &Path, rows: &[Row]) {
    if let Err(e) = std::fs::write(path, page::render(rows)) {
        eprintln!("incident: page state not saved at {}: {e}", path.display());
    }
}

fn message_of(c: &Candidate) -> Message {
    Message {
        mailbox: if c.class.is_some() { "operator" } else { "concierge" }.into(),
        subject: c.subject(),
        kind: "question".into(),
        class: c.class.clone(),
        default: c.decision.default.clone(),
        body: c.body(),
    }
}

fn page_all(p: &Ports, cfg: &Config, cands: &[Candidate], now: u64, report: &mut Report) {
    let path = cfg.run_dir.join(PAGES_STATE);
    let mut rows = read_rows(&path);
    let now_i = now as i64;
    for c in cands {
        match page::decide(&rows, c, now_i) {
            Verdict::Quiet => {}
            Verdict::Send => {
                // A send that reaches no one is still the page: retry-delivery re-sends it.
                let ask = deliver(p, cfg, CALLER, PAGE, &c.reference(), &message_of(c), cfg.fallback);
                if ask.is_ok() {
                    report.sent.push(c.reference());
                }
                rows = page::sent(&rows, c, ask.as_deref().unwrap_or(""), now_i);
            }
            Verdict::Note { ask } => {
                let text = format!("Changed inside the page window, not re-sent:\n{}", c.body());
                if let Err(e) = p.primary.note(&ask, &text) {
                    eprintln!("incident: note on {ask} not written: {e}");
                }
                report.noted.push(c.reference());
                rows = page::noted(&rows, c, now_i);
            }
        }
    }
    write_rows(&path, &rows);
}

fn numeric(s: &Section, field: &str) -> Option<f64> {
    s.status.fields.get(field)?.get("value")?.as_f64()
}

fn floor(p: &Ports, cfg: &Config, seen: &[Seen], pass: Option<&PassRecord>, pass_raw: &Value, now: u64, report: &mut Report) {
    let path = cfg.run_dir.join(FLOOR_STATE);
    let lc = seen.iter().find(|s| s.section.kind == lifecycle::NAME && s.section.metadata.name == lifecycle::SECTION);
    let below = lc.is_some_and(|s| s.open().any(|c| c.type_ == lifecycle::BELOW_FLOOR));
    let rows = read_rows(&path);
    let Some(lc) = lc.filter(|_| below) else {
        if !rows.is_empty() {
            write_rows(&path, &[]);
        }
        return;
    };
    let binding = pass.and_then(|r| r.binding.clone()).unwrap_or_else(|| "none recorded".into());
    if rows.iter().any(|r| r.condition == binding) {
        return;
    }
    let rate = numeric(&lc.section, "goodput_rate_24h").map_or("unmeasured".into(), |v| format!("{v:.1}/day"));
    let floor = lc.section.spec.get("goodput_rate_floor").map_or("undeclared".into(), |t| t.value.to_string());
    let mut evidence = format!("goodput rate (24 h, unhalted): {rate}, floor {floor}\nbinding condition: {binding}\n");
    let path_to_goodput = pass_raw
        .get("path_to_goodput")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" -> "))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "none recorded".into());
    evidence.push_str(&format!("path to goodput: {path_to_goodput}\ndistances:\n"));
    for g in pass.map(|r| r.gaps.as_slice()).unwrap_or_default() {
        evidence.push_str(&format!("- {}: {}\n", g.section, g.distance));
    }
    let proposal = cfg.floor_proposal.clone().unwrap_or_else(|| "no proposal recorded".into());
    let default = "The world runs below the floor while the binding constraint is structural, and law-throughput-floor is amended to say so.";
    let body = format!(
        "## Question\nThe 24 h goodput rate is below the floor (law-throughput-floor). Does the world run below the floor while {binding} binds?\n\n## Default\n{default}\n\n## Evidence\n{evidence}\n## Proposal\n{proposal}\n\n## Class basis\nWhether the world runs below a statute's floor is policy (law-throughput-floor).\n\nMOOT-WHEN: {FLOOR_MOOT}\n"
    );
    let m = Message { mailbox: "operator".into(), subject: format!("goodput below the floor: {binding} binds"), kind: "question".into(), class: Some("policy".into()), default: default.into(), body };
    let ask = deliver(p, cfg, CALLER, FLOOR, &format!("floor:{binding}"), &m, cfg.fallback);
    let row = Row { section: FLOOR.into(), condition: binding.clone(), identity: String::new(), evidence: String::new(), ask: ask.unwrap_or_default(), sent: now as i64 };
    write_rows(&path, &[row]);
    report.floor = Some(binding);
}

fn ping(p: &Ports, cfg: &Config) -> (String, alarms::HeartbeatState) {
    use alarms::HeartbeatState;
    if cfg.heartbeat_url.trim().is_empty() {
        return (NO_HEARTBEAT_URL.into(), HeartbeatState::Unconfigured);
    }
    match p.http.get(&cfg.heartbeat_url, HEARTBEAT_DEADLINE) {
        Ok(s) => (s, HeartbeatState::Delivered),
        Err(e) => (format!("failed: {e}"), HeartbeatState::Failed),
    }
}

fn retry(p: &Ports, cfg: &Config, snap: &Value, report: &mut Report) {
    let intents = snap.get("intents").and_then(Value::as_array).cloned().unwrap_or_default();
    for item in intents {
        let Some(key) = item.get("key").and_then(Value::as_str) else { continue };
        if parse_key(key).is_none_or(|(k, _, _)| k != alarms::NAME) {
            continue;
        }
        let Some(i) = item.get("intent").and_then(|v| parse_intent(v).ok()) else { continue };
        if i.ack.is_some() || i.action != alarms::RETRY_DELIVERY {
            continue;
        }
        let all = tsd::alarm::read(&cfg.run_dir);
        let open = attempts::undelivered(&all, now_secs(p));
        let ack = |extra: &[&str]| {
            let mut args = argv(&["intent", "ack", key, "--actor", alarms::OWNER]);
            args.extend(extra.iter().map(|s| s.to_string()));
            args
        };
        let (args, word) = match open.iter().find_map(|a| Some((a, a.message.as_ref()?))) {
            None if open.is_empty() => (ack(&["--result", "already-applied"]), "already-applied".to_string()),
            None => (ack(&["--result", "refused", "--reason", "no undelivered alarm recorded what to re-send"]), "refused".into()),
            Some((a, m)) => {
                match deliver(p, cfg, RETRY_CALLER, &a.kind, &a.reference, m, false) {
                    Ok(_) => (argv(&["intent", "apply", key, "--knob", &i.target, "--actor", alarms::OWNER]), "applied".into()),
                    Err(e) => (ack(&["--result", "refused", "--reason", &e]), "refused".into()),
                }
            }
        };
        let (code, v) = p.lc.call(&args);
        if code != 0 {
            eprintln!("incident: ack of {key} not written: exit {code}: {v}");
        }
        report.retried.push((key.to_string(), word));
    }
}

fn level(v: f64) -> Value {
    json!({ "value": v, "window": "level" })
}

fn day(v: u64) -> Value {
    json!({ "value": v as f64, "window": "24h" })
}

fn condition(prev: Option<&Section>, type_: &str, on: bool, reason: &str, now: u64) -> Value {
    let status = if on { "True" } else { "False" };
    let want = if on { ConditionStatus::True } else { ConditionStatus::False };
    let since = prev
        .and_then(|s| s.status.conditions.iter().find(|c| c.type_ == type_ && c.status == want))
        .map_or_else(|| format_rfc3339(now), |c| c.last_transition_time.clone());
    json!({ "type": type_, "status": status, "reason": reason, "lastTransitionTime": since })
}

fn publish(p: &Ports, cfg: &Config, seen: &[Seen], pass: Option<&PassRecord>, dm: &DeadMan, report: &mut Report, now: u64) -> Result<(), String> {
    let knob = reconciler_engine::select::knob_key(&format!("{}/{}", alarms::NAME, alarms::SECTION), alarms::RETRY_DELIVERY);
    let (code, v) = p.lc.call(&argv(&["intent", "knob-init", &knob, "--actor", alarms::OWNER, "--json", "0"]));
    if code != 0 {
        eprintln!("incident: {knob} not initialized: exit {code}: {v}");
    }
    let all = tsd::alarm::read(&cfg.run_dir);
    let open = attempts::undelivered(&all, now);
    let n = attempts::counts(&all, now, PAGE);
    report.undelivered_open = open.len();
    let prev = seen.iter().find(|s| s.section.kind == alarms::NAME && s.section.metadata.name == alarms::SECTION).map(|s| &s.section);
    let stale = matches!(dm, DeadMan::Stale { .. });
    let deadman = match dm {
        DeadMan::Waiting(why) => format!("WAITING: {why}"),
        DeadMan::Fresh { .. } => "fresh".into(),
        DeadMan::Stale { age_secs: Some(_) } => "stale: the last pass record is past the dead-man".into(),
        DeadMan::Stale { age_secs: None } => "stale: no pass record".into(),
    };
    let deadman_state = match dm {
        DeadMan::Waiting(_) => alarms::DeadManState::Waiting,
        DeadMan::Fresh { .. } => alarms::DeadManState::Fresh,
        DeadMan::Stale { .. } => alarms::DeadManState::Stale,
    };
    let mut status = json!({
        "observedGeneration": prev.and_then(|s| s.metadata.generation).unwrap_or(0),
        "conditions": [
            condition(prev, alarms::UNDELIVERED, !open.is_empty(), &format!("{} alarm(s) reached no one", open.len()), now),
            condition(prev, alarms::RECONCILER_PASS_STALE, stale, &deadman, now),
        ],
        alarms::UNDELIVERED_OPEN: level(open.len() as f64),
        "attempts_24h": day(n.attempts),
        "delivered_24h": day(n.delivered),
        "failed_24h": day(n.failed),
        "pages_sent_24h": day(n.pages),
        "deadman": deadman,
        alarms::DEADMAN_STATE: deadman_state,
        "undelivered": open.iter().map(|a| json!({ "reference": a.reference, "kind": a.kind, "at": format_rfc3339(a.at) })).collect::<Vec<_>>(),
    });
    if let Some(h) = &report.heartbeat {
        status["heartbeat_last"] = json!(h);
    } else if let Some(h) = prev.and_then(|s| s.status.fields.get("heartbeat_last")) {
        status["heartbeat_last"] = h.clone();
    }
    if let Some(h) = &report.heartbeat_state {
        status[alarms::HEARTBEAT_STATE] = json!(h);
    } else if let Some(h) = prev.and_then(|s| s.status.fields.get(alarms::HEARTBEAT_STATE)) {
        status[alarms::HEARTBEAT_STATE] = h.clone();
    }
    let unchanged = prev.is_some_and(|s| {
        let mut was = serde_json::to_value(&s.status).unwrap_or(Value::Null);
        if let Some(o) = was.as_object_mut() {
            o.remove("pass_age_secs");
        }
        let kept = s.metadata.observed_at.as_deref().and_then(parse_rfc3339).is_some_and(|t| now.saturating_sub(t) < TARGET_LATENCY_S);
        kept && was == status
    });
    if let Some(r) = pass.and_then(|r| parse_rfc3339(&r.at)) {
        status["pass_age_secs"] = level(now.saturating_sub(r) as f64);
    }
    if unchanged {
        return Ok(());
    }
    let body = json!({
        "apiVersion": "spira/v1",
        "kind": alarms::NAME,
        "metadata": { "name": alarms::SECTION, "owner": alarms::OWNER, "observedAt": format_rfc3339(now), "target_latency": "5m", "max_age": "15m" },
        "status": status,
        "distance": { "fn": alarms::UNDELIVERED_OPEN, "open": 0.5, "close": 0.25 },
        "actions": [{
            "name": alarms::RETRY_DELIVERY, "class": "local", "step": "one failed alarm", "moves": alarms::UNDELIVERED_OPEN,
            "addresses": [alarms::UNDELIVERED], "expected_effect": -1.0, "horizon": "10m",
        }],
    });
    let rev = prev.and_then(|s| s.metadata.resource_version.clone()).unwrap_or_else(|| "0".into());
    let (code, v) = p.lc.call(&argv(&["section", "publish-status", "--actor", alarms::OWNER, "--expect-rev", &rev, "--json", &body.to_string()]));
    if code != 0 {
        return Err(format!("publish Alarms/{}: exit {code}: {v}", alarms::SECTION));
    }
    report.published = true;
    Ok(())
}

/// One alarms pass. `Err` when the snapshot cannot be read or the section cannot be published:
/// a pass that cannot see or say what it saw has not run.
pub fn run(p: &Ports, cfg: &Config) -> Result<Report, String> {
    let (code, snap) = p.lc.call(&argv(&["section", "snapshot"]));
    if code != 0 {
        return Err(format!("section snapshot: exit {code}: {snap}"));
    }
    let now = now_secs(p);
    let seen = seen(&snap);
    let pass: Option<PassRecord> = match snap.get("pass").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(serde_json::from_value(v.clone()).map_err(|e| format!("the pass record does not decode: {e}"))?),
    };
    let mut report = Report::default();
    retry(p, cfg, &snap, &mut report);
    let dm = deadman(p.switch, pass.as_ref(), now, cfg.deadman_secs);
    let mut cands = pass.as_ref().map(|r| from_pass(r, &seen, now)).unwrap_or_default();
    cands.extend(raised(&seen, now));
    if let DeadMan::Stale { age_secs } = dm {
        cands.push(deadman_candidate(pass.as_ref(), age_secs, cfg.deadman_secs));
    }
    page_all(p, cfg, &cands, now, &mut report);
    floor(p, cfg, &seen, pass.as_ref(), snap.get("pass").unwrap_or(&Value::Null), now, &mut report);
    if matches!(dm, DeadMan::Fresh { .. }) {
        let (said, state) = ping(p, cfg);
        report.heartbeat = Some(said);
        report.heartbeat_state = Some(state);
    }
    publish(p, cfg, &seen, pass.as_ref(), &dm, &mut report, now)?;
    report.deadman = Some(dm);
    Ok(report)
}

/// `incident floor-cleared`: the Lifecycle section's 24 h goodput rate is at least its floor, and
/// neither warming up nor stale. `Err` when it cannot be read, which a MOOT-WHEN reads as live.
pub fn floor_cleared(lc: &dyn Lc) -> Result<bool, String> {
    let r = format!("{}/{}", lifecycle::NAME, lifecycle::SECTION);
    let (code, v) = lc.call(&argv(&["section", "get", &r]));
    if code != 0 {
        return Err(format!("section get {r}: exit {code}: {v}"));
    }
    let s = parse_section(v.get("section").unwrap_or(&Value::Null)).map_err(|e| e.to_string())?;
    let fresh = v.get("freshness").and_then(|f| serde_json::from_value::<Freshness>(f.clone()).ok()) == Some(Freshness::Fresh);
    let floor = s.spec.get("goodput_rate_floor").and_then(|t| t.value.as_f64()).ok_or("the Lifecycle section declares no goodput_rate_floor")?;
    let rate = s.status.fields.get("goodput_rate_24h").and_then(|f| serde_json::from_value::<Windowed>(f.clone()).ok());
    Ok(fresh && matches!(rate, Some(Windowed::Value(n)) if n.value >= floor))
}

/// The probe of the primary path: one note to `mailbox`, never through the fallback.
pub fn probe(p: &Ports, cfg: &Config, mailbox: &str) -> Result<String, String> {
    let m = Message {
        mailbox: mailbox.into(),
        subject: "alarms probe of the primary delivery path".into(),
        kind: "note".into(),
        class: None,
        default: String::new(),
        body: "## Note\nThe alarms component's hourly probe: the primary path delivered this.\n".into(),
    };
    deliver(p, cfg, PROBE_CALLER, PROBE, &format!("probe:{mailbox}"), &m, false)
}

/// Whether the latest probe in `run_dir` delivered within `max_age_secs` of `now`.
pub fn probe_health(run_dir: &Path, now: u64, max_age_secs: u64) -> Result<(), String> {
    let all = tsd::alarm::read(run_dir);
    let Some(last) = all.iter().rev().find(|a| a.caller == PROBE_CALLER) else {
        return Err("no probe has been recorded".into());
    };
    match &last.outcome {
        Outcome::Failed { reason } => Err(format!("the last probe failed: {reason}")),
        _ if now.saturating_sub(last.at) > max_age_secs => Err(format!("the last probe is {}s old", now.saturating_sub(last.at))),
        _ => Ok(()),
    }
}

/// The `[floor] proposal` the Concierge wrote in the causal map's seed file, if any.
pub fn floor_proposal(causal_map: &str) -> Option<String> {
    let v: toml::Value = causal_map.parse().ok()?;
    v.get("floor")?.get("proposal")?.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted(Result<spira_config::lc_call::Reply, String>, std::sync::Mutex<Vec<Vec<String>>>);

    impl spira_config::lc_call::Transport for Scripted {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<spira_config::lc_call::Reply, String> {
            self.1.lock().unwrap().push(argv.to_vec());
            self.0.clone()
        }
    }

    /// sp-ipwlic: the alarms pass reaches the store through the client, with the program's
    /// answer shapes kept: JSON out, non-JSON as cannot_tell, no answer as -1.
    #[test]
    fn lc_json_carries_the_clients_answer_and_its_failures() {
        let reply = |code, out: &str| Ok(spira_config::lc_call::Reply { code, out: out.into(), err: String::new() });
        let t = std::sync::Arc::new(Scripted(reply(0, "{\"rev\":\"4\"}\n"), Default::default()));
        let c = spira_config::lc_call::LcClient::over(t.clone(), true);
        assert_eq!(lc_json(&c, &argv(&["section", "snapshot"])), (0, json!({"rev": "4"})));
        assert_eq!(*t.1.lock().unwrap(), vec![argv(&["section", "snapshot"])]);
        let bad = spira_config::lc_call::LcClient::over(std::sync::Arc::new(Scripted(reply(2, "cannot tell"), Default::default())), true);
        assert!(lc_json(&bad, &argv(&["section", "get", "x"])).1["cannot_tell"].is_string());
        let down = spira_config::lc_call::LcClient::over(std::sync::Arc::new(Scripted(Err("not answering".into()), Default::default())), true);
        let (code, v) = lc_json(&down, &argv(&["lock", "acquire"]));
        assert_eq!((code, v["cannot_tell"].as_str()), (-1, Some("not answering")));
    }

    #[test]
    fn the_floor_proposal_is_read_from_the_causal_maps_floor_table_or_is_none() {
        assert_eq!(floor_proposal("[floor]\nproposal = \" add a slot \"\n").as_deref(), Some("add a slot"));
        for none in ["", "[floor]\n", "[floor]\nproposal = \"  \"\n", "[weights]\nx = 1\n", "not toml ["] {
            assert_eq!(floor_proposal(none), None, "{none:?}");
        }
    }
}
