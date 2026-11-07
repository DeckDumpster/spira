//! One goodput pass: take the reconciler's lock, assemble, rank, read every intent back, select,
//! write, record, release. Every write carries the lock's token, so a pass whose lock expired
//! under it is refused by the store rather than trusted to notice. The pass reaches the store
//! only through [`Lc`], the `spira-lc` verbs, and reads time only from the injected clock.

use runtime::ports::Clock;
use serde_json::Value;
use spira_desired_state::intent::{JudgementResult, RECONCILER, RECONCILER_LOCK};
use spira_desired_state::sections::lifecycle;
use spira_desired_state::store::format_rfc3339;

use crate::assemble::{assemble, Assembled, Snapshot};
use crate::pass_record::{Gap, Goodput, Judged, Mode, PassRecord, Refused, Step};
use crate::causal_map::CausalMap;
use crate::rank::{rank, RankedGap};
use crate::select::{select, Inputs, Plan, Planned};

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
    /// The pass's deadline on the injected clock: the lock grant plus its TTL.
    deadline_at_ms: u64,
    token: String,
    deadline_hit: bool,
    failed: Vec<Refused>,
}

impl Pass<'_> {
    /// Whether a write may still be made: once the deadline passes, none is.
    fn may_write(&mut self) -> bool {
        if self.clock.now_ms() >= self.deadline_at_ms {
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
/// release did not commit; the lock is released on every path that read its token. It is the
/// [`legs`] run back to back in one process.
pub fn run(lc: &dyn Lc, clock: &dyn Clock, cfg: &PassConfig) -> Result<Outcome, String> {
    let started_ms = clock.now_ms();
    let token = match legs::acquire(lc, &cfg.holder, cfg.timeout_secs)? {
        legs::Lock::Granted(token) => token,
        legs::Lock::Locked => return Ok(Outcome::Standby { held_by: legs::standby(lc, &cfg.holder)? }),
    };
    let leg = legs::LegConfig { token: token.clone(), holder: cfg.holder.clone(), shadow: cfg.shadow.clone(), halted: cfg.halted, deadline_at_ms: started_ms.saturating_add(cfg.timeout_secs.saturating_mul(1000)) };
    let mut deadline_hit = false;
    let result = legs::judge(lc, clock, &leg).map(|judged| {
        let (record, hit) = legs::step(lc, clock, &leg, judged);
        deadline_hit = hit;
        record
    });
    let recorded = match &result {
        Ok(record) if clock.now_ms() < leg.deadline_at_ms => legs::record(lc, &token, record).is_ok(),
        Ok(_) => {
            deadline_hit = true;
            false
        }
        Err(_) => false,
    };
    let released = legs::release(lc, &token, &cfg.holder).is_ok();
    let record = result?;
    if !released || (!recorded && !deadline_hit) {
        return Err(format!("pass {}: recorded={recorded} released={released}", record.n));
    }
    Ok(Outcome::Passed(Box::new(Passed { record, deadline_hit, recorded, released })))
}

fn mode_of(halted: bool, steps: &[Step], would_step: &[Step]) -> Mode {
    if halted {
        Mode::Halted
    } else if steps.is_empty() && !would_step.is_empty() {
        Mode::Shadow
    } else {
        Mode::Acting
    }
}

/// The pass split at its writes, so a scheduler runs each leg as one effect and decides the
/// next on its outcome: acquire, then judge (snapshot, assemble, rank, select and the
/// judgements), then step (every put and its read-back), then record, then release. Each leg
/// reads time only from the clock it is handed and writes only with the lock's token.
pub mod legs {
    use super::*;
    use serde::{Deserialize, Serialize};

    /// What the lock answered.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Lock {
        Granted(String),
        /// Another holder has it: the pass stands by.
        Locked,
    }

    /// The fixed facts every leg of one pass carries.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct LegConfig {
        pub token: String,
        pub holder: String,
        pub shadow: Vec<String>,
        pub halted: bool,
        /// The lock grant plus its TTL; no write is made at or after it.
        pub deadline_at_ms: u64,
    }

    /// The judge leg's answer: the record so far, and the steps still to write.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub struct Judged {
        pub record: PassRecord,
        pub steps: Vec<Planned>,
    }

    /// What a pass on snapshot `v` would do, read without the lock and writing nothing: the open
    /// gaps, the refusals by `section/action/code`, and how many judgements and steps are due.
    /// A scheduler asks this to tell an idle minute from one with work.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Preview {
        pub gaps: Vec<String>,
        pub refused: Vec<String>,
        pub judgements: usize,
        pub steps: usize,
    }

    /// The `section/action/code` a refusal is told apart by.
    pub fn refusal_key(r: &Refused) -> String {
        format!("{}/{}/{}", r.section, r.action, r.code)
    }

    pub fn preview(v: &Value, now_ms: u64, shadow: &[String], halted: bool) -> Result<Preview, String> {
        let a = assemble(&Snapshot::parse(v)?, now_ms);
        let gaps = rank(&a, &CausalMap::seed()?).gaps;
        let plan = select(&Inputs { assembled: &a, gaps: &gaps, shadow, halted, now_ms, token: "" });
        Ok(Preview { gaps: gaps.iter().filter(|g| g.open).map(|g| g.section.clone()).collect(), refused: plan.refused.iter().map(refusal_key).collect(), judgements: plan.judged.len(), steps: plan.steps.len() })
    }

    /// `lock acquire` for `holder` with `ttl_secs`.
    pub fn acquire(lc: &dyn Lc, holder: &str, ttl_secs: u64) -> Result<Lock, String> {
        let ttl = ttl_secs.to_string();
        let (code, v) = lc.call(&argv(&["lock", "acquire", RECONCILER_LOCK, "--ttl", &ttl, "--holder", holder]));
        if code == 0 {
            return v.get("token").and_then(Value::as_str).map(|t| Lock::Granted(t.to_string())).ok_or_else(|| "lock acquire answered no token".to_string());
        }
        if refusal_code(&v) == Some("locked") {
            return Ok(Lock::Locked);
        }
        Err(format!("lock acquire: {}", explain(code, &v)))
    }

    /// `pass standby`: the record that names who holds the lock.
    pub fn standby(lc: &dyn Lc, holder: &str) -> Result<Value, String> {
        let (code, s) = lc.call(&argv(&["pass", "standby", "--holder", holder]));
        if code != 0 {
            return Err(format!("pass standby: {}", explain(code, &s)));
        }
        Ok(s.get("held_by").cloned().unwrap_or(Value::Null))
    }

    /// The snapshot under the lock, the plan, and the judgements written.
    pub fn judge(lc: &dyn Lc, clock: &dyn Clock, cfg: &LegConfig) -> Result<Judged, String> {
        let (code, v) = lc.call(&argv(&["section", "snapshot"]));
        if code != 0 {
            return Err(format!("section snapshot: {}", explain(code, &v)));
        }
        let now_ms = clock.now_ms();
        let a = assemble(&Snapshot::parse(&v)?, now_ms);
        let ranking = rank(&a, &CausalMap::seed()?);
        let gaps = ranking.gaps;
        let plan = select(&Inputs { assembled: &a, gaps: &gaps, shadow: &cfg.shadow, halted: cfg.halted, now_ms, token: &cfg.token });
        let mut pass = Pass { lc, clock, deadline_at_ms: cfg.deadline_at_ms, token: cfg.token.clone(), deadline_hit: false, failed: Vec::new() };
        let judged = pass.judge(&plan, &cfg.shadow, cfg.halted);
        let mut refused = plan.refused.clone();
        refused.append(&mut pass.failed);
        let open: Vec<&RankedGap> = gaps.iter().filter(|g| g.open).collect();
        let would_step: Vec<Step> = plan.would_step.iter().map(|p| as_step(p, None)).collect();
        let record = PassRecord {
            n: a.n,
            list_rev: a.list_rev,
            at: format_rfc3339(now_ms / 1000),
            holder: cfg.holder.clone(),
            mode: mode_of(cfg.halted, &[], &would_step),
            goodput: goodput(&a),
            binding: ranking.binding,
            gaps: open.iter().map(|g| Gap { section: g.section.clone(), distance: g.distance, score: g.score, path_to_goodput: g.path_to_goodput.clone() }).collect(),
            steps: Vec::new(),
            would_step,
            judged,
            acks_refused: plan.acks_refused.clone(),
            refused,
            exhausted: plan.exhausted.clone(),
            no_lever: plan.no_lever.clone(),
            ages: a.ages.clone(),
            shadow_kinds: cfg.shadow.clone(),
        };
        Ok(Judged { record, steps: plan.steps })
    }

    /// Every planned step written and read back; a step that fails is refused in the record.
    /// The second value is whether the deadline stopped a write.
    pub fn step(lc: &dyn Lc, clock: &dyn Clock, cfg: &LegConfig, judged: Judged) -> (PassRecord, bool) {
        let mut pass = Pass { lc, clock, deadline_at_ms: cfg.deadline_at_ms, token: cfg.token.clone(), deadline_hit: false, failed: Vec::new() };
        let mut record = judged.record;
        for p in &judged.steps {
            match pass.step(p) {
                Ok(s) => record.steps.push(s),
                Err(r) => record.refused.push(r),
            }
        }
        record.mode = mode_of(cfg.halted, &record.steps, &record.would_step);
        (record, pass.deadline_hit)
    }

    /// `pass record` under the token.
    pub fn record(lc: &dyn Lc, token: &str, record: &PassRecord) -> Result<(), String> {
        let (code, v) = lc.call(&with_body(argv(&["pass", "record", "--token", token, "--actor", RECONCILER]), record));
        if code == 0 {
            Ok(())
        } else {
            Err(format!("pass record: {}", explain(code, &v)))
        }
    }

    /// `lock release` of the token; attempted on every path that read it.
    pub fn release(lc: &dyn Lc, token: &str, holder: &str) -> Result<(), String> {
        let (code, v) = lc.call(&argv(&["lock", "release", RECONCILER_LOCK, "--token", token, "--holder", holder]));
        if code == 0 {
            Ok(())
        } else {
            Err(format!("lock release: {}", explain(code, &v)))
        }
    }
}

/// [`Lc`] over the store's socket client: every verb the pass speaks (`intent`, `lock`, `pass`,
/// `section`) is served there. A non-JSON answer is a cannot-tell value, never a guess.
pub struct Socket(pub spira_config::lc_call::LcClient);

impl Lc for Socket {
    fn call(&self, args: &[String]) -> (i32, Value) {
        match self.0.call(args) {
            Ok(r) => {
                let v = serde_json::from_str(r.out.trim()).unwrap_or_else(|_| serde_json::json!({ "cannot_tell": format!("the lifecycle store answered non-JSON: {}", r.why()) }));
                (r.code, v)
            }
            Err(e) => (-1, serde_json::json!({ "cannot_tell": e.to_string() })),
        }
    }
}
