//! The seams whose transport moved in-process: the store through `LcClient`, the forge through
//! its library, the CONTEXT settings and the czar fence from the config, unit state from the
//! `Units` port. Each double records what crossed the seam.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use spira_store::lc_call::{LcClient, Reply, Transport};

use crate::ports::{Lc, Rerun, RoundLease};
use crate::real::{self, RealLc};

/// Answers each call from a script, oldest first, and records argv and stdin.
#[derive(Default)]
struct Scripted {
    answers: Mutex<Vec<Result<Reply, String>>>,
    seen: Mutex<Vec<(Vec<String>, Option<String>)>>,
}

impl Transport for Scripted {
    fn exchange(
        &self,
        argv: &[String],
        stdin: Option<&str>,
        _d: Duration,
    ) -> Result<Reply, String> {
        self.seen
            .lock()
            .unwrap()
            .push((argv.to_vec(), stdin.map(str::to_string)));
        let mut a = self.answers.lock().unwrap();
        if a.is_empty() {
            return Err("cannot tell: nothing scripted".into());
        }
        a.remove(0)
    }
}

fn reply(code: i32, out: &str) -> Result<Reply, String> {
    Ok(Reply {
        code,
        out: out.into(),
        err: String::new(),
    })
}

fn lc(answers: Vec<Result<Reply, String>>) -> (RealLc, Arc<Scripted>) {
    let t = Arc::new(Scripted {
        answers: Mutex::new(answers),
        ..Default::default()
    });
    (
        RealLc {
            bin: None,
            client: Some(LcClient::over(t.clone(), true)),
        },
        t,
    )
}

#[test]
fn a_store_read_goes_through_the_door_with_no_program_and_no_stdin() {
    let (lc, t) = lc(vec![reply(0, "[\"sp-a\",\"sp-b\"]")]);
    let _ = lc.express_ids();
    let seen = t.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].0[..2],
        ["list".to_string(), "--express".to_string()]
    );
    assert_eq!(seen[0].1, None);
}

#[test]
fn a_door_read_carries_its_body_on_argv_because_the_socket_takes_no_stdin() {
    let (lc, t) = lc(vec![reply(0, "{\"ok\":{\"value\":null}}")]);
    let _ = crate::batches::Door::get(&lc, "batches/r/b1");
    let seen = t.seen.lock().unwrap();
    assert_eq!(
        seen[0].0[..3],
        ["rc".to_string(), "get".to_string(), "--json".to_string()]
    );
    assert_eq!(seen[0].0[3], "{\"key\":\"batches/r/b1\"}");
    assert_eq!(seen[0].1, None, "no body on stdin");
}

#[test]
fn a_failed_read_is_asked_twice_and_says_why() {
    let (lc, t) = lc(vec![
        reply(2, "cannot tell: down"),
        Err("cannot tell: lost".into()),
    ]);
    let err = lc.express_ids().unwrap_err();
    assert_eq!(t.seen.lock().unwrap().len(), 2);
    assert!(err.contains("cannot tell: lost"), "{err}");
}

#[test]
fn the_round_lease_reads_held_absent_and_unanswered_apart() {
    let (lc, _) = lc(vec![
        reply(
            0,
            "{\"token\":\"7.2\",\"holder\":\"q\",\"ttl_left_secs\":30}",
        ),
        reply(1, ""),
        reply(2, "cannot tell"),
    ]);
    assert_eq!(
        lc.round_lease("r", "b").unwrap(),
        Some(RoundLease {
            holder: Some("q".into()),
            token: "7-2".into(),
            ttl_left_secs: 30
        })
    );
    assert_eq!(lc.round_lease("r", "b").unwrap(), None);
    assert!(lc.round_lease("r", "b").is_err());
}

#[test]
fn the_context_reads_every_key_from_the_config_and_a_repos_own_ci_limits() {
    let values = |k: &str| -> Option<String> {
        match k {
            "SPIRA_QUEUE_CI_MAXSEC_MY_REPO" => Some("99".into()),
            "SPIRA_QUEUE_CI_IDLE_SEC_MY_REPO" => Some(String::new()),
            k if k.starts_with("SPIRA_") => Some(format!("v:{k}")),
            _ => None,
        }
    };
    let kv = real::context_kv(std::path::Path::new("/h"), "my-repo", &values).unwrap();
    assert_eq!(kv["home"], "/h");
    assert_eq!(kv["run"], "v:SPIRA_RUN");
    assert_eq!(kv["ci_maxsec"], "99");
    assert_eq!(
        kv["ci_idle"], "v:SPIRA_QUEUE_CI_IDLE_SEC",
        "an empty own value is the repo declaring none"
    );
    assert_eq!(kv["mender_label"], "v:SPIRA_MENDER_LABEL");
    assert_eq!(kv.len(), 22);
}

#[test]
fn a_context_key_the_config_does_not_declare_is_refused_by_name() {
    let values = |k: &str| (k != "SPIRA_QUEUE_LOCK_WAIT").then(|| "1".to_string());
    let err = real::context_kv(std::path::Path::new("/h"), "r", &values).unwrap_err();
    assert!(err.contains("SPIRA_QUEUE_LOCK_WAIT"), "{err}");
}

#[test]
fn the_toml_path_is_the_spec_only_when_every_layer_exists() {
    let d = crate::testutil::tmpdir("toml-path");
    let (a, b) = (d.path().join("a.toml"), d.path().join("b.toml"));
    std::fs::write(&a, "").unwrap();
    let both = format!("{}:{}", a.display(), b.display());
    assert_eq!(
        real::toml_path_of(Some(&a.display().to_string())),
        Some(a.clone())
    );
    assert_eq!(real::toml_path_of(Some(&both)), None);
    assert_eq!(real::toml_path_of(Some("")), None);
    assert_eq!(real::toml_path_of(None), None);
}

#[test]
fn the_czar_fence_passes_only_an_act_stage_and_shadow_is_the_unset_stage() {
    let env = |k: &str| match k {
        "SPIRA_CZAR_STAGE_LOOP_STALLED" => Some("act".to_string()),
        "SPIRA_CZAR_STAGE_ODD" => Some("maybe".to_string()),
        _ => None,
    };
    assert_eq!(real::czar_stage("loop-stalled", &env), Ok(()));
    let shadow = real::czar_stage("deadlock", &env).unwrap_err();
    assert!(
        shadow.contains("deadlock is shadow") && shadow.contains("SPIRA_CZAR_STAGE_DEADLOCK=act"),
        "{shadow}"
    );
    assert!(real::czar_stage("odd", &env)
        .unwrap_err()
        .contains("unknown stage maybe"));
    assert!(real::czar_stage("", &env)
        .unwrap_err()
        .contains("class required"));
}

#[test]
fn unit_facts_read_as_the_active_state_word() {
    use runtime::units::{UnitFacts, UnitState};
    let f = |state| {
        Some(UnitFacts {
            state,
            main_pid: None,
            exit: None,
            since_ms: None,
        })
    };
    assert_eq!(real::active_state_of(f(UnitState::Active)), "active");
    assert_eq!(real::active_state_of(f(UnitState::Absent)), "inactive");
    assert_eq!(real::active_state_of(f(UnitState::Failed)), "failed");
    assert_eq!(real::active_state_of(None), "");
}

#[test]
fn the_forges_answers_map_as_its_childs_did() {
    use forge::client::Reply as F;
    let r = |code, out: &str, err: &str| F {
        code,
        out: out.into(),
        err: err.into(),
    };
    assert_eq!(
        real::forge_refusal(&r(1, "", ""), false),
        "rc=1: (no stderr)"
    );
    assert_eq!(
        real::forge_refusal(&r(1, "", "refusing x\n"), false),
        "refusing x"
    );
    assert_eq!(
        real::forge_refusal(&r(1, "", "refusing x\n"), true),
        "rc=1: refusing x"
    );
    assert_eq!(real::rerun_of(&r(0, "", "")), Rerun::Started);
    assert_eq!(
        real::rerun_of(&r(3, "", "forge: workflow-rerun refused: same tree\n")),
        Rerun::Decided("same tree".into())
    );
    assert_eq!(real::rerun_of(&r(1, "", "")), Rerun::Failed);
    assert_eq!(
        real::prs_on(
            "12 spira/queue/a\n13 other\n14 spira/publish/b\n",
            "spira/queue/"
        ),
        vec![("12".to_string(), "spira/queue/a".to_string())]
    );
}
