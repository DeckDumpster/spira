//! `reconciler arm-check <kind> [--disarm]`: the shadow list that arms one section kind, with
//! every precondition's verdict and the kind's would-be steps from the retained shadow passes,
//! or with `--disarm` the reverse list. It writes nothing; arming is the operator's one
//! `spira-config set` of the list it prints. The check itself is `reconciler_engine::arm`.

use reconciler_engine::arm::{arm_check, disarmed_list, parse_history, ArmCheck};
use reconciler_engine::assemble::{assemble, Snapshot};
use reconciler_engine::goodput_pass::Lc;
use runtime::ports::{Clock, WallClock};

use crate::goodput_pass::SpiraLc;

pub const VERB: &str = "arm-check";

fn set_line(list: &[String]) -> String {
    format!("spira-config set spira.reconciler_shadow_kinds \"{}\" \"$SPIRA_TOML\"", list.join(" "))
}

fn read(lc: &dyn Lc, verb: &[&str]) -> Result<serde_json::Value, String> {
    let args: Vec<String> = verb.iter().map(|s| s.to_string()).collect();
    match lc.call(&args) {
        (0, v) => Ok(v),
        (code, v) => Err(format!("spira-lc {}: exit {code}: {v}", verb.join(" "))),
    }
}

/// The report `arm-check` prints for `c`.
pub fn render(c: &ArmCheck) -> String {
    let mut out = format!("reconciler: arm-check {}\n", c.kind);
    for k in &c.checks {
        out.push_str(&format!("  [{}] {}: {}\n", if k.holds { "ok" } else { "FAIL" }, k.name, k.detail));
    }
    out.push_str(&format!("would-be steps ({}):\n", c.would_step.len()));
    for (n, s) in &c.would_step {
        out.push_str(&format!("  pass {n}: {} {} on {}: {} -> {}\n", s.section, s.action, s.target, s.from, s.to));
    }
    let lead = if c.ready() { "arm with" } else { "once every precondition holds, arm with" };
    out.push_str(&format!("{lead}: {}\n", set_line(&c.list)));
    out
}

/// `args` are the words after the verb. `Err` names each failed precondition.
pub fn run(lc: &dyn Lc, clock: &dyn Clock, shadow: &[String], args: &[String]) -> Result<String, String> {
    let (kind, disarm) = match args {
        [k] => (k.as_str(), false),
        [k, flag] if flag == "--disarm" => (k.as_str(), true),
        _ => return Err(format!("usage: reconciler {VERB} <kind> [--disarm]")),
    };
    if disarm {
        return Ok(format!("disarm with: {}\n", set_line(&disarmed_list(shadow, kind)?)));
    }
    let snap = Snapshot::parse(&read(lc, &["section", "snapshot"])?)?;
    let history = parse_history(&read(lc, &["pass", "history"])?)?;
    let c = arm_check(kind, shadow, &assemble(&snap, clock.now_ms()), &history)?;
    let report = render(&c);
    if c.ready() {
        return Ok(report);
    }
    let failed: Vec<&str> = c.checks.iter().filter(|k| !k.holds).map(|k| k.name).collect();
    Err(format!("{report}{} is not ready to arm: {}", c.kind, failed.join(", ")))
}

pub fn main(args: &[String]) -> Result<(), String> {
    let shadow: Vec<String> = spira_config::process::cfg("SPIRA_RECONCILER_SHADOW_KINDS")?.split_whitespace().map(String::from).collect();
    let lc = SpiraLc::installed()?;
    print!("{}", run(&lc, &WallClock, &shadow, args)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use runtime::ports::VirtualClock;
    use serde_json::{json, Value};

    use super::*;

    /// Answers `section snapshot` and `pass history` from canned JSON and records each call.
    struct Canned {
        snapshot: Value,
        history: Value,
        calls: RefCell<Vec<String>>,
    }

    impl Lc for Canned {
        fn call(&self, args: &[String]) -> (i32, Value) {
            self.calls.borrow_mut().push(args.join(" "));
            match args.join(" ").as_str() {
                "section snapshot" => (0, self.snapshot.clone()),
                "pass history" => (0, self.history.clone()),
                _ => (2, json!({ "cannot_tell": "not a verb" })),
            }
        }
    }

    const NOW: &str = "2026-11-03T10:00:00Z";

    fn section(kind: &str, name: &str, status: Value) -> Value {
        json!({ "section": {
            "apiVersion": "spira/v1", "kind": kind,
            "metadata": { "name": name, "owner": "o", "observedAt": NOW, "target_latency": "5m", "max_age": "15m", "resourceVersion": "4" },
            "status": status,
        } })
    }

    fn canned(deadman: &str) -> Canned {
        let alarms = json!({ "observedGeneration": 0, "deadman": deadman, "deadman_state": deadman, "heartbeat_state": "unconfigured" });
        let pass = json!({ "n": 1, "list_rev": 1, "at": NOW, "holder": "inv", "mode": "shadow", "shadow_kinds": ["RoundController"] });
        Canned {
            snapshot: json!({
                "list_rev": 5, "undecodable": [], "intents": [], "knobs": {}, "pass": null,
                "sections": [section("RoundController", "lane", json!({ "observedGeneration": 0 })), section("Alarms", "all", alarms)],
            }),
            history: json!({ "passes": [pass], "list_rev": 5 }),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn clock() -> VirtualClock {
        VirtualClock::new(spira_desired_state::section::parse_rfc3339(NOW).unwrap() * 1000 + 60_000)
    }

    fn shadow() -> Vec<String> {
        ["RoundController", "Lifecycle", "Alarms"].map(String::from).to_vec()
    }

    #[test]
    fn a_ready_kind_prints_the_one_write_that_arms_it_and_writes_nothing() {
        let lc = canned("fresh");
        let out = run(&lc, &clock(), &shadow(), &["RoundController".into()]).unwrap();
        assert!(
            out.ends_with("arm with: spira-config set spira.reconciler_shadow_kinds \"Lifecycle Alarms batch-landed queue-mergeable lc-orphan-in-delivery queue-lock-age\" \"$SPIRA_TOML\"\n"),
            "{out}"
        );
        assert!(out.contains("[ok] sections-fresh") && out.contains("[ok] shadow-passes"), "{out}");
        assert_eq!(*lc.calls.borrow(), ["section snapshot", "pass history"], "reads only");
    }

    #[test]
    fn an_unmet_precondition_refuses_naming_it_with_the_report() {
        let lc = canned("waiting");
        let e = run(&lc, &clock(), &shadow(), &["RoundController".into()]).unwrap_err();
        assert!(e.contains("[FAIL] dead-man") && e.ends_with("RoundController is not ready to arm: dead-man"), "{e}");
    }

    #[test]
    fn disarm_prints_the_reverse_write_without_reading_the_store_and_bad_words_are_usage() {
        let lc = canned("fresh");
        let armed: Vec<String> = ["Lifecycle", "Alarms", "batch-landed", "queue-mergeable", "lc-orphan-in-delivery", "queue-lock-age"].map(String::from).to_vec();
        let out = run(&lc, &clock(), &armed, &["RoundController".into(), "--disarm".into()]).unwrap();
        assert_eq!(out, "disarm with: spira-config set spira.reconciler_shadow_kinds \"Lifecycle Alarms queue-mergeable RoundController\" \"$SPIRA_TOML\"\n");
        assert!(lc.calls.borrow().is_empty());
        assert!(run(&lc, &clock(), &armed, &[]).unwrap_err().starts_with("usage:"));
    }
}
