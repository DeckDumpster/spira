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

/// A copied script beside a stub conf.sh, plus a PATH dir for stub `release` and `round-vm`.
struct Stubbed {
    tmp: testkit::TempDir,
}

impl Stubbed {
    fn new(name: &str, release: &str, plan: &str) -> Self {
        let tmp = testkit::TempDir::new(name);
        let spira = tmp.path().join("tree/spira");
        let bin = tmp.path().join("bin");
        for d in [&spira, &bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(spira.join("round-image-preflight.sh"), HOST).unwrap();
        std::fs::write(spira.join("conf.sh"), "[ -f \"$SPIRA_TOML\" ] && echo \"conf.sh read $SPIRA_TOML\"\n").unwrap();
        // Only these are linked in, so a host podman is never on the script's PATH.
        for tool in ["dirname", "mktemp", "rm"] {
            let real = std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|d| d.join(tool)).find(|p| p.is_file()).expect(tool);
            std::os::unix::fs::symlink(real, bin.join(tool)).unwrap();
        }
        testkit::write_exe(bin.join("release"), release);
        testkit::write_exe(bin.join("round-vm"), &format!("#!/bin/bash\nprintf '{plan}\\n'\n"));
        Stubbed { tmp }
    }

    fn run(&self) -> (Option<i32>, String, String) {
        let o = std::process::Command::new("/bin/bash")
            .arg(self.tmp.path().join("tree/spira/round-image-preflight.sh"))
            .args(["--base", "HEAD"])
            .env_clear()
            .env("PATH", self.tmp.path().join("bin"))
            .env("SPIRA_TOML", "box.toml")
            .output()
            .unwrap();
        (o.status.code(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
    }
}

const REFUSAL: &str = "release: spira.task_modes names incident.file=act, which this release does not register";

fn refusing_release(marker: &std::path::Path) -> String {
    format!("#!/bin/bash\n: > '{}'\necho '{REFUSAL}' >&2\nexit 1\n", marker.display())
}

#[test_id(tzccjyz)]
#[test]
fn the_config_is_read_with_the_trees_delta_staged_and_the_layers_are_removed() {
    let staging = "#!/bin/bash\n[ \"$1\" = config-stage ] && [ \"$SPIRA_TOML\" = box.toml ] && [ \"$2\" -ef \"${0%/*}/../tree\" ] && : > \"$TMPDIR/staged.toml\" && echo \"$TMPDIR/staged.toml\"\n";
    let (rc, out, err) = Stubbed::new("preflight-staged-config", staging, "plan=full\\nnpaths=1\\n").run();
    assert_eq!(rc, Some(75), "no podman: {out}{err}");
    assert!(err.contains("podman is not on PATH"), "{err}");
    let read = out.lines().find_map(|l| l.strip_prefix("conf.sh read ")).expect("conf.sh read the staged config");
    assert!(read.ends_with("/staged.toml"), "{out}");
    assert!(!std::path::Path::new(read).parent().unwrap().exists(), "the staged scratch is removed");
}

#[test_id(touzna2)]
#[test]
fn a_branch_touching_no_recipe_skips_without_staging_the_config() {
    let marker = testkit::TempDir::new("preflight-skip-marker");
    let ran = marker.path().join("release-ran");
    let (rc, out, err) = Stubbed::new("preflight-skip-unstaged", &refusing_release(&ran), "plan=skip\\nnpaths=9\\n").run();
    assert_eq!(rc, Some(0), "{out}{err}");
    assert!(out.contains("fence: round-image-preflight checked 9 recipe paths against the diff (0 touched, setup not run)\n"), "{out}");
    assert!(!out.contains("conf.sh read"), "{out}");
    assert!(!ran.exists(), "release ran on a skip: {err}");
}

#[test_id(tfpcd1m)]
#[test]
fn a_recipe_branch_refused_staging_names_the_rebase() {
    let marker = testkit::TempDir::new("preflight-refused-marker");
    let stub = refusing_release(&marker.path().join("release-ran"));
    let (rc, out, err) = Stubbed::new("preflight-refused-staging", &stub, "plan=full\\nnpaths=9\\n").run();
    assert_eq!(rc, Some(75), "{out}{err}");
    assert!(err.contains(REFUSAL), "{err}");
    assert!(err.contains("round-image-preflight: cannot stage the tree config delta; rebase onto HEAD and rerun"), "{err}");
}
