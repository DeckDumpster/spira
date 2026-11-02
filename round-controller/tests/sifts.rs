//! `round-controller sifts` on a recorded pool: every verdict the screen reaches is written to
//! the lifecycle store, the bead and the Concierge's inbox.

mod sifts_world;

use round_controller::sifts::sifts;
use sifts_world::World;

fn run(
    env: &round_controller::sifts::Env,
    reg: &spira_config::repos::Registry,
) -> (Result<(), String>, String) {
    let mut out = Vec::new();
    let r = sifts(env, reg, &mut out);
    (r, String::from_utf8(out).unwrap())
}

fn has(lines: &[String], want: &str) -> bool {
    lines.iter().any(|l| l.contains(want))
}

#[test]
fn every_pool_bead_takes_its_verdict_and_an_unscreenable_repo_is_named() {
    let dir = testkit::TempDir::new("rc-sifts");
    let w = World::build(&dir);
    let (r, out) = run(&w.env(), &w.registry());
    assert_eq!(r, Err("sift failed for: broken".to_string()));
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines
            .contains(&"round-controller spira: sift: 3 sifted, 4 sent back, 1 superseded, 1 held"),
        "{out}"
    );
    assert!(
        lines
            .contains(&"round-controller empty: sift: 0 sifted, 0 sent back, 0 superseded, 0 held"),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "round-controller: sift: sp-gone is DROPPED, not screened at {}; no pass recorded",
            w.tips["sp-gone"]
        )),
        "{out}"
    );
    assert!(
        !out.contains("other:"),
        "a repo outside the queue is not screened: {out}"
    );

    let v = w.verdicts();
    let t = &w.tips;
    let sifted = |id: &str| {
        format!(
            r#"spira-lc event bead {id} --expect SUBMITTED --version 4 --actor sift --kind {{"Sifted":{{"tip":"{}"}}}}"#,
            t[id]
        )
    };
    assert!(has(&v, &sifted("sp-clean")), "{v:#?}");
    for id in [
        "sp-seen",
        "sp-ejected",
        "sp-gone",
        "sp-moved",
        "sp-held",
        "sp-onheld",
        "sp-nocontent",
    ] {
        assert!(
            !v.iter()
                .any(|l| l.starts_with("effects.log: ") && l.contains(&format!(" {id} "))),
            "{id} takes no write: {v:#?}"
        );
    }
    let red = |id: &str, state: &str, reason: &str| {
        format!(
            r#"spira-lc event bead {id} --expect {state} --version 4 --actor sift --kind {{"GateRed":{{"reason":"{reason}","tip":"{}"}}}}"#,
            t[id]
        )
    };
    assert!(
        has(&v, &red("sp-conflict", "CERTIFIED", "no-rebase")),
        "{v:#?}"
    );
    assert!(
        has(&v, &red("sp-stacked", "SUBMITTED", "suites-failed")),
        "{v:#?}"
    );
    assert!(
        has(&v, &red("sp-ident", "SUBMITTED", "policy-violation")),
        "{v:#?}"
    );
    assert!(
        has(&v, &red("sp-target", "SUBMITTED", "policy-violation")),
        "{v:#?}"
    );
    assert!(
        has(
            &v,
            "bead.sh amend sp-conflict --note Sift (pre-round screen) sent this back"
        ),
        "{v:#?}"
    );
    assert!(has(&v, "spira-lc close sp-dupe --reason-file - --actor sift --superseded-by sp-inround <<< Sift: identical patch (patch-id) to sp-inround."), "{v:#?}");
    assert!(
        has(
            &v,
            "[watch:sift] SIFT superseded sp-dupe (identical patch to sp-inround)"
        ),
        "{v:#?}"
    );
    assert!(
        has(
            &v,
            "[watch:sift] SIFT REPEAT sp-capped: red after 3 send-backs"
        ),
        "{v:#?}"
    );
    assert!(
        has(&v, "[watch:sift] SIFT sent sp-ident to REWORK:"),
        "{v:#?}"
    );
    assert!(
        has(&v, "run/sift/events/sp-stacked.json:"),
        "a counted send-back is recorded: {v:#?}"
    );
    assert!(
        !has(&v, "run/sift/events/sp-conflict.json:"),
        "a conflict is never counted: {v:#?}"
    );
}

#[test]
fn a_base_that_cannot_be_fetched_screens_nothing() {
    let dir = testkit::TempDir::new("rc-sifts-fetch");
    let w = World::build(&dir);
    testkit::git(
        &w.repo,
        &[
            "remote",
            "add",
            "origin",
            &dir.join("gone").display().to_string(),
        ],
    );
    testkit::git(
        &w.repo,
        &["update-ref", "refs/remotes/origin/main", "local/main"],
    );
    let map = format!("spira|{}|queue|origin/main||\n", w.repo.display());
    let reg = spira_config::repos::Registry::new(
        Some(&map),
        &[("SPIRA_HOME_REPO".to_string(), "spira".to_string())]
            .into_iter()
            .collect(),
        &w.home,
    );
    let (r, out) = run(&w.env(), &reg);
    assert_eq!(r, Err("sift failed for: spira".to_string()));
    assert!(out.is_empty(), "{out}");
    assert!(
        w.verdicts()
            .iter()
            .all(|l| l.starts_with("run/sift/send-backs/")),
        "{:#?}",
        w.verdicts()
    );
}

#[test]
fn a_verdict_the_concierge_cannot_be_told_is_still_recorded() {
    let dir = testkit::TempDir::new("rc-sifts-inbox");
    let w = World::build(&dir);
    let mut env = w.env();
    env.inbox = Err("SPIRA_CONCIERGE_INBOX is not a registered config key".into());
    let (_, out) = run(&env, &w.registry());
    assert!(
        out.contains("round-controller: SIFT superseded sp-dupe"),
        "{out}"
    );
    let v = w.verdicts();
    assert!(!v.iter().any(|l| l.starts_with("inbox.log:")), "{v:#?}");
    assert!(has(&v, "spira-lc close sp-dupe"), "{v:#?}");
}
