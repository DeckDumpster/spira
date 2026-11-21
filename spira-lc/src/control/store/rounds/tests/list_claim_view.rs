use super::*;


#[test_id(txu5hjh)]
#[test]
fn list_answers_a_beads_claimability_from_its_prerequisite_when_it_is_read() {
    let r = Rig::new();
    for id in ["sp-a", "sp-b"] {
        r.ok(&["create-bead", id, "--repo", "spira", "--ready"]);
    }
    r.ok(&["set-claim-inputs", "sp-a"]);
    r.ok(&["set-claim-inputs", "sp-b", "--block", "sp-a"]);

    let b = claim_view_of(&r, "sp-b");
    assert_eq!((b["verdict"].as_str(), b["blocker"].as_str()), (Some("blocked"), Some("sp-a")));
    assert_eq!(claim_view_of(&r, "sp-a")["verdict"], "claimable");

    r.bead_event("sp-a", "READY", "0", r#"{"ContentOnBase":{"proof":"p"}}"#);
    assert_eq!(r.state("sp-a"), BeadState::Landed);
    assert_eq!(claim_view_of(&r, "sp-b")["verdict"], "claimable", "no write-back stands between the landing and the next list");
    assert!(claim_view_of(&r, "sp-a").is_null(), "a LANDED record has no claim view");
}

#[test_id(tuxylj4)]
#[test]
fn a_ready_record_without_a_repo_says_the_repo_is_what_its_claim_view_lacks() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.ok(&["set-claim-inputs", "sp-a"]);
    let v = claim_view_of(&r, "sp-a");
    assert_eq!(v["verdict"], "cannot-tell");
    assert_eq!(v["missing"], serde_json::json!(["repo"]));
}

#[test_id(tmbbbhv)]
#[test]
fn list_keeps_a_pilgrimage_blocked_while_its_blocker_is_in_delivery_and_claimable_once_it_lands() {
    let r = Rig::new();
    open_round(&r);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
    r.ok(&["create-bead", "sp-b", "--repo", "spira", "--ready"]);
    r.ok(&["set-claim-inputs", "sp-b", "--block", "sp-a", "--label", "pilgrimage"]);

    let b = claim_view_of(&r, "sp-b");
    assert_eq!((b["verdict"].as_str(), b["blocker"].as_str()), (Some("blocked"), Some("sp-a")));
    assert_eq!(b["runes"], serde_json::json!([]));

    r.bead_event("sp-a", "IN_DELIVERY", "4", r#"{"ContentOnBase":{"proof":"p"}}"#);
    assert_eq!(r.state("sp-a"), BeadState::Landed);
    assert_eq!(claim_view_of(&r, "sp-b")["verdict"], "claimable");
}

#[test_id(tqezht8)]
#[test]
fn list_with_unreadable_view_inputs_still_lists_and_each_ready_row_says_it_cannot_tell() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    r.ok(&["create-bead", "sp-w", "--repo", "spira"]);
    let args: Vec<String> = ["list", "--live"].iter().map(|s| s.to_string()).collect();
    let req = r.req();
    let (rc, out) = state::run(&r.s, &args, &Call { now: AT, request: &req.id, inputs: &crate::control::claimable::UNREADABLE });
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let by = |id: &str| rows.as_array().unwrap().iter().find(|r| r["bead_id"] == id).unwrap().clone();
    assert_eq!(by("sp-a")["claim_view"]["verdict"], "cannot-tell");
    assert!(by("sp-a")["claim_view"]["error"].as_str().unwrap().contains("SPIRA_STACK_MAX_DEPTH"));
    assert!(by("sp-w")["claim_view"].is_null(), "a record the view does not cover is untouched");
}
