//! spira/round-image-preflight.sh proves what the real round image build does and nothing more
//! (sp-hyfjgl): the Proxmox setup_command and EC2 ssm-commands.jq run ami-setup.sh with no
//! foreign checkouts, so the preflight mounts none and runs no foreign runner-deps check.

const HOST: &str = include_str!("../../spira/round-image-preflight.sh");
const GUEST: &str = include_str!("../../spira/round-image-preflight-guest.sh");

#[test]
fn the_preflight_runs_no_foreign_repo_check_the_real_build_does_not() {
    for (name, text) in [("round-image-preflight.sh", HOST), ("round-image-preflight-guest.sh", GUEST)] {
        for gone in ["/foreign", "runner-deps", "SPIRA_PREFLIGHT_FOREIGN", "foreign_checkouts"] {
            assert!(!text.contains(gone), "{name} still names {gone:?}");
        }
    }
}

#[test]
fn the_guest_runs_ami_setup_as_the_real_builds_do() {
    let call = GUEST.lines().find(|l| l.contains("round-vm/ami-setup.sh")).expect("the guest runs ami-setup.sh");
    assert_eq!(call.trim(), r#"bash -x round-vm/ami-setup.sh /root/tree "$toolchain" /root/.cargo"#, "three arguments, as setup_command and ssm-commands.jq pass");
}
