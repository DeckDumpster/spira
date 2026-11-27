//! The seams whose transport moved in-process: the store through `LcClient`, the forge through
//! its library, the CONTEXT settings and the czar fence from the config, unit state from the
//! `Units` port. Each double records what crossed the seam.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use spira_store::lc_call::{LcClient, Reply, Transport};

use crate::ports::{Lc, Lib, Rerun, RoundLease};
use crate::real::{self, RealLc, RealLib};

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
        err: String::new(), read_ended: false
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

#[test_id(t4m32zs)]
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

#[test_id(tagllto)]
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

#[test_id(tl9iuoq)]
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

#[test_id(tinzvix)]
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

#[test_id(t90yapv)]
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
    let kv = real::context_kv(std::path::Path::new("h"), "my-repo", &values).unwrap();
    assert_eq!(kv["home"], "h");
    assert_eq!(kv["run"], "v:SPIRA_RUN");
    assert_eq!(kv["ci_maxsec"], "99");
    assert_eq!(
        kv["ci_idle"], "v:SPIRA_QUEUE_CI_IDLE_SEC",
        "an empty own value is the repo declaring none"
    );
    assert_eq!(kv["mender_label"], "v:SPIRA_MENDER_LABEL");
    assert_eq!(kv.len(), 22);
}

#[test_id(thu2a8e)]
#[test]
fn a_context_key_the_config_does_not_declare_is_refused_by_name() {
    let values = |k: &str| (k != "SPIRA_QUEUE_LOCK_WAIT").then(|| "1".to_string());
    let err = real::context_kv(std::path::Path::new("h"), "r", &values).unwrap_err();
    assert!(err.contains("SPIRA_QUEUE_LOCK_WAIT"), "{err}");
}

#[test_id(t906ejw)]
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

// covers: UC-safety-fences-31
#[test_id(tw5b7ir)]
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

#[test_id(tpoo6yc)]
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

#[test_id(tzybqbr)]
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

type Logged = Arc<Mutex<Vec<String>>>;

fn lib(answers: Vec<Result<Reply, String>>) -> (RealLib, Arc<Scripted>, Logged) {
    let (store, t) = lc(answers);
    let logged = Logged::default();
    let sink = logged.clone();
    let lib = RealLib {
        home: "h".into(),
        store,
        warn: Arc::new(move |m: &str| sink.lock().unwrap().push(m.to_string())),
    };
    (lib, t, logged)
}

fn sent(t: &Scripted) -> Vec<(Vec<String>, Option<String>)> {
    t.seen.lock().unwrap().clone()
}

fn call(argv: &[&str], body: Option<&str>) -> (Vec<String>, Option<String>) {
    (
        argv.iter().map(|a| a.to_string()).collect(),
        body.map(str::to_string),
    )
}

fn row(state: &str) -> Result<Reply, String> {
    reply(0, &format!("{{\"bead\":{{\"bead_id\":\"sp-a\",\"state\":\"{state}\"}}}}"))
}

#[test_id(tjidt8f)]
#[test]
fn a_comment_is_added_through_the_content_door() {
    let (lib, t, logged) = lib(vec![reply(0, "")]);
    lib.comment("sp-a", "ejected: $(x)\nsecond line");
    assert_eq!(
        sent(&t),
        [call(&["content", "comments", "add", "sp-a", "ejected: $(x)\nsecond line"], None)]
    );
    assert!(logged.lock().unwrap().is_empty());
}

#[test_id(tfaoc8u)]
#[test]
fn a_refused_comment_logs_the_reason() {
    let (lib, _, logged) = lib(vec![reply(3, "no such bead sp-a")]);
    lib.comment("sp-a", "x");
    let logged = logged.lock().unwrap();
    assert!(logged[0].contains("sp-a") && logged[0].contains("no such bead"), "{logged:?}");
}

#[test_id(tndl7xe)]
#[test]
fn a_bug_is_filed_with_its_body_on_stdin_and_the_actor_on_the_first_line() {
    let (lib, t, logged) = lib(vec![reply(0, "sp-new1\n")]);
    let id = lib.create_bug("queue.sh", "publish PR 7 red", "1", "spira,plan,repo:spira", "## Build\nfix it\n");
    assert_eq!(id.as_deref(), Some("sp-new1"));
    assert_eq!(
        sent(&t),
        [call(
            &["content", "create", "publish PR 7 red", "--type", "bug", "--priority", "1", "--labels", "spira,plan,repo:spira", "--body-file", "-", "--silent"],
            Some("Filed by queue.sh.\n\n## Build\nfix it\n")
        )]
    );
    assert!(logged.lock().unwrap().is_empty());
}

#[test_id(to3apze)]
#[test]
fn a_refused_create_files_nothing_and_logs_the_reason() {
    let (lib, _, logged) = lib(vec![reply(1, "bead: --repo <name> required")]);
    assert_eq!(lib.create_bug("q", "t", "1", "spira", "b"), None);
    let logged = logged.lock().unwrap();
    assert!(logged[0].contains("bead: --repo <name> required"), "{logged:?}");
}

#[test_id(txk0lmn)]
#[test]
fn a_create_that_names_no_id_files_nothing_and_says_so() {
    let (lib, _, logged) = lib(vec![reply(0, "\n")]);
    assert_eq!(lib.create_bug("q", "t", "1", "spira", "b"), None);
    assert_eq!(logged.lock().unwrap().len(), 1);
}

#[test_id(tlyabms)]
#[test]
fn a_live_bead_is_noted_with_the_body_on_stdin() {
    let (lib, t, _) = lib(vec![row("REWORK"), reply(0, "")]);
    assert!(lib.amend_bug("queue.sh", "sp-a", "red again"));
    assert_eq!(
        sent(&t),
        [
            call(&["show", "sp-a"], None),
            call(&["content", "note", "sp-a", "--stdin"], Some("Noted by queue.sh.\n\nred again")),
        ]
    );
}

#[test_id(t7nrvil)]
#[test]
fn a_refused_note_is_not_an_amendment_and_logs_the_reason() {
    let (lib, _, logged) = lib(vec![row("READY"), reply(3, "note refused here")]);
    assert!(!lib.amend_bug("q", "sp-a", "n"));
    let logged = logged.lock().unwrap();
    assert!(logged[0].contains("note refused here"), "{logged:?}");
}

#[test_id(tx0tyhg)]
#[test]
fn a_terminal_bead_is_neither_amended_nor_live() {
    for state in ["LANDED", "SUPERSEDED", "DROPPED", "DONE"] {
        let (lib, t, _) = lib(vec![row(state), row(state)]);
        assert!(!lib.amend_bug("q", "sp-a", "n"), "{state}");
        assert!(!lib.bead_live("sp-a"), "{state}");
        assert_eq!(sent(&t), [call(&["show", "sp-a"], None), call(&["show", "sp-a"], None)], "{state}");
    }
}

#[test_id(tjdsrko)]
#[test]
fn an_unreadable_row_is_neither_amended_nor_live() {
    for answer in [
        reply(1, ""),
        reply(2, "cannot tell: down"),
        reply(0, "not json"),
        reply(0, "{\"bead\":null}"),
        row(""),
        Err("cannot tell: lost".to_string()),
    ] {
        let (lib, t, _) = lib(vec![answer.clone(), answer]);
        assert!(!lib.amend_bug("q", "sp-a", "n"));
        assert!(!lib.bead_live("sp-a"));
        assert!(sent(&t).iter().all(|c| c.0[0] == "show"));
    }
}

#[test_id(toma1x4)]
#[test]
fn a_bead_with_a_fix_in_flight_is_live() {
    let (lib, _, _) = lib(vec![row("WORKING")]);
    assert!(lib.bead_live("sp-a"));
}
