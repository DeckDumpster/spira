use super::in_nix_profile;
use std::path::PathBuf;

#[test_id(tmgncta)]
#[test]
fn a_nix_program_comes_from_the_profile_bin_and_an_unset_key_is_refused_by_name() {
    assert_eq!(in_nix_profile(Ok("p/bin".into()), "nix-store"), Ok(PathBuf::from("p/bin/nix-store")));
    let e = in_nix_profile(Ok(String::new()), "nix").unwrap_err();
    assert!(e.contains("SPIRA_NIX_PROFILE_BIN is not set"), "{e}");
    assert_eq!(in_nix_profile(Err("unresolved".into()), "nix"), Err("unresolved".into()));
}
