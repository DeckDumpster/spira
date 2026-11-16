//! The pilgrim world, walked: file A, link A on B, see A blocked in both views, close B, see A
//! claimable in both. The walk needs a real world (a Dolt fixture and a release), which
//! `SPIRA_SIM_RELEASE` names, so it is `#[ignore]`d and run with `--ignored` where one exists.

use spira_sim::pilgrim;
use spira_sim::world::{self, lc_socket, ProcessSteps};
use std::path::Path;

struct Up<'a>(&'a Path);
impl Drop for Up<'_> {
    fn drop(&mut self) {
        let _ = world::down(self.0, &ProcessSteps);
    }
}

#[test]
fn the_gates_socket_is_the_socket_the_marker_sits_beside_and_the_serve_listens_on() {
    let w = Path::new("/tmp/pw");
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

#[test]
fn a_marker_is_refused_beside_any_socket_that_is_not_a_sim_worlds() {
    let t = testkit::TempDir::new("pw-guard");
    assert!(world::make_pilgrim_world(t.path()).is_err());
    assert!(!Path::new(&spira_store::lc_call::pilgrim_world_marker(
        &lc_socket(t.path()).display().to_string()
    ))
    .exists());
}

#[test]
#[ignore = "needs SPIRA_SIM_RELEASE and a Dolt fixture: run with --ignored"]
fn a_pilgrim_files_links_and_closes_in_a_world_of_its_own_and_both_views_follow() {
    let release = std::env::var(world::RELEASE_ENV)
        .unwrap_or_else(|_| panic!("{} is unset", world::RELEASE_ENV));
    assert!(Path::new(&release).is_dir(), "{release}");
    let t = testkit::TempDir::new("pw-walk");
    let repo = t.join("repo");
    world::fixture_repo(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."), &repo).unwrap();
    let dir = t.join("w");
    world::up_repos(
        &repo,
        &dir,
        "HEAD",
        &|k| std::env::var(k).ok(),
        &ProcessSteps,
        &[],
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
        &ProcessSteps,
        &[],
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
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
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

    pilgrim::up(&dir, pilgrim::FIXTURE, &ProcessSteps).unwrap();
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

    pilgrim::close(&dir, &b, "walked").unwrap();
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
}

#[test]
#[ignore = "needs SPIRA_SIM_RELEASE, SPIRA_TOML naming spira.bd, and a Dolt fixture: run with --ignored"]
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
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("SPIRA_RELEASE", &release)
        .env(world::RELEASE_ENV, &release)
        .env("SPIRA_TOML", toml)
        .env("SPIRA_LC_SOCKET", t.join("production.sock"))
        .output()
        .unwrap();
    let dir = dir.canonicalize();
    if let Ok(d) = &dir {
        let _ = world::down(d, &ProcessSteps);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(!err.contains("bd not found"), "{err}");
}
