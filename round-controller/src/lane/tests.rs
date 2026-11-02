use serde_json::json;

use super::*;
use crate::outcome::Kind;
use crate::pass::pass;
use crate::resources::round_key;
use crate::sim::bench::Bench;
use crate::sim::git::GitSeed;

const IMAGE: &str = "sha256:img-1";
const HARNESS: &str = "harness-1";

/// A round repo `alpha` whose landing ref is at `base-1`, on the installed image and harness.
fn bench() -> Bench {
    let b = Bench::new();
    b.installed.set(IMAGE, HARNESS);
    b.git.seed(&GitSeed {
        repo: "alpha".into(),
        landing: "base-1".into(),
        history: vec!["base-0".into()],
    });
    b
}

fn lane_of(b: &Bench) -> Option<(serde_json::Value, state_store::Rev)> {
    b.store
        .get(&lane_key("alpha"))
        .unwrap()
        .map(|i| (i.value.data, i.mod_rev))
}

#[test]
fn a_pass_on_a_round_repo_records_its_landing_ref_and_image_digest_once() {
    let b = bench();
    pass(&b.ports(), &["alpha".into()]);
    let (data, rev) = lane_of(&b).expect("the pass recorded the lane");
    assert_eq!(
        data,
        json!({ "landing_ref": "base-1", "image_digest": IMAGE, "harness_version": HARNESS })
    );
    let lane = stack::lane(&b.ports(), "alpha").unwrap().unwrap();
    assert_eq!(
        (lane.landing_ref.as_str(), lane.image_digest.as_str()),
        ("base-1", IMAGE)
    );

    pass(&b.ports(), &["alpha".into()]);
    assert_eq!(
        lane_of(&b).unwrap().1,
        rev,
        "an unchanged lane is not written again"
    );
    assert_eq!(record(&b.ports(), "alpha"), Ok(Recorded::Unchanged));
}

#[test]
fn a_new_image_or_a_moved_landing_ref_rewrites_the_lane_keeping_selection_and_depth() {
    let b = bench();
    b.seed(
        &lane_key("alpha"),
        json!({ "landing_ref": "base-0", "image_digest": "sha256:img-0", "harness_version": HARNESS,
                "selection": "test-a.sh", "concurrency": 3 }),
    );
    pass(&b.ports(), &["alpha".into()]);
    let (data, _) = lane_of(&b).unwrap();
    assert_eq!(
        data,
        json!({ "landing_ref": "base-1", "image_digest": IMAGE, "harness_version": HARNESS,
                "selection": "test-a.sh", "concurrency": 3 })
    );

    b.installed.set("sha256:img-2", "harness-2");
    assert_eq!(record(&b.ports(), "alpha"), Ok(Recorded::Written));
    let lane = stack::lane(&b.ports(), "alpha").unwrap().unwrap();
    assert_eq!(
        (
            lane.image_digest.as_str(),
            lane.harness_version.as_str(),
            lane.depth()
        ),
        ("sha256:img-2", "harness-2", 3)
    );
}

#[test]
fn a_landing_the_lander_has_not_recorded_keeps_the_landing_ref_where_it_was() {
    let b = bench();
    b.seed(
        &lane_key("alpha"),
        json!({ "landing_ref": "base-0", "image_digest": "sha256:img-0", "harness_version": HARNESS }),
    );
    b.seed(
        &round_key("alpha", "k1"),
        json!({ "batch_id": "k1", "repo": "alpha", "state": "Green", "head": "base-1", "base": "base-0",
                "pass": 1, "version": 3, "tested_base": "base-0",
                "members": [{ "id": "sp-k1", "tip": "t1" }] }),
    );
    assert_eq!(record(&b.ports(), "alpha"), Ok(Recorded::Written));
    let lane = stack::lane(&b.ports(), "alpha").unwrap().unwrap();
    assert_eq!(
        (lane.landing_ref.as_str(), lane.image_digest.as_str()),
        ("base-0", IMAGE),
        "the round stays grounded for the Lander to record"
    );
}

#[test]
fn a_repo_that_lands_without_rounds_gets_no_lane() {
    let b = Bench::new();
    b.git.seed(&GitSeed {
        repo: "alpha".into(),
        landing: "base-1".into(),
        history: vec![],
    });
    pass(&b.ports(), &["alpha".into()]);
    assert_eq!(lane_of(&b), None);
    assert_eq!(record(&b.ports(), "alpha"), Ok(Recorded::NotRounds));
}

#[test]
fn an_input_or_a_lane_that_cannot_be_read_is_refused_naming_it_and_told_once() {
    let b = bench();
    b.installed.refuse("round-vm recipe-sha exited Some(2)");
    pass(&b.ports(), &["alpha".into()]);
    pass(&b.ports(), &["alpha".into()]);
    assert_eq!(lane_of(&b), None);
    let declined: Vec<String> = b
        .log
        .outcomes()
        .into_iter()
        .filter(|o| o.kind == Kind::Declined && o.line().contains(INSTALLED_KEY))
        .map(|o| o.line())
        .collect();
    assert_eq!(declined.len(), 1, "{declined:?}");

    let b = bench();
    assert!(
        matches!(record(&b.ports(), "ghost"), Err(Refusal::Unreadable { key, .. }) if key == git_key("ghost"))
    );
    b.seed(&lane_key("alpha"), json!(["not", "a", "lane"]));
    assert!(
        matches!(record(&b.ports(), "alpha"), Err(Refusal::Unreadable { key, text }) if key == lane_key("alpha") && text.contains("not an object"))
    );

    let b = bench();
    b.seed(&lane_key("alpha"), json!({ "landing_ref": 7 }));
    assert!(
        matches!(record(&b.ports(), "alpha"), Err(Refusal::Unreadable { key, .. }) if key == lane_key("alpha"))
    );
}

#[test]
fn a_lane_another_writer_keeps_moving_is_refused_after_three_tries() {
    let b = bench();
    b.racing_writes(&lane_key("alpha"));
    assert!(
        matches!(record(&b.ports(), "alpha"), Err(Refusal::Unreadable { text, .. }) if text.contains("3 tries"))
    );
}
