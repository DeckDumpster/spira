//! Family V — `spira_repo_lanes` / `_spira_expand_lanes` / `spira_lane_admitted`, ported
//! from `spira/lib.sh` (wave4-decomposition.md row V, wave 4.35, sp-kelr2). Pure
//! argv-in/string-out logic lives here; `real.rs`'s `Real` supplies the registry lookup and
//! the label values — resolved via `spira_config::resolve` (the same `env_or` precedence
//! `run()`'s own `maechen_label` already uses), never read raw, per
//! law-a-binary-resolves-the-config-it-reads.
//!
//! `_spira_expand_lanes` is folded into [`repo_lanes`] rather than kept as its own CLI door:
//! grepped the whole tree, and nothing outside `spira_repo_lanes` itself ever called it —
//! lib.sh's own copy is retired, not shimmed (`spira_open_trigger_count`, the fourth
//! function this row names, has no shared logic with the other three and is ported
//! separately in `real.rs`/`main.rs`).

/// The six lane labels this family's functions compare against — one field per
/// `SPIRA_*_LABEL` conf key. The caller resolves each (module doc) and builds this once per
/// invocation.
#[derive(Debug, Clone)]
pub struct LaneLabels {
    pub plan: String,
    pub incident: String,
    pub groomer: String,
    pub maechen: String,
    pub spike: String,
    pub czar: String,
}

impl LaneLabels {
    /// Validation order AND the unknown-lane refusal's "valid:" list order — `$p,$inc,$gr,
    /// $mae,$sp,$cz` in the bash.
    fn known(&self) -> [&str; 6] {
        [self.plan.as_str(), self.incident.as_str(), self.groomer.as_str(), self.maechen.as_str(), self.spike.as_str(), self.czar.as_str()]
    }

    /// `spira_repo_lanes`'s own no-lanes-column fallback: `"$p $inc $gr $mae $sp $cz"`.
    /// NOTE this is a DIFFERENT label order than `self` mode below (maechen before spike,
    /// not after) — verbatim from the bash, not a typo here.
    fn default_set(&self) -> String {
        format!("{} {} {} {} {} {}", self.plan, self.incident, self.groomer, self.maechen, self.spike, self.czar)
    }
}

/// `_spira_expand_lanes <repo-name> <raw>`. `Err` carries the exact stderr line the bash
/// printed (minus the trailing newline) on an unknown mode or lane label.
fn expand(name: &str, raw: &str, l: &LaneLabels) -> Result<String, String> {
    match raw {
        "consume" => return Ok(l.plan.clone()),
        "develop" => return Ok(format!("{} {} {} {}", l.plan, l.incident, l.groomer, l.spike)),
        "self" => return Ok(format!("{} {} {} {} {} {}", l.plan, l.incident, l.groomer, l.spike, l.maechen, l.czar)),
        _ => {}
    }
    let known = l.known();
    let mut result: Vec<&str> = Vec::new();
    for item in raw.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if !known.contains(&item) {
            return Err(format!("spira: repo:{name} — unknown lane {item} (valid: {})", known.join(",")));
        }
        result.push(item);
    }
    if result.is_empty() {
        return Ok(l.plan.clone());
    }
    Ok(result.join(" "))
}

/// `spira_repo_lanes <name>`, given the raw `lanes` column already read from the registry.
/// `None` or `Some("")` — `repo_field`'s own "absent and present-but-blank are the same
/// answer" convention — both mean "no lanes column": admit every lane.
pub fn repo_lanes(name: &str, raw: Option<&str>, l: &LaneLabels) -> Result<String, String> {
    match raw.filter(|r| !r.is_empty()) {
        None => Ok(l.default_set()),
        Some(raw) => expand(name, raw, l),
    }
}

/// `spira_lane_admitted <lane>`: the home repo (if any) is checked first, then every repo
/// named in the map, in map order — "any yes wins", matching the bash's own two-stage scan.
/// A lookup that errors (an unknown mode/lane on some OTHER repo's row) counts as "no" for
/// that repo and never aborts the scan, exactly as the bash's `|| continue` did.
pub fn lane_admitted<F>(lane: &str, home_repo: &str, names: &[String], raw_lanes: F, l: &LaneLabels) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    let admits = |name: &str| matches!(repo_lanes(name, raw_lanes(name).as_deref(), l), Ok(lanes) if lanes.split(' ').any(|x| x == lane));
    (!home_repo.is_empty() && admits(home_repo)) || names.iter().any(|n| admits(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> LaneLabels {
        LaneLabels {
            plan: "plan".into(),
            incident: "incident".into(),
            groomer: "groom".into(),
            maechen: "maechen-sweep".into(), // literal-ok: test fixture
            spike: "spike".into(),
            czar: "czar-trigger".into(),
        }
    }

    #[test]
    fn no_lanes_column_admits_every_lane() {
        let out = repo_lanes("alpha", None, &labels()).unwrap();
        for l in ["plan", "incident", "groom", "maechen-sweep", "spike", "czar-trigger"] { // literal-ok: test fixture
            assert!(out.split(' ').any(|x| x == l), "{out} missing {l}");
        }
    }

    #[test]
    fn empty_lanes_field_admits_every_lane() {
        let out = repo_lanes("alpha", Some(""), &labels()).unwrap();
        assert_eq!(out.split(' ').count(), 6);
    }

    #[test]
    fn consume_mode_yields_plan_only() {
        assert_eq!(repo_lanes("alpha", Some("consume"), &labels()).unwrap(), "plan");
    }

    #[test]
    fn develop_mode_yields_four_lanes_in_order() {
        assert_eq!(repo_lanes("beta", Some("develop"), &labels()).unwrap(), "plan incident groom spike");
    }

    #[test]
    fn self_mode_yields_all_six_in_order() {
        assert_eq!(repo_lanes("gamma", Some("self"), &labels()).unwrap(), "plan incident groom spike maechen-sweep czar-trigger"); // literal-ok: test fixture
    }

    #[test]
    fn explicit_lane_list_yields_exactly_those_lanes_in_the_given_order() {
        assert_eq!(repo_lanes("alpha", Some("plan,incident"), &labels()).unwrap(), "plan incident");
        assert_eq!(repo_lanes("alpha", Some("spike"), &labels()).unwrap(), "spike");
    }

    #[test]
    fn unknown_mode_is_a_hard_error_naming_the_row() {
        let err = repo_lanes("alpha", Some("fullaccess"), &labels()).unwrap_err();
        assert!(err.contains("alpha"), "{err}");
    }

    #[test]
    fn unknown_lane_is_a_hard_error_naming_the_row_and_the_label() {
        let err = repo_lanes("alpha", Some("plan,bogus-lane"), &labels()).unwrap_err();
        assert!(err.contains("alpha") && err.contains("bogus-lane"), "{err}");
    }

    #[test]
    fn lane_admitted_checks_the_home_repo_first() {
        assert!(lane_admitted("plan", "home", &[], |_| None, &labels()));
    }

    #[test]
    fn lane_admitted_falls_back_to_the_repo_map_when_the_home_repo_does_not_admit() {
        let names = vec!["satellite".to_string()];
        let raw = |n: &str| if n == "satellite" { Some("develop".to_string()) } else { Some("consume".to_string()) };
        assert!(lane_admitted("incident", "home", &names, raw, &labels()));
        assert!(!lane_admitted("incident", "home", &names, |_| Some("consume".to_string()), &labels()));
    }
}
