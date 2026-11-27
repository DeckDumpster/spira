//! What a trial publishes: its result on `gate/<repo>/<bead>`, and `test/<id>/red` for each
//! test it saw red. A publish that fails is logged and changes nothing about the trial.

use crate::compose::{self, Changed, Class, Member};
use crate::stopped::Stopped;
use crate::telemetry::GateRun;
use serde_json::{json, Value};
use spira_pub::{PubError, Publisher};

/// Whether `reason` says a cap ended the trial.
pub fn is_cap_reason(reason: &str) -> bool {
    matches!(reason, "phase-cap" | "budget" | "deadline" | "timeout")
}

/// A test the branch's trial saw red.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedTest {
    pub id: String,
    /// `None` when the output or the workspace does not say which crate, or what changed.
    pub touches_crate: Option<bool>,
}

pub fn gate_payload(run: &GateRun, killed_by: Option<&Stopped>) -> Value {
    json!({
        "status": run.status,
        "rc": run.rc,
        "reason": run.reason,
        "over_cap": is_cap_reason(&run.reason),
        "killed_by": killed_by.map(|s| json!({"sig": s.sig, "by_pid": s.by_pid, "cmd": s.cmd, "cgroup": s.cgroup})),
    })
}

pub fn test_payload(tree: &str, touches_crate: Option<bool>, at: u64) -> Value {
    json!({"tree": tree, "touches_crate": touches_crate, "at": at})
}

/// Whether `changed` touches `package`: a file of its own, or a workspace-wide input.
pub fn touches(changed: &[Changed], members: &[Member], package: &str) -> bool {
    changed.iter().any(|c| match compose::classify(c, members) {
        Class::Crate(n) => n == package,
        Class::Workspace => true,
        _ => false,
    })
}

/// Every message of one trial. A run with no bead has no `gate/` topic.
pub fn messages(run: &GateRun, killed_by: Option<&Stopped>, tree: &str, reds: &[RedTest], at: u64) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    if !run.bead.is_empty() {
        out.push((format!("gate/{}/{}", run.repo, run.bead), gate_payload(run, killed_by)));
    }
    out.extend(reds.iter().map(|r| (format!("test/{}/red", r.id), test_payload(tree, r.touches_crate, at))));
    out
}

/// Publishes `messages` and waits for the broker to take them. A broker that cannot be
/// reached is tried once, not once per message.
pub fn send(publisher: Result<Publisher, PubError>, messages: &[(String, Value)]) {
    let publisher = match publisher {
        Ok(p) => p,
        Err(e) => return eprintln!("gate: nothing published: {e}"),
    };
    for (topic, payload) in messages {
        if matches!(publisher.publish(spira_pub::Class::State, topic, payload), Err(PubError::Unreachable { .. } | PubError::Timeout)) {
            break;
        }
    }
    // Dropping flushes: the gate leaves by `process::exit`, which drops nothing later.
    drop(publisher);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(reason: &str) -> GateRun {
        GateRun { repo: "r".into(), bead: "sp-a".into(), status: "NO_VERDICT".into(), rc: 75, reason: reason.into(), ..Default::default() }
    }

    #[test_id(t1ohs1g)]
    #[test]
    fn a_gate_run_is_over_cap_for_each_cap_reason_and_no_other() {
        for cap in ["phase-cap", "budget", "deadline", "timeout"] {
            assert_eq!(gate_payload(&run(cap), None)["over_cap"], true, "{cap}");
        }
        assert_eq!(
            gate_payload(&run("branch-red"), None),
            json!({"status": "NO_VERDICT", "rc": 75, "reason": "branch-red", "over_cap": false, "killed_by": null})
        );
    }

    #[test_id(tnm1rns)]
    #[test]
    fn killed_by_is_the_stop_record_read_back_with_an_unresolved_pid_kept_null() {
        let sent = Stopped { sig: 15, by_pid: None, cmd: "kill -TERM 5".into(), cgroup: "/u.slice/a.service".into() }; // not a path: a cgroup name
        let read = Stopped::parse(&sent.line()).expect("the record line parses");
        assert_eq!(
            gate_payload(&run("died"), Some(&read))["killed_by"],
            json!({"sig": 15, "by_pid": null, "cmd": "kill -TERM 5", "cgroup": "/u.slice/a.service"}) // not a path: a cgroup name
        );
    }

    #[test_id(tqo3fgn)]
    #[test]
    fn a_trial_with_no_bead_publishes_its_red_tests_alone() {
        let reds = [RedTest { id: "tabc123".into(), touches_crate: Some(true) }];
        let sent = messages(&GateRun { bead: String::new(), ..run("branch-red") }, None, "tr", &reds, 7);
        assert_eq!(sent, [("test/tabc123/red".to_string(), json!({"tree": "tr", "touches_crate": true, "at": 7}))]);
    }

    #[test_id(tv5bdmb)]
    #[test]
    fn a_crate_is_touched_by_its_own_files_or_a_workspace_input() {
        let members = [Member { name: "a".into(), dir: "a".into(), ..Default::default() }, Member { name: "b".into(), dir: "b".into(), ..Default::default() }];
        let changed = |p: &str| [Changed { path: p.into(), exec: false }];
        assert!(touches(&changed("a/src/lib.rs"), &members, "a"));
        assert!(!touches(&changed("a/src/lib.rs"), &members, "b"));
        assert!(touches(&changed("Cargo.lock"), &members, "b"));
        assert!(!touches(&changed("docs/x.md"), &members, "a"));
    }
}
