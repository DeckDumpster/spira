//! spira/round-image-preflight.sh proves what the real round image build does and nothing more
//! (sp-hyfjgl): the Proxmox setup_command and EC2 ssm-commands.jq run ami-setup.sh with no
//! foreign checkouts, so the preflight mounts none and runs no foreign runner-deps check.

const HOST: &str = include_str!("../../spira/round-image-preflight.sh");
const GUEST: &str = include_str!("../../spira/round-image-preflight-guest.sh");

#[test]
fn the_preflight_runs_no_foreign_repo_check_the_real_build_does_not() {
    for (name, text) in [("round-image-preflight.sh", HOST), ("round-image-preflight-guest.sh", GUEST)] {
        for gone in ["/foreign", "runner-deps", "SPIRA_PREFLIGHT_FOREIGN", "foreign_checkouts"] { // not a path: script text the preflight must not name
            assert!(!text.contains(gone), "{name} still names {gone:?}");
        }
    }
}

#[test]
fn the_guest_runs_ami_setup_as_the_real_builds_do() {
    let call = GUEST.lines().find(|l| l.contains("round-vm/ami-setup.sh")).expect("the guest runs ami-setup.sh");
    assert_eq!(call.trim(), r#"bash -x round-vm/ami-setup.sh /root/tree "$toolchain" /root/.cargo"#, "three arguments, as setup_command and ssm-commands.jq pass");
}

#[test]
fn the_guest_copies_only_the_listed_source_not_the_whole_mounted_tree() {
    assert!(!GUEST.contains("cp -a /src/."), "the guest copies the tree's target/ with the source");
    assert!(GUEST.contains("tar --null -C /src -T /tree.list"), "the guest copies the host's list of source files");
}

#[test]
fn the_package_list_payload_is_update_then_the_apt_line_on_stdin_never_argv() {
    assert!(HOST.contains(r#"printf '%s\n' 'set -euo pipefail' 'export DEBIAN_FRONTEND=noninteractive' 'apt-get update -qq' "$apt_line""#));
    let run = HOST.lines().find(|l| l.contains("run --rm -i --network host")).expect("the package-list run");
    assert!(run.contains(r#"-v "$CACHE/apt:/var/cache/apt/archives""#) && !run.contains("apt_line"), "{run}");
    assert!(HOST.contains(r#"PODMAN=(podman --root "$ROOT/storage""#));
}

#[test]
fn a_failing_install_prints_the_red_block_with_the_log_tail_and_exits_1() {
    assert!(HOST.contains(r#"RED rc=$rc after"#));
    assert!(HOST.contains(r#"tail -n 40 "$log""#));
    let red = &HOST[HOST.find(r#"if [ "$rc" -ne 0 ]"#).unwrap()..];
    assert!(red.contains("exit 1"));
}

#[test]
fn a_skipped_diff_keeps_its_fence_line() {
    assert!(HOST.contains("(0 touched, setup not run)"));
}
