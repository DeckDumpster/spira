//! `reconciler goodput-pass`: one pass of the goodput loop, as its own oneshot on the
//! observability plane, so a world stop never silences it; under a halt it takes no step and
//! says why. The pass itself is `reconciler_engine::goodput_pass`; this is its process: config,
//! the lock holder's identity, the `spira-lc` verbs and the metrics.

use std::collections::BTreeMap;

use reconciler_engine::goodput_pass::{run, Lc, Outcome, PassConfig};
use runtime::ports::WallClock;
use serde_json::{json, Value};
use spira_otel::metrics::{MetricsSink, Point};

pub const VERB: &str = "goodput-pass";

/// `spira-lc` as a program on the launcher's PATH.
struct SpiraLc {
    program: String,
}

impl Lc for SpiraLc {
    fn call(&self, args: &[String]) -> (i32, Value) {
        let out = spira_config::bounded::bounded(&self.program).args(args).stdin(std::process::Stdio::null()).output();
        match out {
            Ok(o) => {
                let code = o.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&o.stdout);
                let v = serde_json::from_str(stdout.trim())
                    .unwrap_or_else(|_| json!({ "cannot_tell": format!("{} answered non-JSON: {}", self.program, String::from_utf8_lossy(&o.stderr).trim()) }));
                (code, v)
            }
            Err(e) => (-1, json!({ "cannot_tell": format!("{}: {e}", self.program) })),
        }
    }
}

/// The unit's invocation id; a hand run gets a fresh one, finds the lock held by any live
/// pass, and stands by.
fn holder() -> String {
    std::env::var("INVOCATION_ID").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        format!("hand-{}-{nanos}", std::process::id())
    })
}

fn config() -> Result<PassConfig, String> {
    use spira_config::process::{cfg, cfg_parse};
    let run_dir = spira_config::resolve::run_dir_for_process()?;
    Ok(PassConfig {
        holder: holder(),
        timeout_secs: cfg_parse::<u64>("SPIRA_RECONCILER_PASS_TIMEOUT_SECS")?,
        shadow: cfg("SPIRA_RECONCILER_SHADOW_KINDS")?.split_whitespace().map(String::from).collect(),
        halted: run_dir.join("world.halted").exists(),
    })
}

fn push(sink: &MetricsSink, outcome: &Outcome) {
    let Outcome::Passed(p) = outcome else {
        sink.push(&[Point::Count { name: "spira_goodput_pass_standby", value: 1 }], &[]);
        return;
    };
    let r = &p.record;
    let count = |name, n: usize| Point::Count { name, value: n as u64 };
    sink.push(
        &[count("spira_goodput_pass_steps", r.steps.len()), count("spira_goodput_pass_would_steps", r.would_step.len()), count("spira_goodput_pass_judged", r.judged.len())],
        &[],
    );
    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    for x in &r.refused {
        *by_code.entry(x.code.as_str()).or_default() += 1;
    }
    for (code, n) in by_code {
        sink.push(&[count("spira_goodput_pass_refusals", n)], &[("reason", code)]);
    }
    for s in &r.exhausted {
        sink.push(&[count("spira_goodput_pass_exhausted", 1)], &[("section", s)]);
    }
    for l in &r.no_lever {
        sink.push(&[count("spira_goodput_pass_no_lever", 1)], &[("section", &l.section)]);
    }
}

pub fn main(sink: &MetricsSink) -> Result<(), String> {
    let cfg = config()?;
    let lc = SpiraLc { program: spira_config::lifecycle_row::lc_bin()? };
    let outcome = run(&lc, &WallClock, &cfg)?;
    push(sink, &outcome);
    match &outcome {
        Outcome::Standby { held_by } => println!("reconciler: goodput-pass: standby, the lock is held by {held_by}"),
        Outcome::Passed(p) => println!(
            "reconciler: goodput-pass {}: {:?}, {} step(s), {} would-step(s), {} refused, recorded={} released={} deadline={}",
            p.record.n,
            p.record.mode,
            p.record.steps.len(),
            p.record.would_step.len(),
            p.record.refused.len(),
            p.recorded,
            p.released,
            p.deadline_hit
        ),
    }
    Ok(())
}
