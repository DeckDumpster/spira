use super::*;
use std::cell::Cell;

const DAY: u64 = 86400;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn age(p: &Path, secs: u64) {
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    fs::File::open(p).unwrap().set_modified(t).unwrap();
}

fn tgt(root: &Path, rel: &str, secs: u64) -> PathBuf {
    let d = root.join(rel);
    fs::create_dir_all(&d).unwrap();
    let f = d.join("artifact");
    fs::write(&f, vec![7u8; 8192]).unwrap();
    age(&f, secs);
    age(&d, secs);
    d
}

fn cfg(root: &Path) -> Config {
    Config {
        patterns: vec![format!("{}/worktree/.tgt-*", root.display()), format!("{}/tgt-*", root.display()), format!("{}/worktree/*/target", root.display())],
        require: vec![],
        lease_dir: Some(root.join("aeon")),
        idle_secs: 2 * DAY,
        min_free_mib: 100,
        target_free_mib: 200,
        cache_dir: None,
        cache_cap_bytes: 0,
        dry: false,
    }
}

fn go(c: &Config, held: &[PathBuf], free: u64) -> (Result<u64, String>, Vec<String>) {
    let mut log = vec![];
    let r = run(c, now(), held, &|| Some(free), &mut |l| log.push(l));
    (r, log)
}

#[test]
fn stale_unheld_targets_go_and_fresh_held_leased_stay() {
    let t = testkit::TempDir::new("reclaim-a");
    let root = t.path();
    let stale = tgt(root, "worktree/.tgt-old", 5 * DAY);
    let stale2 = tgt(root, "tgt-concierge-x", 6 * DAY);
    let stale_wt = tgt(root, "worktree/sp-gone/target", 7 * DAY);
    let fresh = tgt(root, "worktree/.tgt-fresh", 60);
    let held = tgt(root, "worktree/.tgt-held", 9 * DAY);
    let live_wt = tgt(root, "worktree/sp-live/target", 9 * DAY);
    fs::create_dir_all(root.join("aeon")).unwrap();
    fs::write(root.join("aeon/sp-live.lease"), "").unwrap();
    let (r, log) = go(&cfg(root), &[held.join("artifact")], 10);
    assert!(r.is_ok(), "{log:?}");
    for gone in [&stale, &stale2, &stale_wt] {
        assert!(!gone.exists(), "{gone:?} should be reclaimed: {log:?}");
    }
    for kept in [&fresh, &held, &live_wt] {
        assert!(kept.exists(), "{kept:?} should survive: {log:?}");
    }
}

#[test]
fn a_worktree_a_process_stands_in_protects_its_target() {
    let t = testkit::TempDir::new("reclaim-b");
    let wt_target = tgt(t.path(), "worktree/sp-busy/target", 9 * DAY);
    let (_, log) = go(&cfg(t.path()), &[t.path().join("worktree/sp-busy/src")], 10);
    assert!(wt_target.exists(), "{log:?}");
}

#[test]
fn nothing_is_touched_above_the_trigger() {
    let t = testkit::TempDir::new("reclaim-c");
    let stale = tgt(t.path(), "worktree/.tgt-old", 9 * DAY);
    let (r, _) = go(&cfg(t.path()), &[], 5000);
    assert_eq!(r, Ok(0));
    assert!(stale.exists());
}

#[test]
fn it_stops_once_the_target_free_space_is_reached() {
    let t = testkit::TempDir::new("reclaim-d");
    let oldest = tgt(t.path(), "worktree/.tgt-a", 9 * DAY);
    let newer = tgt(t.path(), "worktree/.tgt-b", 4 * DAY);
    let free = Cell::new(10u64);
    let mut log = vec![];
    let c = cfg(t.path());
    run(&c, now(), &[], &|| Some(free.get()), &mut |l| {
        if l.starts_with("reclaimed") {
            free.set(500);
        }
        log.push(l)
    })
    .unwrap();
    assert!(!oldest.exists() && newer.exists(), "{log:?}");
}

#[test]
fn a_control_that_matches_nothing_refuses_before_removing_anything() {
    let t = testkit::TempDir::new("reclaim-e");
    let stale = tgt(t.path(), "worktree/.tgt-old", 9 * DAY);
    let mut c = cfg(t.path());
    c.require = vec![format!("{}/worktree/sp-*", t.path().display())];
    let (r, _) = go(&c, &[], 10);
    assert!(r.unwrap_err().contains("positive control"));
    assert!(stale.exists());
    fs::create_dir_all(t.path().join("worktree/sp-known")).unwrap();
    assert!(go(&c, &[], 10).0.is_ok());
    assert!(!stale.exists());
}

#[test]
fn dry_run_removes_nothing_but_reports() {
    let t = testkit::TempDir::new("reclaim-f");
    let stale = tgt(t.path(), "tgt-x", 9 * DAY);
    let mut c = cfg(t.path());
    c.dry = true;
    let (r, log) = go(&c, &[], 10);
    assert!(r.unwrap() > 0 && stale.exists() && log.iter().any(|l| l.contains("(dry)")), "{log:?}");
}

#[test]
fn the_cache_is_trimmed_oldest_first_to_its_cap_and_fresh_tmp_survives() {
    let t = testkit::TempDir::new("reclaim-g");
    let dir = t.path().join("cache/ab");
    fs::create_dir_all(&dir).unwrap();
    for (n, a) in [("old", 5000u64), ("mid", 3000), ("new", 100)] {
        fs::write(dir.join(n), vec![1u8; 65536]).unwrap();
        age(&dir.join(n), a);
    }
    fs::write(dir.join("up.tmp"), vec![1u8; 65536]).unwrap();
    let (n, _) = trim_cache(&t.path().join("cache"), 100_000, now(), false);
    assert_eq!(n, 2);
    assert!(!dir.join("old").exists() && !dir.join("mid").exists());
    assert!(dir.join("new").exists() && dir.join("up.tmp").exists());
}

#[test]
fn glob_matches_wildcards_and_finds_nothing_for_a_missing_directory() {
    assert!(wild(".tgt-*", ".tgt-final") && !wild(".tgt-*", "tgt-final") && wild("a*c*e", "abcde"));
    assert!(glob("/nonexistent-reclaim/*/target").is_empty());
}
