use super::*;

#[test]
fn create_bead_refuses_the_planning_fields_it_used_to_keep() {
    let r = Rig::new();
    for flag in ["--title", "--priority"] {
        let (rc, why) = r.lc(&["create-bead", "sp-plan", flag, "x", "--ready"]);
        assert_eq!(rc, crate::REFUSED, "{flag}: {why}");
        assert!(why.contains(flag), "{why}");
    }
    assert_eq!(r.lc(&["show", "sp-plan"]).0, 1, "a refused create leaves no record");
    // The positive control: the same call without the planning flag creates the record.
    r.ok(&["create-bead", "sp-plan", "--ready"]);
    assert_eq!(r.lc(&["show", "sp-plan"]).0, 0);
}

fn repo_of(r: &Rig, id: &str) -> serde_json::Value {
    let (rc, out) = r.lc(&["list", "--ids", id]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    rows[0]["repo"].clone()
}

#[test]
fn create_bead_with_a_repo_fills_the_repo_of_a_record_another_writer_made_without_one() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.ok(&["set-claim-inputs", "sp-a"]);
    assert_eq!(claim_view_of(&r, "sp-a")["verdict"], "cannot-tell");
    r.ok(&["create-bead", "sp-a", "--ready", "--repo", "spira"]);
    assert_eq!(repo_of(&r, "sp-a"), "spira");
    assert_ne!(claim_view_of(&r, "sp-a")["verdict"], "cannot-tell");
    let (rc, _) = r.lc(&["create-bead", "sp-a", "--ready", "--repo", "spira"]);
    assert_eq!(rc, 0);
}

#[test]
fn create_bead_never_overwrites_a_repo_the_record_already_names() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready", "--repo", "deck"]);
    r.ok(&["create-bead", "sp-a", "--ready", "--repo", "spira"]);
    assert_eq!(repo_of(&r, "sp-a"), "deck");
}

#[test]
fn create_bead_leaves_a_working_record_without_a_repo_and_still_exits_zero() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.bead_event("sp-a", "READY", "0", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#);
    assert_eq!(r.state("sp-a"), BeadState::Working);
    let (rc, out) = r.lc(&["create-bead", "sp-a", "--ready", "--repo", "spira"]);
    assert_eq!(rc, 0, "{out}");
    assert!(repo_of(&r, "sp-a").is_null());
}
