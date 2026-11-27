//! The pilgrim world, walked: file A, link A on B, see A blocked in both views, close B, see A
//! claimable in both. The walk needs a real world (a Dolt fixture and a release), which
//! `SPIRA_SIM_RELEASE` names, so it is `#[ignore]`d and run with `--ignored` where one exists.

use spira_sim::pilgrim;
use spira_sim::world::{self, lc_socket, PILGRIM_STEPS};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;
use testkit::{broker, ChildGuard};

/// Bounds each wait on the subscriber, so a lost delivery fails the test instead of hanging it.
const FAILSAFE: Duration = Duration::from_secs(30);

struct Up<'a>(&'a Path);
impl Drop for Up<'_> {
    fn drop(&mut self) {
        let _ = world::down(self.0, &PILGRIM_STEPS);
    }
}

#[test_id(t0zjc9u)]
#[test]
fn the_gates_socket_is_the_socket_the_marker_sits_beside_and_the_serve_listens_on() {
    let w = Path::new("tmp/pw");
    let socket = lc_socket(w).display().to_string();
    let settings = world::lc_settings(w);
    assert_eq!(
        settings
            .iter()
            .find(|(k, _)| k == "spira.lc_socket")
            .map(|(_, v)| v.as_str()),
        Some(socket.as_str())
    );
    assert_eq!(world::serve_argv(w).last(), Some(&socket));
    assert_eq!(
        spira_store::lc_call::pilgrim_world_marker(&socket),
        format!("{socket}.pilgrim-world")
    );
}

#[test_id(t98hkgf)]
#[test]
fn a_marker_is_refused_beside_any_socket_that_is_not_a_sim_worlds() {
    let t = testkit::TempDir::new("pw-guard");
    assert!(world::make_pilgrim_world(t.path()).is_err());
    assert!(!Path::new(&spira_store::lc_call::pilgrim_world_marker(
        &lc_socket(t.path()).display().to_string()
    ))
    .exists());
}

#[test_id(tyi9g34)]
#[test]
#[ignore = "needs SPIRA_SIM_RELEASE and a Dolt fixture: run with --ignored"]
fn a_pilgrim_files_links_and_closes_in_a_world_of_its_own_and_both_views_follow() {
    let release = std::env::var(world::RELEASE_ENV)
        .unwrap_or_else(|_| panic!("{} is unset", world::RELEASE_ENV));
    assert!(Path::new(&release).is_dir(), "{release}");
    let t = testkit::TempDir::new("pw-walk");
    let repo = t.join("repo");
    world::fixture_repo(&testkit::manifest_dir().join("../.."), &repo).unwrap();
    let dir = t.join("w");
    world::up_repos(
        &repo,
        &dir,
        "HEAD",
        &|k| std::env::var(k).ok(),
        &PILGRIM_STEPS,
        &[world::RepoSpec::parse(pilgrim::FIXTURE_REPO).unwrap()],
        &|_| Ok(()),
    )
    .unwrap();
    let dir = dir.canonicalize().unwrap();
    let _down = Up(&dir);

    let plain = t.join("plain");
    world::up_repos(
        &repo,
        &plain,
        "HEAD",
        &|k| std::env::var(k).ok(),
        &PILGRIM_STEPS,
        &[world::RepoSpec::parse(pilgrim::FIXTURE_REPO).unwrap()],
        &|_| Ok(()),
    )
    .unwrap();
    let plain = plain.canonicalize().unwrap();
    let _down_plain = Up(&plain);
    let plain_before = pilgrim::lc_view_once(&plain).unwrap();

    let pid: u32 = std::fs::read_to_string(world::lc_pid_file(&dir))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let cmdline = std::fs::read(format!("{}/{pid}/cmdline", system_paths::PROC)).unwrap();
    assert!(
        world::is_world_serve(&cmdline, &dir),
        "the serving process listens on the socket the marker will sit beside"
    );
    let gate_socket = spira_sim::drive::read_env_file(&dir.join("config/sim.env"))
        .unwrap()
        .into_iter()
        .find(|(k, _)| k == "SPIRA_LC_SOCKET")
        .map(|(_, v)| v);
    assert_eq!(gate_socket, Some(lc_socket(&dir).display().to_string()));
    let env_text = std::fs::read_to_string(dir.join("config/sim.env")).unwrap();
    let named = dwarf::pilgrim_world::model_env(&dir, &env_text).unwrap();
    assert!(named.contains(&("PILGRIM_TOML".to_string(), dir.join("config/sim.toml").display().to_string())), "{named:?}");

    pilgrim::up(&dir, pilgrim::FIXTURE, &PILGRIM_STEPS).unwrap();
    let loom = pilgrim::Loom::start(&dir).unwrap();
    let (base_claimable, _) = pilgrim::view_counts(&pilgrim::lc_view_once(&dir).unwrap());
    assert!(
        base_claimable >= 1,
        "the fixture bead is claimable in the seeded world"
    );

    let a = pilgrim::file_bead(&dir, "pilgrim walk A").unwrap();
    let b = pilgrim::file_bead(&dir, "pilgrim walk B").unwrap();
    pilgrim::link(&dir, &a, &b).unwrap();
    let blocked = pilgrim::lc_view_once(&dir).unwrap();
    assert!(blocked.contains(&a), "{blocked}");
    assert_eq!(
        pilgrim::view_counts(&blocked),
        (base_claimable + 1, 1),
        "B is claimable, A is blocked: {blocked}"
    );
    let floor = pilgrim::loom_counts(&loom.factory().unwrap()).expect("Loom's READY belt");
    assert_eq!(floor, (base_claimable as u64 + 1, 1), "Loom sees A blocked");

    let sub = Watching::start(&dir);

    pilgrim::close(&dir, &b, "walked").unwrap();
    sub.follow.delivers(&format!("bead/{b}/state state=DROPPED from=READY"));
    let freed = pilgrim::lc_view_once(&dir).unwrap();
    assert_eq!(
        pilgrim::view_counts(&freed),
        (base_claimable + 1, 0),
        "A is claimable once B is closed: {freed}"
    );
    let floor = pilgrim::loom_counts(&loom.factory().unwrap()).expect("Loom's READY belt");
    assert_eq!(
        floor,
        (base_claimable as u64 + 1, 0),
        "Loom sees A claimable"
    );

    assert_eq!(
        pilgrim::lc_view_once(&plain).unwrap(),
        plain_before,
        "the walk left the other store's view unchanged"
    );
    assert!(!plain_before.contains(&a));
    let (code, out) = pilgrim::work(
        &plain,
        "file",
        &["x", "--for", "builder", "--repo", world::REPO_NAME],
    )
    .unwrap();
    assert_eq!(
        code,
        spira_store::lc_call::REFUSED,
        "an unmarked world refuses the pilgrim: {out}"
    );

    let broker: u32 = std::fs::read_to_string(world::broker_pid_file(&dir)).unwrap().trim().parse().unwrap();
    let socket = world::broker_socket(&dir);
    assert!(UnixStream::connect(&socket).is_ok(), "positive control: the world's broker accepts");
    drop(sub);
    world::down(&dir, &PILGRIM_STEPS).unwrap();
    assert!(!Path::new(&format!("{}/{broker}", system_paths::PROC)).exists(), "broker pid {broker} outlived the world");
    assert!(UnixStream::connect(&socket).is_err(), "{} still accepts", socket.display());
}

/// The world's own `spira-sub run --as t`, watching every bead's state with a `follow` attached.
struct Watching {
    follow: Follow,
    _run: ChildGuard,
}

struct Follow {
    lines: Receiver<String>,
    _child: ChildGuard,
}

impl Follow {
    fn delivers(&self, what: &str) {
        let mut seen = Vec::new();
        while !seen.last().is_some_and(|l: &String| l.contains(what)) {
            seen.push(self.lines.recv_timeout(FAILSAFE).unwrap_or_else(|e| panic!("follow never delivered {what:?} ({e}): {seen:#?}")));
        }
    }
}

fn lines(from: impl std::io::Read + Send + 'static) -> Receiver<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || BufReader::new(from).lines().map_while(Result::ok).try_for_each(|l| tx.send(l).ok()));
    rx
}

impl Watching {
    fn start(world: &Path) -> Watching {
        let command = |args: &[&str]| {
            let mut cmd = Command::new(world.join("release/bin/spira-sub"));
            cmd.args(args)
                .args(["--as", "t"])
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("SPIRA_TOML", world.join("config/sim.toml"))
                .env("SPIRA_RELEASE", world.join("release"))
                .env("HOME", world.join("home"))
                .stdin(Stdio::null());
            cmd
        };
        let ok = |args: &[&str]| {
            let out = command(args).output().unwrap();
            assert!(out.status.success(), "spira-sub {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        // `run` publishes this once it holds the broker, by which time its control socket answers.
        let mut up = broker::Subscriber::to(&world::broker_socket(world), &["subscriber/t"]);
        let run = ChildGuard::spawn(&mut command(&["run"]));
        while !String::from_utf8_lossy(&up.recv().payload).contains("\"up\"") {}
        let listed = ok(&["list"]);
        assert!(listed.starts_with("t: broker up"), "{listed}");
        ok(&["add", "bead/+/state"]);
        let mut cmd = command(&["follow"]);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = ChildGuard::spawn(&mut cmd);
        let (stdout, stderr) = child.pipes();
        let notes = lines(stderr.unwrap());
        let follow = Follow { lines: lines(stdout.unwrap()), _child: child };
        while !notes.recv_timeout(FAILSAFE).expect("follow never attached").contains("following") {}
        Watching { follow, _run: run }
    }
}

#[test_id(t3o6si4)]
#[test]
#[ignore = "needs SPIRA_SIM_RELEASE, SPIRA_TOML and dolt on PATH: run with --ignored"]
fn a_pilgrim_session_with_no_bd_on_path_builds_a_pilgrim_world() {
    let release = std::env::var(world::RELEASE_ENV).or_else(|_| std::env::var("SPIRA_RELEASE")).expect("SPIRA_SIM_RELEASE or SPIRA_RELEASE names a release");
    let toml = std::env::var("SPIRA_TOML").unwrap_or_else(|_| panic!("SPIRA_TOML is unset"));
    let t = testkit::TempDir::new("pw-nobd");
    let dir = t.join("w");
    let out = std::process::Command::new(testkit::bin_exe("sim"))
        .args(["world", "up"])
        .arg(&dir)
        .arg("--pilgrim")
        .env_clear()
        .env("PATH", system_paths::SYSTEM_PATH_MIN)
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("SPIRA_RELEASE", &release)
        .env(world::RELEASE_ENV, &release)
        .env("SPIRA_TOML", toml)
        .env("SPIRA_LC_SOCKET", t.join("production.sock"))
        .output()
        .unwrap();
    let dir = dir.canonicalize();
    if let Ok(d) = &dir {
        let _ = world::down(d, &PILGRIM_STEPS);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(!err.contains("bd not found"), "{err}");
}

fn non_spira_worktree(t: &testkit::TempDir) -> std::path::PathBuf {
    let repo = t.join("other-repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(repo.join("README.md"), "x\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["add", "."], &["-c", "user.name=t", "-c", "user.email=t@t.invalid", "commit", "-q", "-m", "one"]] {
        assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
    }
    repo
}

fn pilgrim_up_from_non_spira_worktree(sim_release: bool, in_testenv: bool) {
    let release = std::env::var(world::RELEASE_ENV).or_else(|_| std::env::var("SPIRA_RELEASE")).expect("SPIRA_SIM_RELEASE or SPIRA_RELEASE names a release");
    let toml = std::env::var("SPIRA_TOML").unwrap_or_else(|_| panic!("SPIRA_TOML is unset"));
    let t = testkit::TempDir::new("pw-nonspira");
    let repo = non_spira_worktree(&t);
    let dir = t.join("w");
    let mut cmd = std::process::Command::new(testkit::bin_exe("sim"));
    cmd.args(["world", "up"])
        .arg(&dir)
        .args(["--pilgrim", "--repo", "fixture:queue.forge"])
        .current_dir(&repo)
        .env_clear()
        .env("PATH", system_paths::SYSTEM_PATH_MIN)
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("SPIRA_RELEASE", &release)
        .env("SPIRA_TOML", toml)
        .env("SPIRA_LC_SOCKET", t.join("production.sock"));
    if sim_release {
        cmd.env(world::RELEASE_ENV, &release);
    }
    if in_testenv {
        cmd.env("SPIRA_IN_TESTENV", "1");
    }
    let out = cmd.output().unwrap();
    let canonical = dir.canonicalize();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let seen = std::fs::read_to_string(dir.join("config/sim.env"));
    let built = dir.join("cache").exists();
    if let Ok(d) = &canonical {
        let _ = world::down(d, &PILGRIM_STEPS);
    }
    assert!(out.status.success(), "{err}");
    assert!(seen.is_ok_and(|e| e.contains("SPIRA_LC_SOCKET")), "{err}");
    assert!(!built, "a pilgrim world builds nothing");
}

#[test_id(t3ugjt9)]
#[test]
#[ignore = "needs a release, SPIRA_TOML and dolt on PATH: run with --ignored"]
fn a_pilgrim_world_for_a_non_spira_worktree_is_built_from_the_release_not_the_worktree() {
    pilgrim_up_from_non_spira_worktree(true, false);
}

#[test_id(tomp7s6)]
#[test]
#[ignore = "needs a release, SPIRA_TOML and dolt on PATH: run with --ignored"]
fn a_pilgrim_world_in_testenv_needs_no_sim_release_of_its_own() {
    pilgrim_up_from_non_spira_worktree(false, true);
}

fn refused_pilgrim_up(extra: &[&str], release: Option<&str>) -> (Option<i32>, String) {
    let t = testkit::TempDir::new("pw-refused");
    let mut cmd = std::process::Command::new(testkit::bin_exe("sim"));
    cmd.args(["world", "up"]).arg(t.join("w")).arg("--pilgrim").args(extra).env_clear().env("PATH", system_paths::SYSTEM_PATH_MIN);
    if let Some(r) = release {
        cmd.env("SPIRA_RELEASE", r);
    }
    let out = cmd.output().unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test_id(t1mx03w)]
#[test]
fn a_pilgrim_world_with_no_active_release_is_refused_naming_the_key() {
    let (code, err) = refused_pilgrim_up(&[], None);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("SPIRA_RELEASE is not set"), "{err}");
}

#[test_id(ti2vsxs)]
#[test]
fn a_pilgrim_world_takes_its_tree_from_the_release_so_tree_is_a_usage_error() {
    let (code, err) = refused_pilgrim_up(&["--tree", "HEAD"], Some("release"));
    assert_eq!(code, Some(2), "{err}");
}

mod landing {
    use spira_sim::pilgrim_land::{self, CargoTests, RoundRegistry, RunTests};
    use spira_sim::registryworld::Registry;
    use spira_sim::world::git;
    use std::path::Path;

    const AT_MS: u64 = 5_000_000;
    const LIB: &str = "pilgrim-fixture/crate/src/lib.rs";

    struct AtRegistry<'a>(&'a Registry);
    impl RoundRegistry for AtRegistry<'_> {
        fn deny_list(&self) -> Result<String, String> {
            let dir = self.0.pass_dir(AT_MS, "pilgrim")?;
            std::fs::read_to_string(dir.join(batcher_cut::vm::DenySnapshot::FILE)).map_err(|e| e.to_string())
        }
        fn activate(&self, test: &str, reason: &str) -> Result<(), String> {
            self.0.activate(AT_MS, test, reason)
        }
        fn clear_landed(&self, bead: &str) -> Result<Vec<String>, String> {
            self.0.clear_landed(AT_MS, &[bead.to_string()])
        }
    }

    fn world_repo(t: &testkit::TempDir) -> std::path::PathBuf {
        let work = t.join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "--initial-branch=main"]).unwrap();
        pilgrim_land::write_tree(&work).unwrap();
        git(&work, &["add", "-f", "."]).unwrap();
        git(&work, &["commit", "-q", "-m", "tree"]).unwrap();
        git(&work, &["branch", "local/main", "main"]).unwrap();
        work
    }

    fn commit_edit(work: &Path, bead: &str, from: &str, to: &str) {
        git(work, &["checkout", "-q", "-b", &format!("spira/{bead}"), "local/main"]).unwrap();
        let file = work.join(LIB);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(from), "the edit finds its line: {text}");
        std::fs::write(&file, text.replace(from, to)).unwrap();
        git(work, &["commit", "-q", "-am", &format!("{bead}: edit")]).unwrap();
        git(work, &["checkout", "-q", "main"]).unwrap();
    }

    #[test_id(t4lettf)]
    #[test]
    fn the_rounds_cargo_tests_run_no_compiler_wrapper_the_parent_names() {
        let t = testkit::TempDir::new("pw-wrapper");
        pilgrim_land::write_tree(t.path()).unwrap();
        std::fs::create_dir_all(t.join(".cargo")).unwrap();
        std::fs::write(t.join(".cargo/config.toml"), "[build]\nrustc-wrapper = \"false\"\nrustc-workspace-wrapper = \"false\"\n").unwrap();

        let run = CargoTests.run(t.path(), &[]).unwrap();

        assert!(run.green, "{}", run.output);
        let fixture_fn = pilgrim_land::stamped_tests(t.path()).into_iter().find(|(id, _)| id == pilgrim_land::FIXTURE_TEST).map(|(_, f)| f).unwrap();
        assert!(run.passed.contains(&fixture_fn), "{fixture_fn} in {:?}", run.passed);
    }

    fn history(reg: &Registry) -> String {
        reg.call(AT_MS, &["history", "--test", pilgrim_land::FIXTURE_TEST]).stdout
    }

    #[test_id(tkthwz5)]
    #[test]
    fn a_body_change_to_a_disabled_test_is_run_by_the_pilgrims_round_and_a_green_landing_activates_its_row() {
        let t = testkit::TempDir::new("pw-land");
        let work = world_repo(&t);
        let reg = Registry::new("7200").unwrap();
        reg.disable(1000, pilgrim_land::FIXTURE_TEST, "sp-owner", "flaky").unwrap();
        assert_eq!(reg.denied(AT_MS).unwrap(), [pilgrim_land::FIXTURE_TEST], "the row holds the test out before the round");
        commit_edit(&work, "sp-fixer", "assert_eq!(answer(), 42);", "assert_eq!(answer() + 1, 43);");

        let out = pilgrim_land::land(&work, "local/main", "sp-fixer", &AtRegistry(&reg), &CargoTests).unwrap();

        assert!(out.green, "{}", out.report());
        assert_eq!(out.ran, [pilgrim_land::FIXTURE_TEST], "{}", out.report());
        assert!(!out.skipped.contains(&pilgrim_land::FIXTURE_TEST.to_string()), "{}", out.report());
        assert_eq!(out.activated, [pilgrim_land::FIXTURE_TEST]);
        assert!(reg.denied(AT_MS).unwrap().is_empty(), "the row reads active");
        assert!(history(&reg).contains("activate"), "{}", history(&reg));
        assert_eq!(git(&work, &["rev-parse", "local/main"]).unwrap(), git(&work, &["rev-parse", "spira/sp-fixer"]).unwrap(), "the change is on the landing branch");
    }

    #[test_id(ti3ztuu)]
    #[test]
    fn a_red_round_lands_nothing_and_leaves_the_disabled_row_standing() {
        let t = testkit::TempDir::new("pw-land-red");
        let work = world_repo(&t);
        let reg = Registry::new("7200").unwrap();
        reg.disable(1000, pilgrim_land::FIXTURE_TEST, "sp-owner", "flaky").unwrap();
        commit_edit(&work, "sp-fixer", "assert_eq!(answer(), 42);", "assert_eq!(answer(), 41);");
        let before = git(&work, &["rev-parse", "local/main"]).unwrap();

        let out = pilgrim_land::land(&work, "local/main", "sp-fixer", &AtRegistry(&reg), &CargoTests).unwrap();

        assert!(!out.green && out.landed_at.is_none(), "{}", out.report());
        assert_eq!(reg.denied(AT_MS).unwrap(), [pilgrim_land::FIXTURE_TEST]);
        assert_eq!(git(&work, &["rev-parse", "local/main"]).unwrap(), before);
    }

    #[test_id(tqpi28w)]
    #[test]
    fn a_change_outside_the_disabled_tests_body_leaves_it_skipped_and_its_row_standing() {
        let t = testkit::TempDir::new("pw-land-other");
        let work = world_repo(&t);
        let reg = Registry::new("7200").unwrap();
        reg.disable(1000, pilgrim_land::FIXTURE_TEST, "sp-owner", "flaky").unwrap();
        commit_edit(&work, "sp-other", "pub fn answer() -> u32 {\n    42", "pub fn answer() -> u32 {\n    40 + 2");

        let out = pilgrim_land::land(&work, "local/main", "sp-other", &AtRegistry(&reg), &CargoTests).unwrap();

        assert!(out.green && out.ran.is_empty(), "{}", out.report());
        assert_eq!(out.skipped, [pilgrim_land::FIXTURE_TEST]);
        assert_eq!(reg.denied(AT_MS).unwrap(), [pilgrim_land::FIXTURE_TEST]);
    }

    #[test_id(t9w2o17)]
    #[test]
    fn a_landing_by_the_rows_own_bead_clears_it_with_a_cleared_landed_entry() {
        let t = testkit::TempDir::new("pw-land-own");
        let work = world_repo(&t);
        let reg = Registry::new("7200").unwrap();
        reg.disable(1000, pilgrim_land::FIXTURE_TEST, "sp-fixer", "flaky").unwrap();
        commit_edit(&work, "sp-fixer", "assert_eq!(answer(), 42);", "assert_eq!(answer() + 1, 43);");

        let out = pilgrim_land::land(&work, "local/main", "sp-fixer", &AtRegistry(&reg), &CargoTests).unwrap();

        assert!(out.green, "{}", out.report());
        assert_eq!(out.ran, [pilgrim_land::FIXTURE_TEST], "{}", out.report());
        assert_eq!(out.cleared, [pilgrim_land::FIXTURE_TEST], "{}", out.report());
        assert!(reg.denied(AT_MS).unwrap().is_empty(), "the row reads active");
        assert!(history(&reg).contains("cleared-landed"), "{}", history(&reg));
    }
}

#[test_id(t7reb5v)]
#[test]
#[ignore = "needs SPIRA_SIM_RELEASE, nix and a Dolt fixture: run with --ignored"]
fn a_pilgrim_claims_the_standing_bead_commits_on_its_branch_and_submits_it() {
    let release = std::env::var(world::RELEASE_ENV).unwrap_or_else(|_| panic!("{} is unset", world::RELEASE_ENV));
    assert!(Path::new(&release).is_dir(), "{release}");
    let t = testkit::TempDir::new("pw-submit");
    let repo = t.join("repo");
    world::fixture_repo(&testkit::manifest_dir().join("../.."), &repo).unwrap();
    let dir = t.join("w");
    let fixture = world::RepoSpec::parse(pilgrim::FIXTURE_REPO).unwrap();
    world::up_repos(&repo, &dir, "HEAD", &|k| std::env::var(k).ok(), &PILGRIM_STEPS, std::slice::from_ref(&fixture), &|_| Ok(())).unwrap();
    let dir = dir.canonicalize().unwrap();
    let _down = Up(&dir);
    pilgrim::up(&dir, pilgrim::FIXTURE, &PILGRIM_STEPS).unwrap();

    let bead = pilgrim::standing_bead(&dir).unwrap();
    pilgrim::claim(&dir, &bead).unwrap();
    let tree = t.join("fixture-tree");
    let checkout = fixture.work(&dir);
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").arg("-C").arg(&tree).args(["-c", "user.name=pilgrim", "-c", "user.email=p@sim.invalid"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    assert!(std::process::Command::new("git").arg("-C").arg(&checkout).args(["worktree", "add", "-q", "-b", &format!("spira/{bead}")]).arg(&tree).arg("origin/main").status().unwrap().success());
    std::fs::write(tree.join("walked.txt"), "the pilgrim was here\n").unwrap();
    git(&["add", "walked.txt"]);
    git(&["commit", "-q", "-m", &format!("{bead}: the pilgrim's change")]);

    let session = [("SPIRA_WORK_BEAD_ID", bead.as_str()), ("SPIRA_RUNE", "pilgrim")];
    let (code, out) = pilgrim::work_bound(&dir, &session, &tree, "submit").unwrap();
    assert_eq!(code, 0, "{out}");
    let (code, shown) = pilgrim::work_bound(&dir, &session, &tree, "show").unwrap();
    assert_eq!(code, 0, "{shown}");
    assert!(shown.contains("LIFECYCLE: SUBMITTED"), "{shown}");
}
