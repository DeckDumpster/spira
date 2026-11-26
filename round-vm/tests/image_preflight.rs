//! spira/round-image-preflight.sh proves what the real round image build does and nothing more
//! (sp-hyfjgl): the Proxmox setup_command and EC2 ssm-commands.jq run ami-setup.sh with no
//! foreign checkouts, so the preflight mounts none and runs no foreign runner-deps check.

#[macro_use]
extern crate spira_test_id;
const HOST: &str = include_str!("../../spira/round-image-preflight.sh");
const GUEST: &str = include_str!("../../spira/round-image-preflight-guest.sh");

#[test_id(tfnp4bl)]
#[test]
fn the_preflight_runs_no_foreign_repo_check_the_real_build_does_not() {
    for (name, text) in [("round-image-preflight.sh", HOST), ("round-image-preflight-guest.sh", GUEST)] {
        for gone in ["/foreign", "runner-deps", "SPIRA_PREFLIGHT_FOREIGN", "foreign_checkouts"] { // not a path: script text the preflight must not name
            assert!(!text.contains(gone), "{name} still names {gone:?}");
        }
    }
}

#[test_id(tkp7xt9)]
#[test]
fn the_guest_runs_ami_setup_as_the_real_builds_do() {
    let call = GUEST.lines().find(|l| l.contains("round-vm/ami-setup.sh")).expect("the guest runs ami-setup.sh");
    assert_eq!(call.trim(), r#"bash -x round-vm/ami-setup.sh /root/tree "$toolchain" /root/.cargo"#, "three arguments, as setup_command and ssm-commands.jq pass");
}

#[test_id(ttcqhen)]
#[test]
fn the_guest_copies_only_the_listed_source_not_the_whole_mounted_tree() {
    assert!(!GUEST.contains("cp -a /src/."), "the guest copies the tree's target/ with the source");
    assert!(GUEST.contains("tar --null -C /src -T /tree.list"), "the guest copies the host's list of source files");
}

#[test_id(t2749f4)]
#[test]
fn the_package_list_payload_is_update_then_the_apt_line_on_stdin_never_argv() {
    assert!(HOST.contains(r#"printf '%s\n' 'set -euo pipefail' 'export DEBIAN_FRONTEND=noninteractive' 'apt-get update -qq' "$apt_line""#));
    let at = HOST.find("'apt-get update -qq'").unwrap();
    let pipeline = &HOST[at..at + HOST[at..].find(">\"$log\"").unwrap()];
    assert_eq!(pipeline.matches("apt_line").count(), 1, "{pipeline}");
    assert!(pipeline.find("apt_line").unwrap() < pipeline.find("| timeout").unwrap(), "the apt line is only the printf's input: {pipeline}");
    assert!(pipeline.contains(r#"-v "$CACHE/apt:/var/cache/apt/archives""#), "{pipeline}");
    assert!(HOST.contains(r#"PODMAN=(podman --root "$ROOT/storage""#));
}

#[test_id(tjzuno1)]
#[test]
fn a_failing_install_prints_the_red_block_with_the_log_tail_and_exits_1() {
    assert!(HOST.contains(r#"RED rc=$rc after"#));
    assert!(HOST.contains(r#"tail -n 40 "$log""#));
    let red = &HOST[HOST.find(r#"if [ "$rc" -ne 0 ]"#).unwrap()..];
    assert!(red.contains("exit 1"));
}

#[test_id(t47axaq)]
#[test]
fn a_skipped_diff_keeps_its_fence_line() {
    assert!(HOST.contains("(0 touched, setup not run)"));
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let st = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).status().unwrap();
    assert!(st.success(), "git {args:?}");
}

#[test_id(t4n0jd6)]
#[test]
fn the_verb_reads_a_real_diff_into_a_plan() {
    let tmp = testkit::TempDir::new("preflight-plan-verb");
    let d = tmp.path();
    std::fs::create_dir_all(d.join("round-vm")).unwrap();
    git(d, &["init", "-q", "-b", "main", "."]);
    std::fs::write(d.join("round-vm/ami-setup.sh"), "apt-get install -y curl\nsystemctl enable ssh\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "base"]);
    std::fs::write(d.join("round-vm/ami-setup.sh"), "apt-get install -y curl git\nsystemctl enable ssh\n").unwrap();
    git(d, &["commit", "-qam", "apt"]);
    let verb = |base: &str| {
        let o = std::process::Command::new(testkit::bin_exe("round-vm")).args(["preflight-plan", "--tree"]).arg(d).args(["--base", base, "--head", "HEAD"]).output().unwrap();
        (o.status.code(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    let (rc, out, _) = verb("HEAD~1");
    assert_eq!(rc, Some(0));
    assert!(out.contains("plan=package-list\n") && out.contains("touched=round-vm/ami-setup.sh\n") && out.contains("apt_line=apt-get install -y curl git\n"), "{out}");
    let (rc, out, _) = verb("HEAD");
    assert_eq!((rc, out.contains("plan=skip\n")), (Some(0), true), "{out}");
    let (rc, _, err) = verb("no-such-ref");
    assert_eq!(rc, Some(2));
    assert!(err.contains("cannot diff no-such-ref...HEAD"), "{err}");
}

#[test_id(txlo4w0)]
#[test]
fn a_comment_only_containerfile_edit_plans_a_skip_and_a_run_edit_plans_the_full_setup() {
    let tmp = testkit::TempDir::new("preflight-plan-containerfile");
    let d = tmp.path();
    let cf = d.join("spira/testenv/Containerfile");
    std::fs::create_dir_all(cf.parent().unwrap()).unwrap();
    git(d, &["init", "-q", "-b", "main", "."]);
    std::fs::write(&cf, "# top\nFROM a\nRUN x\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "base"]);
    std::fs::write(&cf, "# reworded\nFROM a\nRUN x\n").unwrap();
    git(d, &["commit", "-qam", "comment"]);
    std::fs::write(&cf, "# reworded\nFROM a\nRUN y\n").unwrap();
    git(d, &["commit", "-qam", "run"]);
    let plan = |base: &str, head: &str| {
        let o = std::process::Command::new(testkit::bin_exe("round-vm")).args(["preflight-plan", "--tree"]).arg(d).args(["--base", base, "--head", head]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let out = plan("HEAD~2", "HEAD~1");
    assert!(out.contains("plan=skip\n") && out.contains("touched=\n"), "{out}");
    assert!(plan("HEAD~2", "HEAD").contains("plan=full\n"));
}

#[test_id(tzccjyz)]
#[test]
fn the_config_is_read_with_the_trees_delta_staged_and_the_layers_are_removed() {
    let tmp = testkit::TempDir::new("preflight-staged-config");
    let spira = tmp.path().join("tree/spira");
    let bin = tmp.path().join("bin");
    let scratch = tmp.path().join("release-config-1");
    let staged = scratch.join("layer/staged.toml");
    for d in [&spira, &bin, &scratch.join("layer")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(&staged, "").unwrap();
    std::fs::write(spira.join("round-image-preflight.sh"), HOST).unwrap();
    std::fs::write(spira.join("conf.sh"), "echo \"conf.sh read $SPIRA_TOML\"\n").unwrap();
    testkit::write_exe(bin.join("release"), &format!("#!/bin/bash\n[ \"$1\" = config-stage ] && [ \"$SPIRA_TOML\" = box.toml ] && [ \"$2\" -ef '{}' ] && echo '{}'\n", spira.parent().unwrap().display(), staged.display()));
    testkit::write_exe(bin.join("round-vm"), "#!/bin/bash\nprintf 'plan=skip\\nnpaths=1\\n'\n");
    let o = std::process::Command::new("bash")
        .arg(spira.join("round-image-preflight.sh"))
        .args(["--base", "HEAD"])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("SPIRA_TOML", "box.toml")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}{}", String::from_utf8_lossy(&o.stderr));
    assert!(out.contains(&format!("conf.sh read {}", staged.display())), "{out}");
    assert!(!scratch.exists(), "the staged layers are removed");
}
