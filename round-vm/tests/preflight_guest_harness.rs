//! The preflight guest builds into a cached CARGO_TARGET_DIR outside the tree, so testenv
//! cannot find its harness above its own binary; the guest declares it (sp-0aqtny).

const GUEST: &str = include_str!("../../spira/round-image-preflight-guest.sh");

#[test]
fn the_preflight_guest_declares_the_testenv_harness_before_the_setup() {
    let declared = GUEST.find("\nexport SPIRA_TESTENV_HARNESS=/root/tree\n").expect("the guest declares SPIRA_TESTENV_HARNESS as the tree");
    let setup = GUEST.find("round-vm/ami-setup.sh /root/tree").expect("the guest runs ami-setup.sh");
    assert!(declared < setup, "declared before the setup that builds the template");
}
