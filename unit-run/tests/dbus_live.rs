// covers: unit-run/**
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use unit_run::dbus::{bus_path, Conn};
use unit_run::watch;
use unit_run::Record;

fn user_manager() -> bool {
    bus_path().is_ok_and(|p| std::path::Path::new(&p).exists()) && Command::new("systemd-run").args(["--user", "--wait", "--collect", "--quiet", "--unit=unit-run-probe", "true"]).status().is_ok_and(|s| s.success())
}

fn read_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/io").unwrap().lines().find_map(|l| l.strip_prefix("read_bytes: ")).unwrap().trim().parse().unwrap()
}

fn run(unit: &str, props: &[&str], argv: &[&str]) {
    let mut c = Command::new("systemd-run");
    c.args(["--user", "--wait", "--collect", "--quiet", &format!("--unit={unit}")]);
    for p in props {
        c.arg(format!("--property={p}"));
    }
    c.args(argv);
    let _ = c.status();
}

#[test]
fn real_transient_units_yield_one_record_each_with_the_managers_result_and_status() {
    if !user_manager() {
        assert!(std::env::var_os("UNIT_RUN_REQUIRE_BUS").is_none(), "UNIT_RUN_REQUIRE_BUS is set but there is no systemd user manager");
        eprintln!("SKIP: no systemd user manager on this host");
        return;
    }
    let tag = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() % 1_000_000);
    let (ok, bad, slow) = (format!("ur-ok-{tag}"), format!("ur-bad-{tag}"), format!("ur-slow-{tag}"));
    let now_us = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64;

    let mut conn = Conn::connect(&bus_path().unwrap()).unwrap();
    watch::arm(&mut conn).unwrap();
    let mut tracker = watch::Tracker::new(now_us());
    let records: Arc<Mutex<Vec<Record>>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let idle_io = Arc::new(Mutex::new(None));
    let (r2, s2, io2) = (records.clone(), stop.clone(), idle_io.clone());
    let h = std::thread::spawn(move || {
        let before = read_bytes();
        let idle_until = Instant::now() + Duration::from_secs(2);
        let mut idle_done = false;
        while !s2.load(Ordering::Relaxed) {
            let got = watch::step(&mut conn, &mut tracker, now_us, Duration::from_millis(200)).unwrap();
            if !idle_done && Instant::now() > idle_until {
                *io2.lock().unwrap() = Some(read_bytes() - before);
                idle_done = true;
            }
            r2.lock().unwrap().extend(got);
        }
    });

    std::thread::sleep(Duration::from_millis(2500));
    run(&ok, &[], &["true"]);
    run(&bad, &[], &["sh", "-c", "exit 3"]);
    run(&slow, &["RuntimeMaxSec=1"], &["sleep", "60"]);

    let deadline = Instant::now() + Duration::from_secs(15);
    while records.lock().unwrap().iter().filter(|r| r.unit.contains(&tag)).count() < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(500));
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();

    let got: Vec<Record> = records.lock().unwrap().iter().filter(|r| r.unit.contains(&tag)).cloned().collect();
    let one = |name: &str| {
        let m: Vec<&Record> = got.iter().filter(|r| r.unit == format!("{name}.service")).collect();
        assert_eq!(m.len(), 1, "{name}: {got:?}");
        m[0].clone()
    };
    let r = one(&ok);
    assert_eq!((r.result.as_str(), r.exit_status), ("success", Some(0)), "{r:?}");
    let r = one(&bad);
    assert_eq!((r.result.as_str(), r.exit_code.as_deref(), r.exit_status), ("exit-code", Some("exited"), Some(3)), "{r:?}");
    let r = one(&slow);
    assert_eq!(r.result, "timeout", "{r:?}");
    assert!(r.started_us.is_some_and(|s| r.ended_us > s && r.ended_us - s < 20_000_000), "{r:?}");
    assert_eq!(r.invocation_id.len(), 32);
    assert_eq!(*idle_io.lock().unwrap(), Some(0), "an idle listener read from storage");
}

#[test]
fn a_run_that_ended_while_the_listener_was_stopped_is_recorded_once_on_catch_up() {
    if !user_manager() {
        assert!(std::env::var_os("UNIT_RUN_REQUIRE_BUS").is_none(), "UNIT_RUN_REQUIRE_BUS is set but there is no systemd user manager");
        eprintln!("SKIP: no systemd user manager on this host");
        return;
    }
    let now_us = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64;
    let unit = format!("ur-gap-{}-{}", std::process::id(), now_us() % 1_000_000);
    let since = now_us();
    let _ = Command::new("systemd-run").args(["--user", "--wait", "--quiet", &format!("--unit={unit}"), "sh", "-c", "exit 3"]).status();

    let mut conn = Conn::connect(&bus_path().unwrap()).unwrap();
    watch::arm(&mut conn).unwrap();
    let mut tracker = watch::Tracker::new(since);
    let got = watch::catch_up(&mut conn, &mut tracker, now_us).unwrap();
    let _ = Command::new("systemctl").args(["--user", "reset-failed", &format!("{unit}.service")]).status();
    let mine: Vec<&Record> = got.iter().filter(|r| r.unit == format!("{unit}.service")).collect();
    assert_eq!(mine.len(), 1, "{got:?}");
    assert_eq!((mine[0].result.as_str(), mine[0].exit_status), ("exit-code", Some(3)), "{:?}", mine[0]);
    assert!(watch::catch_up(&mut conn, &mut tracker, now_us).unwrap().iter().all(|r| r.unit != format!("{unit}.service")), "a second catch-up recorded the run again");
}
