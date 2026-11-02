//! The off switches, checked before anything else and in this order: `run/world.halted`, the
//! repo's scoped stop on its rounds lane, then the andon section. Under any of them `pass` does
//! nothing mutating for that repo and publishes why, and a runner starts nothing new
//! (round-controller v2 §Off switches). Cutover mode removes the halt stamp for its window and
//! holds the world with scoped stops instead, so it needs nothing of its own here.

use std::path::PathBuf;

use serde_json::Value;
use spira_config::scoped_stop::RepoStop;
use state_store::Selector;

use crate::ports::Ports;

pub const HALTED_FILE: &str = "world.halted";
/// The scoped-stop lane a round controller obeys.
pub const ROUNDS_LANE: &str = "rounds";
/// Every andon section: `telemetry/sections/Andon/<name>`.
pub const ANDON_PREFIX: &str = "telemetry/sections/Andon/";
/// The controller's section kind, `RoundController/<repo>`, which a pulled cord names.
pub const SECTION_KIND: &str = "RoundController";

/// The run directory's two off switches: the halt stamp and the scoped stops.
pub trait Stops {
    /// `run/world.halted` is present.
    fn halted(&self) -> bool;
    /// The stop recorded on `repo`'s `lane`, if any.
    fn scoped(&self, repo: &str, lane: &str) -> Option<RepoStop>;
}

/// The real run directory.
pub struct RunDir(pub PathBuf);

impl Stops for RunDir {
    fn halted(&self) -> bool {
        self.0.join(HALTED_FILE).exists()
    }
    fn scoped(&self, repo: &str, lane: &str) -> Option<RepoStop> {
        spira_config::scoped_stop::stop_for(&self.0, repo, lane)
    }
}

/// No halt stamp and no scoped stop: what a test that is not about the off switches runs under.
pub struct NoStops;

impl Stops for NoStops {
    fn halted(&self) -> bool {
        false
    }
    fn scoped(&self, _: &str, _: &str) -> Option<RepoStop> {
        None
    }
}

/// Why nothing may start for a repo: who set it, why, and what lifts it
/// (law-every-hold-names-who-lifts-it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    Halted,
    Stopped {
        owner: String,
        reason: String,
        until: String,
    },
    /// A cord in the andon section `section` is pulled with `condition`, and its scope covers
    /// the repo.
    AndonPulled { section: String, condition: String },
    /// The andon sections could not be read. A pass that cannot read its off switch does nothing.
    AndonUnreadable { key: String, why: String },
}

impl Stop {
    pub const WORDS: [&'static str; 4] = ["halted", "scoped-stop", "andon-pulled", "andon-unreadable"];

    pub fn word(&self) -> &'static str {
        match self {
            Stop::Halted => Stop::WORDS[0],
            Stop::Stopped { .. } => Stop::WORDS[1],
            Stop::AndonPulled { .. } => Stop::WORDS[2],
            Stop::AndonUnreadable { .. } => Stop::WORDS[3],
        }
    }

    /// Who set it.
    pub fn owner(&self) -> String {
        match self {
            Stop::Halted => "world".into(),
            Stop::Stopped { owner, .. } => owner.clone(),
            Stop::AndonPulled { section, .. } => format!("Andon/{section}"),
            Stop::AndonUnreadable { key, .. } => key.clone(),
        }
    }

    pub fn reason(&self) -> String {
        match self {
            Stop::Halted => format!("{HALTED_FILE} is present"),
            Stop::Stopped { reason, .. } => reason.clone(),
            Stop::AndonPulled { condition, .. } => format!("condition {condition} is pulled"),
            Stop::AndonUnreadable { why, .. } => why.clone(),
        }
    }

    /// What lifts it.
    pub fn until(&self) -> String {
        match self {
            Stop::Halted => "world start removes the halt stamp".into(),
            Stop::Stopped { until, .. } => until.clone(),
            Stop::AndonPulled { condition, .. } => format!("{condition} is no longer True"),
            Stop::AndonUnreadable { key, .. } => format!("{key} reads again"),
        }
    }

    /// The runner's one line for it.
    pub fn why(&self, repo: &str) -> String {
        match self {
            Stop::Halted => format!("{HALTED_FILE} is present: the world is stopped"),
            Stop::Stopped { owner, reason, .. } => {
                format!("{repo}'s {ROUNDS_LANE} lane is stopped by {owner}: {reason}")
            }
            Stop::AndonPulled { section, condition } => {
                format!("the andon cord Andon/{section} is pulled on {repo}: {condition}")
            }
            Stop::AndonUnreadable { key, why } => {
                format!("the andon sections under {key} cannot be read: {why}")
            }
        }
    }
}

/// The first off switch set for `repo`, in the order the design fixes, or `None`.
pub fn check(p: &Ports, repo: &str) -> Option<Stop> {
    if p.stops.halted() {
        return Some(Stop::Halted);
    }
    if let Some(s) = p.stops.scoped(repo, ROUNDS_LANE) {
        return Some(Stop::Stopped {
            owner: s.owner,
            reason: s.reason,
            until: s.until,
        });
    }
    andon(p, repo)
}

/// A pulled cord: a condition whose `status` is `True` and whose `reason` names the repo's
/// section (`RoundController/<repo>`), as the reconciler reads a cord's scope, or the whole kind
/// (`RoundController`), which stops every repo's rounds. No andon section means no cord exists,
/// so none is pulled. A store that cannot list the sections stops the pass; a section or
/// condition that does not decode names no scope, so it blinds no other, as the reconciler
/// keeps an undecodable section by key and reads the rest. The andon design owns freshness.
fn andon(p: &Ports, repo: &str) -> Option<Stop> {
    let unreadable = |why: String| {
        Some(Stop::AndonUnreadable {
            key: ANDON_PREFIX.into(),
            why,
        })
    };
    let items = match p.store.list(&[ANDON_PREFIX], &Selector::Everything) {
        Ok((items, _)) => items,
        Err(e) => return unreadable(e.to_string()),
    };
    let ours = format!("{SECTION_KIND}/{repo}");
    for i in items {
        let section = i.key.strip_prefix(ANDON_PREFIX).unwrap_or(&i.key).to_string();
        let Some(Value::Array(conditions)) = i.value.data.pointer("/status/conditions") else {
            continue;
        };
        for c in conditions {
            let field = |k: &str| c.get(k).and_then(Value::as_str);
            let (Some(kind), Some(status)) = (field("type"), field("status")) else {
                continue;
            };
            let reason = field("reason").unwrap_or("");
            if status == "True" && (reason == ours || reason == SECTION_KIND) {
                return Some(Stop::AndonPulled {
                    section,
                    condition: kind.into(),
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::bench::Bench;
    use serde_json::json;

    fn cord(b: &Bench, name: &str, conditions: Value) {
        b.seed_ttl(
            &format!("{ANDON_PREFIX}{name}"),
            json!({ "kind": "Andon", "status": { "conditions": conditions } }),
            604_800,
        );
    }

    fn pulled(kind: &str, status: &str, reason: &str) -> Value {
        json!({ "type": kind, "status": status, "reason": reason, "lastTransitionTime": "t" })
    }

    #[test]
    fn nothing_set_and_no_andon_section_is_no_stop() {
        let b = Bench::new();
        assert_eq!(check(&b.ports(), "r"), None);
    }

    #[test]
    fn the_halt_stamp_comes_before_a_scoped_stop_and_a_pulled_cord() {
        let b = Bench::new();
        b.stops.stop("r", "ops", "drill", "manual");
        cord(&b, "main", json!([pulled("Stop", "True", "RoundController/r")]));
        let p = b.ports();
        assert_eq!(check(&p, "r").unwrap().word(), "scoped-stop");
        b.stops.halt();
        assert_eq!(check(&p, "r"), Some(Stop::Halted));
    }

    #[test]
    fn a_scoped_stop_names_its_owner_reason_and_lift_and_covers_only_its_repo() {
        let b = Bench::new();
        b.stops.stop("r", "ops", "drill", "bead:sp-x=LANDED");
        let p = b.ports();
        let s = check(&p, "r").unwrap();
        assert_eq!(
            (s.owner(), s.reason(), s.until()),
            ("ops".into(), "drill".into(), "bead:sp-x=LANDED".into())
        );
        assert_eq!(s.why("r"), "r's rounds lane is stopped by ops: drill");
        assert_eq!(check(&p, "other"), None);
    }

    #[test]
    fn a_cord_is_pulled_only_when_true_and_scoped_to_the_repo_or_the_whole_kind() {
        let b = Bench::new();
        cord(
            &b,
            "main",
            json!([
                pulled("Stop", "False", "RoundController/r"),
                pulled("Stop", "True", "RoundController/other"),
                pulled("Stop", "True", "Claims/r"),
            ]),
        );
        b.seed_ttl(&format!("{ANDON_PREFIX}bare"), json!({ "kind": "Andon" }), 604_800);
        let p = b.ports();
        assert_eq!(check(&p, "r"), None);
        assert_eq!(
            check(&p, "other"),
            Some(Stop::AndonPulled {
                section: "main".into(),
                condition: "Stop".into()
            })
        );
        cord(&b, "all", json!([pulled("Freeze", "True", "RoundController")]));
        let s = check(&p, "r").unwrap();
        assert_eq!(
            (s.word(), s.owner(), s.reason(), s.until()),
            (
                "andon-pulled",
                "Andon/all".into(),
                "condition Freeze is pulled".into(),
                "Freeze is no longer True".into()
            )
        );
        assert_eq!(s.why("r"), "the andon cord Andon/all is pulled on r: Freeze");
    }

    #[test]
    fn andon_sections_that_cannot_be_listed_stop_the_pass() {
        let down = Bench::unserved();
        let s = check(&down.ports(), "r").unwrap();
        assert_eq!((s.word(), s.owner()), ("andon-unreadable", ANDON_PREFIX.to_string()));
        assert!(s.until().contains("reads again") && s.why("r").contains("cannot be read"));
        assert!(!s.reason().is_empty());
    }

    #[test]
    fn a_section_or_condition_that_does_not_decode_blinds_no_other() {
        let b = Bench::new();
        b.seed_ttl(
            &format!("{ANDON_PREFIX}garbled"),
            json!({ "status": { "conditions": "pulled" } }),
            604_800,
        );
        cord(&b, "half", json!([{ "type": "Stop" }]));
        let p = b.ports();
        assert_eq!(check(&p, "r"), None);
        cord(&b, "real", json!([pulled("Stop", "True", "RoundController/r")]));
        assert_eq!(check(&p, "r").unwrap().owner(), "Andon/real");
    }

    #[test]
    fn the_halt_names_the_stamp_and_world_start() {
        let s = Stop::Halted;
        assert_eq!(
            (s.word(), s.owner(), s.reason(), s.until(), s.why("r")),
            (
                "halted",
                "world".into(),
                "world.halted is present".into(),
                "world start removes the halt stamp".into(),
                "world.halted is present: the world is stopped".into()
            )
        );
    }

    #[test]
    fn the_run_dir_s_switches_are_the_halt_stamp_and_a_scoped_stop_on_rounds() {
        let d = testkit::TempDir::new("rc-switches-run-dir");
        let s = RunDir(d.path().to_path_buf());
        assert!(!s.halted());
        assert_eq!(s.scoped("home", ROUNDS_LANE), None);
        spira_config::scoped_stop::pause_repo(
            d.path(),
            "home",
            &["claims".into()],
            "ops",
            "drill",
            "manual",
            1,
        )
        .unwrap();
        assert_eq!(
            s.scoped("home", ROUNDS_LANE),
            None,
            "a stop on another lane is not this one"
        );
        spira_config::scoped_stop::pause_repo(
            d.path(),
            "home",
            &[ROUNDS_LANE.into()],
            "ops",
            "drill",
            "manual",
            1,
        )
        .unwrap();
        assert_eq!(s.scoped("home", ROUNDS_LANE).unwrap().owner, "ops");
        std::fs::write(d.path().join(HALTED_FILE), "").unwrap();
        assert!(s.halted());
    }
}
