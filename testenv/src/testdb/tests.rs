use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn tmpdir(tag: &str) -> testkit::TempDir {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    testkit::TempDir::new(&format!("testenv-testdb-{tag}-{n}"))
}

/// testkit::write_exe, never write + chmod: see testkit/DESIGN.md (ETXTBSY).
fn exe(p: &Path, text: &str) {
    testkit::write_exe(p, text);
}

/// A stand-in `dolt`: `sql-server --config C` listens on C's port until SIGTERM.
const FAKE_DOLT: &str = r#"#!/usr/bin/env python3
import re, signal, socket, sys
cfg = open(sys.argv[sys.argv.index("--config") + 1]).read()
port = int(re.search(r"port: (\d+)", cfg).group(1))
signal.signal(signal.SIGTERM, lambda *a: sys.exit(0))
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(64)
while True:
    c, _ = s.accept()
    c.sendall(bytes([5, 0, 0, 0, 0x0a]) + b"8.0\x00"); c.close()
"#;

/// A stand-in `bd init --server`: writes the .beads files bd writes, counts its runs.
fn fake_bd(dir: &Path) -> PathBuf {
    let p = dir.join("bd");
    exe(
        &p,
        &format!(
            "#!/usr/bin/env bash\nset -e\necho run >> {}/bd-inits\nport=\nwhile [ $# -gt 0 ]; do [ \"$1\" = --server-port ] && port=$2; shift; done\nmkdir -p .beads\nprintf '{{\"backend\":\"dolt\",\"dolt_mode\":\"server\",\"dolt_server_port\":%s,\"dolt_database\":\"sptest\"}}' \"$port\" > .beads/metadata.json\nprintf %s \"$port\" > .beads/dolt-server.port\n",
            dir.display()
        ),
    );
    p
}

fn fakes(tag: &str) -> Option<(testkit::TempDir, Tools)> {
    if resolve_exe("python3", &std::env::var("PATH").unwrap_or_default()).is_none() {
        eprintln!("skip: no python3 for the stand-in dolt");
        return None;
    }
    let d = tmpdir(tag);
    let dolt = d.join("dolt");
    exe(&dolt, FAKE_DOLT);
    let bd = fake_bd(&d);
    Some((d, Tools { bd, dolt }))
}

fn port_of(beads: &Path) -> u16 {
    read_trim(&beads.join("dolt-server.port"))
        .unwrap()
        .parse()
        .unwrap()
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

#[test]
fn resolve_exe_searches_path_and_takes_paths_as_given() {
    let d = tmpdir("resolve");
    exe(&d.join("tool"), "#!/bin/sh\n");
    let pv = format!("/nonexistent:{}", d.display());
    assert_eq!(
        resolve_exe("tool", &pv),
        Some(fs::canonicalize(d.join("tool")).unwrap())
    );
    assert_eq!(resolve_exe("missing", &pv), None);
    assert_eq!(
        resolve_exe(&d.join("tool").display().to_string(), ""),
        Some(fs::canonicalize(d.join("tool")).unwrap())
    );
}

#[test]
fn a_bd_that_is_the_meter_resolves_to_the_real_bd_behind_it() {
    // A parallel suite's PATH: $HOME/.local/bin/bd -> <artifacts>/bd-meter, then the real bd.
    let d = tmpdir("meter");
    let art = d.join("artifacts");
    let home_bin = d.join("home-bin");
    let sys = d.join("sys");
    for p in [&art, &home_bin, &sys] {
        fs::create_dir_all(p).unwrap();
    }
    exe(&art.join("bd-meter"), "#!/bin/sh\n");
    std::os::unix::fs::symlink(art.join("bd-meter"), home_bin.join("bd")).unwrap();
    exe(&sys.join("bd"), "#!/bin/sh\n");
    let pv = format!("{}:{}", home_bin.display(), sys.display());
    let real = fs::canonicalize(sys.join("bd")).unwrap();

    let metered = resolve_exe("bd", &pv).unwrap();
    assert!(
        metered.ends_with("bd-meter"),
        "plant: bd resolves to the meter first"
    );
    assert_eq!(see_through_meter(metered.clone(), "bd", &pv), real);
    assert_eq!(
        see_through_meter(
            metered.clone(),
            &home_bin.join("bd").display().to_string(),
            &pv
        ),
        real,
        "a path spelling of bd sees through the meter too"
    );
    // The key a suite computes is the key setup computed without the meter on PATH.
    let key = |pv: &str| {
        let bd = see_through_meter(resolve_exe("bd", pv).unwrap(), "bd", pv);
        template_key(&exe_identity(&bd).unwrap(), "dolt")
    };
    assert_eq!(key(&pv), key(&sys.display().to_string()));
    // Not the meter, or a meter with nothing behind it: kept as resolved.
    assert_eq!(see_through_meter(real.clone(), "bd", &pv), real);
    let alone = home_bin.display().to_string();
    assert_eq!(see_through_meter(metered.clone(), "bd", &alone), metered);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn locate_exe_is_absolute_and_keeps_the_link() {
    let d = tmpdir("locate");
    let art = d.join("art");
    let bin = d.join("bin");
    fs::create_dir_all(&art).unwrap();
    fs::create_dir_all(&bin).unwrap();
    exe(&art.join("bd-meter"), "#!/bin/sh\n");
    std::os::unix::fs::symlink(art.join("bd-meter"), bin.join("bd")).unwrap();
    let pv = format!("relative-dir:/nonexistent:{}", bin.display());
    assert_eq!(
        locate_exe("bd", &pv),
        Some(bin.join("bd")),
        "the link, not the meter it points at, so calls stay metered"
    );
    assert!(locate_exe("bd", &pv).unwrap().is_absolute());
    assert_eq!(locate_exe("missing", &pv), None);
    assert_eq!(
        locate_exe(&bin.join("bd").display().to_string(), ""),
        Some(bin.join("bd"))
    );
    let _ = fs::remove_dir_all(&d);
}

// ---- orchestration against stand-ins -------------------------------------------------

#[test]
fn template_is_built_once_under_concurrency() {
    let Some((d, tools)) = fakes("tpl") else {
        return;
    };
    let root = d.join("root");
    let tools = std::sync::Arc::new(tools);
    let hs: Vec<_> = (0..4)
        .map(|_| {
            let (root, tools) = (root.clone(), tools.clone());
            std::thread::spawn(move || ensure_template(&root, &tools).unwrap())
        })
        .collect();
    let dirs: Vec<PathBuf> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(dirs.windows(2).all(|w| w[0] == w[1]));
    assert!(dirs[0].join("READY").is_file());
    assert!(dirs[0].join("ws/.beads/metadata.json").is_file());
    assert_eq!(
        fs::read_to_string(d.join("bd-inits"))
            .unwrap()
            .lines()
            .count(),
        1,
        "one bd init for four callers"
    );
    // The build's server was stopped: nothing listens on the port it used.
    let port = port_of(&dirs[0].join("ws/.beads"));
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_failed_template_build_leaves_nothing_and_says_why() {
    let Some((d, mut tools)) = fakes("tplfail") else {
        return;
    };
    let bad = d.join("bad-bd");
    exe(&bad, "#!/bin/sh\necho 'schema exploded' >&2\nexit 2\n");
    tools.bd = bad;
    let root = d.join("root");
    let e = ensure_template(&root, &tools).unwrap_err();
    assert!(e.contains("rc=2") && e.contains("schema exploded"), "{e}");
    let left: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .filter(|n| !n.to_string_lossy().ends_with(".lock"))
        .collect();
    assert!(left.is_empty(), "no half-built template: {left:?}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn fixtures_are_isolated_reset_is_fresh_and_down_removes_all() {
    let Some((d, tools)) = fakes("fx") else {
        return;
    };
    let root = d.join("root");
    let tpl = ensure_template(&root, &tools).unwrap();
    let a = up(&root, &tpl, &tools, "a", 0, None).unwrap();
    let b = up(&root, &tpl, &tools, "b", 0, None).unwrap();
    assert_ne!(a.port, b.port);
    assert_ne!(a.pid, b.pid);
    assert_ne!(a.fixture, b.fixture);
    assert_eq!(
        port_of(&a.ws.join(".beads")),
        a.port,
        ".beads points at A's own server"
    );
    assert!(TcpStream::connect(("127.0.0.1", a.port)).is_ok());
    assert!(a.ws.ends_with("ws") && a.name.starts_with("sptest_a_"));

    // A write lands in A's store only; reset puts A back to the template exactly.
    fs::write(a.fixture.join("data/written-by-test"), "x").unwrap();
    fs::write(a.ws.join(".beads/scribble"), "x").unwrap();
    let (pid2, port2, _) = reset(&a.fixture).unwrap();
    assert!(!a.fixture.join("data/written-by-test").exists());
    assert!(!a.ws.join(".beads/scribble").exists());
    assert!(
        !pid_alive(a.pid)
            || !pid_is_our_server(a.pid, &a.fixture.join("data/config.yaml"))
            || pid2 == a.pid
    );
    assert_eq!(port2, a.port, "same port when it is free");
    assert_eq!(port_of(&a.ws.join(".beads")), port2);
    assert!(TcpStream::connect(("127.0.0.1", port2)).is_ok());
    assert_eq!(read_trim(&a.fixture.join("resets")).as_deref(), Some("1"));
    assert!(
        TcpStream::connect(("127.0.0.1", b.port)).is_ok(),
        "B untouched by A's reset"
    );

    let dn = down(&a.fixture).unwrap();
    assert_eq!(dn.resets, Some(1));
    assert!(!a.fixture.exists());
    assert!(!pid_alive(pid2));
    assert!(down(&a.fixture).is_ok(), "down is idempotent");
    down(&b.fixture).unwrap();
    assert!(!pid_alive(b.pid));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn reaper_takes_the_fixture_down_when_its_owner_dies() {
    let Some((d, tools)) = fakes("reap") else {
        return;
    };
    let root = d.join("root");
    let tpl = ensure_template(&root, &tools).unwrap();
    // Kill-on-drop (sp-r70dc): a failed assertion between spawn and the explicit kill
    // below used to leave this fixture running for its full 30s as an orphan.
    let mut owner = testkit::ChildGuard::spawn(Command::new("sleep").arg("30"));
    let u = up(&root, &tpl, &tools, "r", owner.id(), None).unwrap();
    let (fx, oid) = (u.fixture.clone(), owner.id());
    let h = std::thread::spawn(move || reap(&fx, oid, Duration::from_millis(20)));
    sleep(Duration::from_millis(60));
    assert!(u.fixture.exists(), "alive owner: fixture kept");
    owner.kill();
    h.join().unwrap();
    assert!(!u.fixture.exists());
    assert!(!pid_alive(u.pid));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn reaper_exits_when_the_fixture_was_dropped_normally() {
    let d = tmpdir("reap-gone");
    let fx = d.join("fx-gone");
    // Owner alive (this process), fixture absent: returns at once rather than polling forever.
    reap(&fx, std::process::id(), Duration::from_millis(5));
    reap(&fx, 0, Duration::from_millis(5));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_server_never_holds_the_callers_pipe() {
    let Some((d, tools)) = fakes("pipe") else {
        return;
    };
    let root = d.join("root");
    let tpl = ensure_template(&root, &tools).unwrap();
    let mut fds = [0; 2];
    // SAFETY: plain pipe(2); both ends closed below. No O_CLOEXEC, as a shell's $(...) is.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let u = up(&root, &tpl, &tools, "p", 0, None).unwrap();
    unsafe { libc::close(fds[1]) };
    let mut buf = [0u8; 1];
    // EOF at once: no live descendant (the server) still holds the write end.
    let n = unsafe { libc::read(fds[0], buf.as_mut_ptr() as *mut libc::c_void, 1) };
    unsafe { libc::close(fds[0]) };
    assert_eq!(n, 0);
    down(&u.fixture).unwrap();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn stop_never_signals_a_pid_that_is_not_this_fixtures_server() {
    // Kill-on-drop (sp-r70dc).
    let mut other = testkit::ChildGuard::spawn(Command::new("sleep").arg("30"));
    stop_server(other.id(), Path::new("/var/tmp/fx-x/data/config.yaml"));
    assert!(pid_alive(other.id()), "an unrelated process survives");
    other.kill();
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
fn an_accepting_socket_that_never_greets_is_not_ready() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let held = std::thread::spawn(move || {
        let (c, _) = l.accept().unwrap();
        sleep(Duration::from_millis(900));
        drop(c);
    });
    assert!(!greets(&addr), "TCP accept alone is not readiness");
    held.join().unwrap();
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
fn embedded_reset_refuses_a_baseline_with_no_beads() {
    let d = tmpdir("er-bad");
    let baseline = d.join("empty-baseline");
    fs::create_dir_all(&baseline).unwrap();
    let fx = d.join("fx");
    write_beads(&fx, "live");

    let err = embedded_reset(&fx, &baseline).unwrap_err();
    assert!(err.contains("no .beads"), "{err}");
    assert_eq!(
        fs::read_to_string(fx.join(".beads/metadata.json")).unwrap(),
        "live",
        "a refused reset must not touch the live fixture"
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
fn embedded_down_removes_every_path_it_is_given() {
    let d = tmpdir("down");
    let a = d.join("fx");
    let b = d.join("baseline");
    let c = d.join("bin");
    write_beads(&a, "x");
    write_beads(&b, "x");
    fs::create_dir_all(&c).unwrap();

    embedded_down(&[a.clone(), b.clone(), c.clone()]);

    let t0 = Instant::now();
    while (a.exists() || b.exists() || c.exists()) && t0.elapsed() < Duration::from_secs(5) {
        sleep(Duration::from_millis(20));
    }
    assert!(!a.exists(), "fixture dir removed");
    assert!(!b.exists(), "baseline dir removed");
    assert!(!c.exists(), "bin dir removed");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn embedded_down_tolerates_empty_and_missing_paths() {
    // Server mode zeroes these vars before calling drop's shared cleanup path; embedded-down
    // must be a no-op, not a failure, on an empty string or a path that is already gone.
    embedded_down(&[PathBuf::new(), PathBuf::from("/does/not/exist")]);
}

#[test]
fn embedded_check_is_false_for_a_bd_that_is_not_on_path() {
    assert!(!embedded_check("sp-k2oyn-no-such-bd-binary"));
}

fn fake_bd_embedded(dir: &Path) -> PathBuf {
    // A stand-in for bd-embedded: `init` writes .beads and succeeds; anything else fails.
    let p = dir.join("bd-embedded");
    exe(
        &p,
        "#!/usr/bin/env bash\nset -e\nif [ \"$1\" = init ]; then mkdir -p .beads; \\\n  printf '{\"backend\":\"dolt\",\"dolt_mode\":\"embedded\"}' > .beads/metadata.json; \\\n  printf run >> .beads/.inits; exit 0; fi\nexit 1\n",
    );
    p
}

#[test]
fn embedded_check_true_with_a_working_bd() {
    let d = tmpdir("check-ok");
    let bd = fake_bd_embedded(&d);
    assert!(embedded_check(&bd.display().to_string()));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn embedded_up_builds_a_fixture_with_a_baseline_and_a_bd_shim() {
    let d = tmpdir("up-ok");
    let bd = fake_bd_embedded(&d);
    let bd_s = bd.display().to_string();

    let u = embedded_up(&bd_s, "sometag").unwrap();
    assert!(u.name.starts_with("sptest_sometag_"));
    assert!(u.dir.join(".beads/metadata.json").is_file());
    assert!(
        u.baseline.join(".beads/metadata.json").is_file(),
        "the baseline is a real snapshot, not a reference to the live dir"
    );
    assert_ne!(u.dir, u.baseline);
    let bin = u.bin.clone().expect("a resolvable bd gets a PATH shim");
    assert_eq!(fs::read_link(bin.join("bd")).unwrap(), bd);
    assert_eq!(u.bd, bd_s, "TESTDB_BD reports the resolved bd path");

    // The baseline is independent of the live dir from this point on.
    fs::write(u.dir.join(".beads/metadata.json"), "mutated by the suite").unwrap();
    assert_eq!(
        fs::read_to_string(u.baseline.join(".beads/metadata.json")).unwrap(),
        "{\"backend\":\"dolt\",\"dolt_mode\":\"embedded\"}"
    );
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&u.dir);
    let _ = fs::remove_dir_all(&u.baseline);
    if let Some(b) = u.bin {
        let _ = fs::remove_dir_all(&b);
    }
}

#[test]
fn embedded_up_fails_closed_and_cleans_up_when_init_fails() {
    let d = tmpdir("up-fail");
    let bd = d.join("bd-embedded");
    exe(&bd, "#!/usr/bin/env bash\nexit 3\n");
    let err = embedded_up(&bd.display().to_string(), "t").unwrap_err();
    assert!(err.contains("bd init failed"), "{err}");
    // embedded_up names the fixture dir it removed on failure (in its error text) so this
    // checks the exact directory, never a heuristic scan of the whole OS temp dir.
    let named = err
        .rsplit("fixture dir removed: ")
        .next()
        .unwrap()
        .trim();
    assert!(
        !Path::new(named).exists(),
        "a failed init must leave no fixture directory behind: {named}"
    );
    let _ = fs::remove_dir_all(&d);
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
