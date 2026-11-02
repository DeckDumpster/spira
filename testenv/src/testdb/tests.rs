use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn tmpdir(tag: &str) -> testkit::TempDir {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    testkit::TempDir::new(&format!("testenv-testdb-{tag}-{n}"))
}

// ---- pure ----------------------------------------------------------------------------

#[test]
fn template_key_changes_with_either_binary() {
    let k = template_key("bd\u{0}/a\u{0}1\u{0}2", "dolt\u{0}/b\u{0}1\u{0}2");
    assert_eq!(k.len(), 16);
    assert_eq!(k, template_key("bd\u{0}/a\u{0}1\u{0}2", "dolt\u{0}/b\u{0}1\u{0}2"));
    assert_ne!(k, template_key("bd\u{0}/a\u{0}1\u{0}3", "dolt\u{0}/b\u{0}1\u{0}2"));
    assert_ne!(k, template_key("bd\u{0}/a\u{0}1\u{0}2", "dolt\u{0}/c\u{0}1\u{0}2"));
}

#[test]
fn exe_identity_names_path_size_and_mtime() {
    let d = tmpdir("ident");
    let p = d.join("x");
    fs::write(&p, "abc").unwrap();
    let id = exe_identity(&p).unwrap();
    assert!(id.starts_with(&format!("{}\u{0}3\u{0}", p.display())));
    assert!(exe_identity(&d.join("missing")).is_err());
}

#[test]
fn config_carries_port_and_absolute_data_dir() {
    let c = render_config(4242, Path::new("/var/tmp/fx/data"));
    assert!(c.contains("  port: 4242\n"));
    assert!(c.contains("  host: 127.0.0.1\n"));
    assert!(c.contains("data_dir: \"/var/tmp/fx/data\"\n"));
    assert!(c.contains("dolt_transaction_commit: false"));
}

#[test]
fn beads_pointed_at_new_port_in_both_files() {
    let d = tmpdir("point");
    let b = d.join(".beads");
    fs::create_dir_all(&b).unwrap();
    fs::write(
        b.join("metadata.json"),
        r#"{"dolt_server_port": 13901, "dolt_database": "sptest", "project_id": "p"}"#,
    )
    .unwrap();
    fs::write(b.join("dolt-server.port"), "13901").unwrap();
    point_beads_at(&b, 40001).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(b.join("metadata.json")).unwrap()).unwrap();
    assert_eq!(v["dolt_server_port"], 40001);
    assert_eq!(v["project_id"], "p", "other keys are kept");
    assert_eq!(
        fs::read_to_string(b.join("dolt-server.port")).unwrap(),
        "40001"
    );
    fs::write(b.join("metadata.json"), "[1]").unwrap();
    assert!(
        point_beads_at(&b, 1).is_err(),
        "a non-object is refused, not clobbered"
    );
}

#[test]
fn cmdline_identity_is_sql_server_for_this_config() {
    let cfg = Path::new("/var/tmp/fx-a/data/config.yaml");
    assert!(cmdline_is_server(
        b"/usr/local/bin/dolt\0sql-server\0--config\0/var/tmp/fx-a/data/config.yaml\0",
        cfg
    ));
    assert!(
        !cmdline_is_server(
            b"dolt\0sql-server\0--config\0/var/tmp/fx-b/data/config.yaml\0",
            cfg
        ),
        "another fixture's server"
    );
    assert!(
        !cmdline_is_server(b"vim\0/var/tmp/fx-a/data/config.yaml\0", cfg),
        "not a server"
    );
    assert!(!cmdline_is_server(b"", cfg), "gone");
}

#[test]
fn zombie_and_missing_state_read_as_dead() {
    assert!(!status_is_dead("Name:\tdolt\nState:\tS (sleeping)\n"));
    assert!(!status_is_dead("State:\tR (running)\n"));
    assert!(status_is_dead("Name:\tdolt\nState:\tZ (zombie)\n"));
    assert!(status_is_dead("State:\tX (dead)\n"));
    assert!(status_is_dead(""));
}

#[test]
fn cpu_ticks_read_past_a_comm_with_spaces() {
    let stat =
        "123 (dolt sql server) S 1 123 123 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 12 0 99 1 2";
    assert_eq!(stat_cpu_ticks(stat), Some(300));
    assert_eq!(stat_cpu_ticks("garbage"), None);
}

#[test]
fn report_is_key_value_lines() {
    assert_eq!(
        report(&[("A", "1".into()), ("B", "x y".into())]),
        "A=1\nB=x y\n"
    );
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn parse_requires_what_each_command_needs() {
    let a = parse(&s(&[
        "up", "--tag", "poison", "--bd", "bd", "--dolt", "dolt", "--owner", "42",
    ]))
    .unwrap();
    assert_eq!(
        (a.cmd.as_str(), a.tag.as_deref(), a.owner),
        ("up", Some("poison"), Some(42))
    );
    assert!(
        parse(&s(&["up", "--bd", "bd", "--dolt", "dolt"])).is_err(),
        "up needs --tag"
    );
    assert!(
        parse(&s(&["up", "--tag", "a b", "--bd", "bd", "--dolt", "d"])).is_err(),
        "tag is a name, not a phrase"
    );
    assert!(
        parse(&s(&["template", "--bd", "bd"])).is_err(),
        "template needs --dolt"
    );
    assert!(parse(&s(&["reset"])).is_err());
    assert!(parse(&s(&["down", "--fixture", "/x"])).is_ok());
    assert!(
        parse(&s(&["reap", "--fixture", "/x"])).is_err(),
        "reap needs --owner"
    );
    assert!(parse(&s(&["up", "--owner", "me"])).is_err());
    assert!(parse(&s(&["bogus"])).is_err());
    assert!(parse(&s(&[])).is_err());
    assert!(
        parse(&s(&["down", "--fixture"])).is_err(),
        "a flag without its value"
    );
}

// ---- orchestration against stand-ins -------------------------------------------------

#[test]
fn the_template_command_takes_its_cache_from_the_flag_or_else_the_batch_variable() {
    let parsed = |v: &[&str]| parse(&s(v)).unwrap();
    let bare = parsed(&["template", "--bd", "bd", "--dolt", "dolt"]);
    assert_eq!(cache_of(&bare, None), None);
    assert_eq!(cache_of(&bare, Some(String::new())), None, "empty is no cache");
    assert_eq!(cache_of(&bare, Some("/c/env".into())), Some(PathBuf::from("/c/env")));
    let flagged = parsed(&["template", "--bd", "bd", "--dolt", "dolt", "--cache", "/c/flag"]);
    assert_eq!(cache_of(&flagged, Some("/c/env".into())), Some(PathBuf::from("/c/flag")));
}

fn fake_template(dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst.join("data")).map_err(|e| e.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    let secret = dst.join("data/owner-only");
    fs::write(&secret, "x").map_err(|e| e.to_string())?;
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    fs::write(dst.join("READY"), "").map_err(|e| e.to_string())
}

fn cached_names(cache: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(cache)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn the_template_cache_is_bounded_and_evicts_the_least_recently_used_key() {
    let d = tmpdir("cachebound");
    let cache = d.join("cache");
    for k in ["k0", "k1", "k2", "k3"] {
        assert!(cache_template(&cache, k, 4, fake_template).unwrap().is_empty());
    }
    // Ages set outright (a second's mtime resolution would tie them), then k0 used again.
    let base = SystemTime::now() - Duration::from_secs(600);
    for (i, k) in ["k0", "k1", "k2", "k3"].iter().enumerate() {
        fs::File::open(cache.join(format!("template-{k}")))
            .unwrap()
            .set_modified(base + Duration::from_secs(i as u64))
            .unwrap();
    }
    touch_template(&cache, "k0");
    // Litter from a killed store: old goes, fresh (a store in flight) stays.
    fs::create_dir_all(cache.join(".incoming-old-1-0")).unwrap();
    fs::File::open(cache.join(".incoming-old-1-0"))
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(7200))
        .unwrap();
    fs::create_dir_all(cache.join(".incoming-live-1-0")).unwrap();

    let gone = cache_template(&cache, "k4", 4, fake_template).unwrap();
    assert_eq!(gone, ["template-k1"]);
    assert_eq!(
        cached_names(&cache),
        [".incoming-live-1-0", "template-k0", "template-k2", "template-k3", "template-k4"]
    );
    // A stored template is readable by the container's other user, owner bits kept.
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(cache.join("template-k4/data/owner-only"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o644);
    // Storing a key already cached fetches nothing.
    let gone = cache_template(&cache, "k4", 4, |_| panic!("fetched a cached key")).unwrap();
    assert!(gone.is_empty());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_failed_fetch_stores_nothing() {
    let d = tmpdir("cachefail");
    let cache = d.join("cache");
    let e = cache_template(&cache, "k", 4, |dst| {
        fs::create_dir_all(dst).unwrap();
        Err("podman cp rc=125".into())
    })
    .unwrap_err();
    assert!(e.contains("rc=125"), "{e}");
    let e = cache_template(&cache, "k", 4, |dst| {
        fs::create_dir_all(dst).map_err(|e| e.to_string())
    })
    .unwrap_err();
    assert!(e.contains("READY"), "{e}");
    assert!(cached_names(&cache).is_empty(), "{:?}", cached_names(&cache));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_template_report_round_trips() {
    let out = report(&[
        ("TESTDB_TEMPLATE", "/tmp/spira-testdb/template-ab".into()),
        ("TESTDB_TEMPLATE_KEY", "ab".into()),
        ("TESTDB_TEMPLATE_ORIGIN", Origin::Built.word().into()),
    ]);
    assert_eq!(
        TemplateReport::parse(&format!("noise\n{out}")),
        Some(TemplateReport {
            path: "/tmp/spira-testdb/template-ab".into(),
            key: "ab".into(),
            origin: Origin::Built
        })
    );
    assert_eq!(TemplateReport::parse("TESTDB_TEMPLATE=/x\n"), None);
}

// fixtures_are_isolated_reset_is_fresh_and_down_removes_all was deleted under
// law-a-test-that-flips-is-deleted: r-auto-147's recert (2026-10-15) failed it at
// `assert!(!pid_alive(pid2))` with a member that touches only round-vm/src/pool.rs —
// red once in the last 34 unit runs.

#[test]
fn reaper_exits_when_the_fixture_was_dropped_normally() {
    let d = tmpdir("reap-gone");
    let fx = d.join("fx-gone");
    // Owner alive (this process), fixture absent: returns at once rather than polling forever.
    reap(&fx, std::process::id(), Duration::from_millis(5));
    reap(&fx, 0, Duration::from_millis(5));
    let _ = fs::remove_dir_all(&d);
}

// ---- the real thing (bd + dolt), run with --ignored ----------------------------------

fn bd_in(ws: &Path, bd: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(bd)
        .arg("-C")
        .arg(ws)
        .args(args)
        .output()
        .unwrap();
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

#[test]
#[ignore = "needs real bd + dolt on PATH; ~6 s (template build)"]
fn real_bd_and_dolt_isolated_fixtures() {
    let tools = Tools::resolve("bd", "dolt").expect("bd and dolt on PATH");
    let d = tmpdir("real");
    let root = d.join("root");
    let t0 = Instant::now();
    let tpl = ensure_template(&root, &tools).unwrap();
    eprintln!("template: {} ms", t0.elapsed().as_millis());
    let a = up(&root, &tpl, &tools, "ra", 0, None).unwrap();
    let b = up(&root, &tpl, &tools, "rb", 0, None).unwrap();
    eprintln!("up: {} ms, {} ms", a.ms, b.ms);
    let (ok, out) = bd_in(&a.ws, &tools.bd, &["create", "only in A", "-q"]);
    assert!(ok, "{out}");
    let (_, la) = bd_in(&a.ws, &tools.bd, &["list"]);
    let (_, lb) = bd_in(&b.ws, &tools.bd, &["list"]);
    assert!(la.contains("only in A"), "{la}");
    assert!(!lb.contains("only in A"), "{lb}");
    let (ok, out) = bd_in(&a.ws, &tools.bd, &["sql", "SELECT COUNT(*) FROM issues"]);
    assert!(ok, "bd sql works in server mode: {out}");
    let (_, _, ms) = reset(&a.fixture).unwrap();
    eprintln!("reset: {ms} ms");
    let (_, la) = bd_in(&a.ws, &tools.bd, &["list"]);
    assert!(!la.contains("only in A"), "reset is fresh: {la}");
    let dn = down(&a.fixture).unwrap();
    eprintln!("down: cpu_ms={:?} life_ms={:?}", dn.cpu_ms, dn.life_ms);
    down(&b.fixture).unwrap();
    assert!(!a.fixture.exists() && !b.fixture.exists());
    let _ = fs::remove_dir_all(&d);
}

// ---- readiness and the tmpfs root (sp-t26yx) -----------------------------------------------

#[test]
fn a_greeting_is_protocol_10_at_sequence_0() {
    assert!(is_greeting(&[0x4a, 0, 0, 0, 0x0a]));
    assert!(!is_greeting(&[0x17, 0, 0, 0, 0xff]), "an error packet is not ready");
    assert!(!is_greeting(&[0x4a, 0, 0, 1, 0x0a]), "not the first packet");
    assert!(!is_greeting(&[0, 0, 0, 0, 0x0a]), "an empty payload");
}

#[test]
fn a_greeting_socket_is_ready() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let srv = std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        c.write_all(&[5, 0, 0, 0, 0x0a, b'8', b'.', b'0', 0]).unwrap();
    });
    assert!(greets(&addr));
    srv.join().unwrap();
}

#[test]
fn nothing_listening_is_not_ready() {
    let port = free_port().unwrap();
    assert!(!greets(&([127, 0, 0, 1], port).into()));
}

#[test]
fn tmpfs_is_read_from_the_nearest_existing_ancestor() {
    assert!(!on_tmpfs(Path::new("/proc")), "procfs is not a tmpfs");
    let shm = Path::new("/dev/shm");
    if on_tmpfs(shm) {
        assert!(on_tmpfs(&shm.join("sp-t26yx-absent/deeper")));
    } else {
        eprintln!("skip: /dev/shm is not a tmpfs here");
    }
}

// ---- embedded fixtures (sp-k2oyn) -----------------------------------------------------

fn write_beads(dir: &Path, marker: &str) {
    let beads = dir.join(".beads");
    fs::create_dir_all(&beads).unwrap();
    fs::write(beads.join("metadata.json"), marker).unwrap();
}

#[test]
fn unique_dir_never_repeats_within_a_process() {
    let a = unique_dir("u");
    let b = unique_dir("u");
    assert_ne!(a, b);
}

#[test]
fn embedded_reset_restores_the_baseline_and_drops_extra_state() {
    let d = tmpdir("er");
    let baseline = d.join("baseline");
    write_beads(&baseline, "baseline");
    let fx = d.join("fx");
    write_beads(&fx, "live-and-changed");
    fs::write(fx.join(".beads/extra-table.json"), "state a fresh fixture never had").unwrap();

    embedded_reset(&fx, &baseline).unwrap();

    assert_eq!(
        fs::read_to_string(fx.join(".beads/metadata.json")).unwrap(),
        "baseline",
        "reset content matches the baseline, not the live state"
    );
    assert!(
        !fx.join(".beads/extra-table.json").exists(),
        "reset drops state the baseline never had"
    );
    assert!(!fx.join(".beads.new").exists());
    assert!(!fx.join(".beads.old").exists(), "the rename dance cleans up after itself");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn embedded_reset_is_a_swap_even_when_beads_is_absent() {
    // A fixture whose .beads was already removed (e.g. a prior interrupted reset) still
    // resets: the `mv` of a missing .beads is a documented no-op, not a failure.
    let d = tmpdir("er-absent");
    let baseline = d.join("baseline");
    write_beads(&baseline, "baseline");
    let fx = d.join("fx");
    fs::create_dir_all(&fx).unwrap();

    embedded_reset(&fx, &baseline).unwrap();
    assert_eq!(
        fs::read_to_string(fx.join(".beads/metadata.json")).unwrap(),
        "baseline"
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn embedded_borrow_gets_its_own_copy_never_the_baseline_itself() {
    let d = tmpdir("bor");
    let baseline = d.join("baseline");
    write_beads(&baseline, "shared baseline");

    let b1 = embedded_borrow(&baseline).unwrap();
    let b2 = embedded_borrow(&baseline).unwrap();
    assert_ne!(b1.dir, b2.dir, "two borrowers never share a directory");
    assert_eq!(
        fs::read_to_string(b1.dir.join(".beads/metadata.json")).unwrap(),
        "shared baseline"
    );
    fs::write(b1.dir.join(".beads/metadata.json"), "borrower 1 wrote this").unwrap();
    assert_eq!(
        fs::read_to_string(baseline.join(".beads/metadata.json")).unwrap(),
        "shared baseline",
        "a borrower's write never reaches the shared baseline"
    );
    assert_eq!(
        fs::read_to_string(b2.dir.join(".beads/metadata.json")).unwrap(),
        "shared baseline",
        "and never reaches another borrower either"
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn embedded_borrow_refuses_a_gone_baseline() {
    let d = tmpdir("bor-gone");
    let err = embedded_borrow(&d.join("never-existed")).unwrap_err();
    assert!(err.contains("could not copy baseline"), "{err}");
}

#[test]
fn embedded_borrow_refuses_a_baseline_with_no_beads() {
    let d = tmpdir("bor-empty");
    let baseline = d.join("baseline");
    fs::create_dir_all(&baseline).unwrap();
    let err = embedded_borrow(&baseline).unwrap_err();
    assert!(err.contains("could not copy baseline"), "{err}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn embedded_down_tolerates_empty_and_missing_paths() {
    // Server mode zeroes these vars before calling drop's shared cleanup path; embedded-down
    // must be a no-op, not a failure, on an empty string or a path that is already gone.
    embedded_down(&[PathBuf::new(), PathBuf::from("/does/not/exist")]);
}

#[test]
#[ignore = "needs real bd (embedded engine) on PATH"]
fn real_bd_embedded_up_check_reset_and_down() {
    let path = std::env::var("PATH").unwrap_or_default();
    let Some(bd) = resolve_exe("bd-embedded", &path) else {
        eprintln!("skip: no bd-embedded on PATH");
        return;
    };
    let bd_s = bd.display().to_string();
    assert!(embedded_check(&bd_s), "real bd-embedded should pass the probe");

    let u = embedded_up(&bd_s, "real").unwrap();
    let real_bd = u.bin.as_ref().unwrap().join("bd");
    let out = Command::new(&real_bd)
        .args(["-C", &u.dir.display().to_string(), "create", "sp-k2oyn real check", "-q"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    embedded_reset(&u.dir, &u.baseline).unwrap();
    let out = Command::new(&real_bd)
        .args(["-C", &u.dir.display().to_string(), "list", "--json"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("sp-k2oyn real check"),
        "reset must clear what was created after the baseline"
    );

    let mut paths = vec![u.dir.clone(), u.baseline.clone()];
    if let Some(b) = &u.bin {
        paths.push(b.clone());
    }
    embedded_down(&paths);
    let t0 = Instant::now();
    while paths.iter().any(|p| p.exists()) && t0.elapsed() < Duration::from_secs(5) {
        sleep(Duration::from_millis(20));
    }
    for p in &paths {
        assert!(!p.exists(), "{} should be gone", p.display());
    }
}
