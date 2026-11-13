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
    b.seed(&lane_key("alpha"), json!({ "landing_ref": "base-0", "depth": 2 }));
    assert!(
        matches!(record(&b.ports(), "alpha"), Err(Refusal::Unreadable { key, text }) if key == lane_key("alpha") && text.contains("depth"))
    );
}

/// Before each of the first `wins` txns, another writer moves the lane to a lane of its own.
struct Rival<'a> {
    inner: &'a dyn crate::ports::Store,
    wins: std::sync::Mutex<usize>,
}

impl crate::ports::Store for Rival<'_> {
    fn get(&self, key: &str) -> state_store::Result<Option<state_store::Item>> {
        self.inner.get(key)
    }
    fn list(
        &self,
        prefixes: &[&str],
        sel: &state_store::Selector,
    ) -> state_store::Result<(Vec<state_store::Item>, u64)> {
        self.inner.list(prefixes, sel)
    }
    fn txn(&self, t: &Txn, req: &Request) -> state_store::Result<state_store::TxnResult> {
        let mut wins = crate::lock(&self.wins);
        if *wins > 0 {
            *wins -= 1;
            let lane = json!({ "landing_ref": "base-0", "image_digest": format!("r-{wins}"), "harness_version": "r" });
            self.inner.txn(
                &Txn::new().put(&lane_key("alpha"), Object::new(lane)),
                &Request::new(&format!("rival-{wins}"), "rival", "t"),
            )?;
        }
        self.inner.txn(t, req)
    }
    fn lock(&self, r: &str, ttl: u64, req: &Request) -> state_store::Result<state_store::Fence> {
        self.inner.lock(r, ttl, req)
    }
    fn renew(&self, f: &state_store::Fence, req: &Request) -> state_store::Result<()> {
        self.inner.renew(f, req)
    }
    fn release(&self, f: &state_store::Fence, req: &Request) -> state_store::Result<()> {
        self.inner.release(f, req)
    }
}

#[test]
fn a_lane_another_writer_moved_is_re_read_and_one_that_keeps_moving_is_refused() {
    let b = bench();
    let once = Rival {
        inner: &b,
        wins: std::sync::Mutex::new(1),
    };
    let p = Ports {
        store: &once,
        ..b.ports()
    };
    assert_eq!(record(&p, "alpha"), Ok(Recorded::Written));
    let lane = stack::lane(&b.ports(), "alpha").unwrap().unwrap();
    assert_eq!((lane.landing_ref.as_str(), lane.image_digest.as_str()), ("base-1", IMAGE));

    let b = bench();
    let always = Rival {
        inner: &b,
        wins: std::sync::Mutex::new(usize::MAX),
    };
    let p = Ports {
        store: &always,
        ..b.ports()
    };
    assert!(
        matches!(record(&p, "alpha"), Err(Refusal::Unreadable { text, .. }) if text.contains("3 tries"))
    );
}

#[test]
fn a_lane_ahead_of_git_is_never_moved_back() {
    let b = bench();
    b.seed(
        &lane_key("alpha"),
        json!({ "landing_ref": "landed-2", "image_digest": IMAGE, "harness_version": HARNESS }),
    );
    assert_eq!(record(&b.ports(), "alpha"), Ok(Recorded::Unchanged));
    assert_eq!(lane_of(&b).unwrap().0["landing_ref"], "landed-2");
}

#[test]
fn lane_record_writes_each_repo_alone_and_names_the_ones_it_could_not() {
    let b = bench();
    let (out, ok) = record_all(&b.ports(), &["alpha".into(), "ghost".into()]);
    assert!(!ok);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "repo=alpha lane=written");
    assert!(lines[1].starts_with("repo=ghost lane=refused unreadable git/ghost/landing-ref"), "{out}");
    assert!(b.verbs.calls().is_empty(), "lane record cuts and lands nothing");
    let (out, ok) = record_all(&b.ports(), &["alpha".into()]);
    assert_eq!((out.as_str(), ok), ("repo=alpha lane=unchanged\n", true));

    let none = Bench::new();
    assert_eq!(record_all(&none.ports(), &["alpha".into()]), ("repo=alpha lane=not-rounds\n".to_string(), true));
}
