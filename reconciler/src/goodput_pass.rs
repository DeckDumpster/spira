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

/// The lifecycle store, in-process over its socket: every verb the pass speaks (`intent`,
/// `lock`, `pass`, `section`) is served there.
pub struct SpiraLc {
    client: spira_config::lc_call::LcClient,
}

impl SpiraLc {
    pub fn installed() -> Result<SpiraLc, String> {
        Ok(SpiraLc { client: spira_config::lc_call::LcClient::configured()? })
    }

    #[cfg(test)]
    pub fn over(client: spira_config::lc_call::LcClient) -> SpiraLc {
        SpiraLc { client }
    }
}

impl Lc for SpiraLc {
    fn call(&self, args: &[String]) -> (i32, Value) {
        match self.client.call(args) {
            Ok(r) => {
                let v = serde_json::from_str(r.out.trim()).unwrap_or_else(|_| json!({ "cannot_tell": format!("the lifecycle store answered non-JSON: {}", r.why()) }));
                (r.code, v)
            }
            Err(e) => (-1, json!({ "cannot_tell": e.to_string() })),
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
        halted: run_dir.join("world.halted").exists() || spira_config::scoped_stop::stop_everywhere_registered(&run_dir, "remedies").is_some(),
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
    let lc = SpiraLc::installed()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use spira_config::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};

    struct Store(Mutex<Vec<Vec<String>>>, i32, &'static str);

    impl Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            Ok(Reply { code: self.1, out: self.2.into(), err: String::new() })
        }
    }

    fn lc(code: i32, out: &'static str) -> (SpiraLc, Arc<Store>) {
        let s = Arc::new(Store(Mutex::default(), code, out));
        (SpiraLc::over(LcClient::over(Arc::clone(&s) as Arc<dyn Transport>, true)), s)
    }

    #[test]
    fn a_pass_verb_is_answered_by_the_store_in_process() {
        let (lc, store) = lc(0, r#"{"held":true}"#);
        let argv: Vec<String> = ["lock", "acquire", "reconciler", "--ttl", "60", "--holder", "h"].map(String::from).to_vec();
        assert_eq!(lc.call(&argv), (0, json!({ "held": true })));
        assert_eq!(store.0.lock().unwrap().as_slice(), &[argv]);
    }

    #[test]
    fn a_non_json_answer_is_cannot_tell_with_its_exit_code() {
        let (lc, _) = lc(2, "cannot tell: down");
        let (code, v) = lc.call(&["section".to_string(), "snapshot".into()]);
        assert_eq!(code, 2);
        assert!(v["cannot_tell"].as_str().unwrap().contains("cannot tell: down"), "{v}");
    }
}
