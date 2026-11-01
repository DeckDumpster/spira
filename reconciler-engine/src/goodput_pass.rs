//! One goodput pass: take the reconciler's lock, assemble, rank, read every intent back, select,
//! write, record, release. Every write carries the lock's token, so a pass whose lock expired
//! under it is refused by the store rather than trusted to notice. The pass reaches the store
//! only through [`Lc`], the `spira-lc` verbs, and reads time only from the injected clock.

use runtime::ports::Clock;
use serde_json::Value;
use spira_desired_state::intent::{JudgementResult, RECONCILER, RECONCILER_LOCK};
use spira_desired_state::section::{self, Freshness, Section};
use spira_desired_state::sections::{self, lifecycle};
use spira_desired_state::store::format_rfc3339;

use crate::assemble::{assemble, Assembled, Snapshot};
use crate::pass_record::{Gap, Goodput, Judged, Mode, PassRecord, Refused, Step};
use crate::select::{select, Inputs, Plan, Planned, RankedGap};

/// One `spira-lc` call: the answer's exit code and its JSON.
pub trait Lc {
    fn call(&self, args: &[String]) -> (i32, Value);
}

pub struct PassConfig {
    /// This pass's identity on the lock: the unit's invocation id, or a fresh one by hand.
    pub holder: String,
    /// The lock's TTL and the pass's own deadline.
    pub timeout_secs: u64,
    pub shadow: Vec<String>,
    pub halted: bool,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The lock was held: a standby record names the holder, and nothing else was written.
    Standby { held_by: Value },
    Passed(Box<Passed>),
}

#[derive(Debug, PartialEq)]
pub struct Passed {
    pub record: PassRecord,
    /// The deadline passed before every write was made; the record names what was.
    pub deadline_hit: bool,
    /// The pass record itself committed.
    pub recorded: bool,
    pub released: bool,
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn with_body(mut args: Vec<String>, body: &impl serde::Serialize) -> Vec<String> {
    args.push("--json".into());
    args.push(serde_json::to_string(body).unwrap_or_default());
    args
}

fn refusal_code(v: &Value) -> Option<&str> {
    v.pointer("/refused/code").and_then(Value::as_str)
}

fn explain(code: i32, v: &Value) -> String {
    format!("exit {code}: {v}")
}

/// The ranking the selector walks until the causal map ranks it: score is the distance, a
/// gap opens above `open` and stays open down to `close`, and a section that is stale or whose
/// distance cannot be evaluated is an open gap scored at its `open`, never zero.
pub fn interim_ranking(a: &Assembled) -> Vec<RankedGap> {
    let was_open = |r: &str| a.prev.as_ref().is_some_and(|p| p.gaps.iter().any(|g| g.section == r));
    let mut gaps = Vec::new();
    for (r, cur) in &a.current.sections {
        let Some(d) = &cur.distance else {
            if cur.freshness != Freshness::Fresh {
                gaps.push(RankedGap { section: r.clone(), distance: 0.0, score: 0.0, open: true });
            }
            continue;
        };
        if cur.freshness != Freshness::Fresh {
            gaps.push(RankedGap { section: r.clone(), distance: d.open, score: d.open, open: true });
            continue;
        }
        let kind = r.split_once('/').map_or(r.as_str(), |(k, _)| k);
        let s = Section {
            api_version: section::API_VERSION.into(),
            kind: kind.into(),
            metadata: cur.metadata.clone(),
            spec: Default::default(),
            status: cur.status.clone(),
            flow: cur.flow.clone(),
            distance: cur.distance.clone(),
            actions: cur.actions.clone(),
        };
        let value = sections::lookup(kind).and_then(|k| s.distance_value(k)).and_then(Result::ok);
        let Some(v) = value else {
            gaps.push(RankedGap { section: r.clone(), distance: d.open, score: d.open, open: true });
            continue;
        };
        let open = v > d.open || (v >= d.close && was_open(r));
        gaps.push(RankedGap { section: r.clone(), distance: v, score: v, open });
    }
    gaps.sort_by(|x, y| y.score.total_cmp(&x.score).then_with(|| x.section.cmp(&y.section)));
    gaps
}

fn goodput(a: &Assembled) -> Option<Goodput> {
    let cur = a.current.sections.get(&format!("{}/{}", lifecycle::NAME, lifecycle::SECTION))?;
    let v = |f: &str| cur.status.fields.get(f).and_then(|x| x.get("value")).and_then(Value::as_f64);
    Some(Goodput { ratio: v("goodput_ratio_24h"), rate_per_day: v("goodput_rate_24h") })
}

fn as_step(p: &Planned, intent: Option<String>) -> Step {
    Step {
        section: p.section.clone(),
        action: p.action.clone(),
        target: p.intent.target.clone(),
        from: p.intent.from.clone(),
        to: p.intent.to.clone(),
        intent,
        held: p.held.clone(),
    }
}

struct Pass<'a> {
    lc: &'a dyn Lc,
    clock: &'a dyn Clock,
    started_ms: u64,
    timeout_ms: u64,
    token: String,
    deadline_hit: bool,
    failed: Vec<Refused>,
}

impl Pass<'_> {
    /// Whether a write may still be made: once the deadline passes, none is.
    fn may_write(&mut self) -> bool {
        if self.clock.now_ms().saturating_sub(self.started_ms) >= self.timeout_ms {
            self.deadline_hit = true;
        }
        !self.deadline_hit
    }

    fn judge(&mut self, plan: &Plan, shadow: &[String], halted: bool) -> Vec<Judged> {
        let mut out = Vec::new();
        for j in &plan.judged {
            let kind = j.section.split_once('/').map_or(j.section.as_str(), |(k, _)| k);
            if halted || crate::effect::is_shadowed(shadow, kind) || !self.may_write() {
                continue;
            }
            let result = match j.result {
                JudgementResult::Fulfilled => "fulfilled",
                JudgementResult::Failed => "failed",
            };
            let observed = j.observed.to_string();
            let args = argv(&["intent", "judge", &j.key, "--token", &self.token, "--actor", RECONCILER, "--result", result, "--observed", &observed]);
            let (code, v) = self.lc.call(&args);
            if code == 0 {
                out.push(Judged { intent: j.key.clone(), result: j.result, baseline: j.baseline, observed: j.observed });
            } else {
                self.failed.push(Refused {
                    section: j.section.clone(),
                    action: "judge".into(),
                    code: refusal_code(&v).unwrap_or("judge-failed").into(),
                    reason: explain(code, &v),
                });
            }
        }
        out
    }

    /// Writes the intent and reads it back; it counts only once it reads back as written.
    fn step(&mut self, p: &Planned) -> Result<Step, Refused> {
        let refused = |code: &str, reason: String| Refused { section: p.section.clone(), action: p.action.clone(), code: code.into(), reason };
        if !self.may_write() {
            return Err(refused("deadline", "the pass's deadline passed before the step was written".into()));
        }
        let put = with_body(argv(&["intent", "put", &p.key, "--token", &self.token, "--actor", RECONCILER]), &p.intent);
        let (code, v) = self.lc.call(&put);
        if code != 0 {
            let c = refusal_code(&v).unwrap_or("put-failed");
            return Err(refused(c, explain(code, &v)));
        }
        let (code, v) = self.lc.call(&argv(&["intent", "get", &p.key]));
        let back = v.pointer("/intent/id").and_then(Value::as_str) == Some(p.intent.id.as_str());
        if code != 0 || !back {
            return Err(refused("no-read-back", explain(code, &v)));
        }
        Ok(as_step(p, Some(p.key.clone())))
    }
}

/// One pass. `Err` when the lock or the snapshot cannot be read, or the pass record or the
/// release did not commit; the lock is released on every path that read its token.
pub fn run(lc: &dyn Lc, clock: &dyn Clock, cfg: &PassConfig) -> Result<Outcome, String> {
    let started_ms = clock.now_ms();
    let ttl = cfg.timeout_secs.to_string();
    let (code, v) = lc.call(&argv(&["lock", "acquire", RECONCILER_LOCK, "--ttl", &ttl, "--holder", &cfg.holder]));
    if code != 0 {
        if refusal_code(&v) != Some("locked") {
            return Err(format!("lock acquire: {}", explain(code, &v)));
        }
        let (code, s) = lc.call(&argv(&["pass", "standby", "--holder", &cfg.holder]));
        if code != 0 {
            return Err(format!("pass standby: {}", explain(code, &s)));
        }
        return Ok(Outcome::Standby { held_by: s.get("held_by").cloned().unwrap_or(Value::Null) });
    }
    let token = v.get("token").and_then(Value::as_str).ok_or("lock acquire answered no token")?.to_string();
    let mut pass = Pass { lc, clock, started_ms, timeout_ms: cfg.timeout_secs.saturating_mul(1000), token, deadline_hit: false, failed: Vec::new() };
    let result = pass_under_lock(&mut pass, clock, cfg);
    let release = argv(&["lock", "release", RECONCILER_LOCK, "--token", &pass.token, "--holder", &cfg.holder]);
    let released = lc.call(&release).0 == 0;
    let mut p = result?;
    p.released = released;
    p.deadline_hit = pass.deadline_hit;
    if !released || (!p.recorded && !p.deadline_hit) {
        return Err(format!("pass {}: recorded={} released={released}", p.record.n, p.recorded));
    }
    Ok(Outcome::Passed(Box::new(p)))
}

fn pass_under_lock(pass: &mut Pass, clock: &dyn Clock, cfg: &PassConfig) -> Result<Passed, String> {
    let (code, v) = pass.lc.call(&argv(&["section", "snapshot"]));
    if code != 0 {
        return Err(format!("section snapshot: {}", explain(code, &v)));
    }
    let now_ms = clock.now_ms();
    let a = assemble(&Snapshot::parse(&v)?, now_ms);
    let gaps = interim_ranking(&a);
    let plan = select(&Inputs { assembled: &a, gaps: &gaps, shadow: &cfg.shadow, halted: cfg.halted, now_ms, token: &pass.token });

    let judged = pass.judge(&plan, &cfg.shadow, cfg.halted);
    let mut refused = plan.refused.clone();
    refused.append(&mut pass.failed);
    let mut steps = Vec::new();
    for p in &plan.steps {
        match pass.step(p) {
            Ok(s) => steps.push(s),
            Err(r) => refused.push(r),
        }
    }
    let mode = if cfg.halted {
        Mode::Halted
    } else if steps.is_empty() && !plan.would_step.is_empty() {
        Mode::Shadow
    } else {
        Mode::Acting
    };
    let open: Vec<&RankedGap> = gaps.iter().filter(|g| g.open).collect();
    let record = PassRecord {
        n: a.n,
        list_rev: a.list_rev,
        at: format_rfc3339(now_ms / 1000),
        holder: cfg.holder.clone(),
        mode,
        goodput: goodput(&a),
        binding: open.first().map(|g| g.section.clone()),
        gaps: open.iter().map(|g| Gap { section: g.section.clone(), distance: g.distance, score: g.score }).collect(),
        steps,
        would_step: plan.would_step.iter().map(|p| as_step(p, None)).collect(),
        judged,
        acks_refused: plan.acks_refused.clone(),
        refused,
        exhausted: plan.exhausted.clone(),
        no_lever: plan.no_lever.clone(),
        ages: a.ages.clone(),
        shadow_kinds: cfg.shadow.clone(),
    };
    let mut recorded = false;
    if pass.may_write() {
        let args = with_body(argv(&["pass", "record", "--token", &pass.token, "--actor", RECONCILER]), &record);
        recorded = pass.lc.call(&args).0 == 0;
    }
    Ok(Passed { record, deadline_hit: false, recorded, released: false })
}
