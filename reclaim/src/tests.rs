use super::*;

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

#[test_id(t1wk95a)]
#[test]
fn the_cache_is_chosen_oldest_first_to_its_cap_and_fresh_tmp_is_never_a_candidate() {
    let t = testkit::TempDir::new("reclaim-g");
    let dir = t.path().join("cache/ab");
    fs::create_dir_all(&dir).unwrap();
    for (n, a) in [("old", 5000u64), ("mid", 3000), ("new", 100)] {
        fs::write(dir.join(n), vec![1u8; 65536]).unwrap();
        age(&dir.join(n), a);
    }
    fs::write(dir.join("up.tmp"), vec![1u8; 65536]).unwrap();
    let files = cache_files(&t.path().join("cache"), now());
    assert!(files.iter().all(|f| f.2 != dir.join("up.tmp")));
    let chosen = trim_selection(&files, 100_000);
    assert_eq!(chosen, [dir.join("old"), dir.join("mid")]);
}

const HOUR: u64 = 3600;

fn tmp_dir(root: &Path, name: &str, secs: u64) -> PathBuf {
    tgt(root, name, secs)
}

fn verdict_of(plan: &[Candidate], d: &Path) -> String {
    let c = plan.iter().find(|c| c.path == d).unwrap_or_else(|| panic!("{d:?} is not a candidate"));
    format!("{:?}", c.verdict)
}

#[test_id(tpq6qfj)]
#[test]
fn tmp_plan_reclaims_an_idle_unheld_dir_and_skips_held_young_socket_and_symlink() {
    let t = testkit::TempDir::new("reclaim-tmp-a");
    let root = t.path().join("tmp");
    fs::create_dir_all(&root).unwrap();
    let idle = tmp_dir(&root, "tmp.idle", 9 * HOUR);
    let held = tmp_dir(&root, "tmp.held", 9 * HOUR);
    let young = tmp_dir(&root, "tmp.young", 60);
    let sock = tmp_dir(&root, "tmp-1000", 9 * HOUR);
    let _l = std::os::unix::net::UnixListener::bind(sock.join("default")).unwrap();
    let link = root.join("tmp.link");
    std::os::unix::fs::symlink(&idle, &link).unwrap();
    fs::write(root.join("plain-file"), vec![1u8; 8192]).unwrap();
    age(&root.join("plain-file"), 9 * HOUR);

    let plan = tmp_plan(&root, own_uid(), now(), 6 * HOUR, &[held.join("artifact")]);
    assert_eq!(verdict_of(&plan, &idle), "Reclaim");
    assert_eq!(verdict_of(&plan, &held), r#"Skip("held by a live process")"#);
    assert!(verdict_of(&plan, &young).starts_with("Skip(\"touched"));
    assert_eq!(verdict_of(&plan, &sock), r#"Skip("holds a socket or fifo")"#);
    assert_eq!(verdict_of(&plan, &link), r#"Skip("symlink")"#);
    assert!(plan.iter().all(|c| c.path != root.join("plain-file")));
}

#[test_id(twe791v)]
#[test]
fn tmp_plan_leaves_a_dir_another_uid_owns() {
    let t = testkit::TempDir::new("reclaim-tmp-uid");
    tmp_dir(t.path(), "tmp.mine", 9 * HOUR);
    assert!(tmp_plan(t.path(), own_uid().wrapping_add(1), now(), 6 * HOUR, &[]).is_empty());
}

#[test_id(t35r0na)]
#[test]
fn an_unreadable_subdirectory_keeps_its_tmp_dir() {
    use std::os::unix::fs::PermissionsExt;
    let t = testkit::TempDir::new("reclaim-tmp-unreadable");
    let d = tmp_dir(t.path(), "tmp.opaque", 9 * HOUR);
    let sub = d.join("sub");
    fs::create_dir(&sub).unwrap();
    let orig = fs::metadata(&sub).unwrap().permissions();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();
    age(&d, 9 * HOUR);
    let plan = tmp_plan(t.path(), own_uid(), now(), 6 * HOUR, &[]);
    fs::set_permissions(&sub, orig).unwrap();
    assert_eq!(verdict_of(&plan, &d), r#"Skip("holds a socket or fifo")"#);
}
