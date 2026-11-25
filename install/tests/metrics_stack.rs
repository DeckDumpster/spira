//! The shell script is the subject (the binary-spira-spawning exception): `metrics-stack.sh` runs
//! under `bash` beside an empty `conf.sh` and the real `metrics/stack.env`, with a stub `podman`
//! that records its arguments.
// covers: systemd/spira-metrics-vm.service systemd/spira-metrics-logs.service
#[macro_use]
extern crate spira_test_id;
use std::fs;
use std::process::Command;

const STUB_PODMAN: &str = "#!/bin/bash\n[ \"$1\" = run ] || exit 0\nprintf '%s\\n' \"$@\" > \"$PODMAN_LOG\"\n";

fn root() -> std::path::PathBuf {
    spira_config::resolve::checkout_root_for_process().unwrap()
}

fn unit(name: &str) -> String {
    fs::read_to_string(root().join("systemd").join(name)).unwrap()
}

fn unit_args(name: &str) -> Vec<String> {
    let text = unit(name);
    let line = text.lines().find(|l| l.starts_with("ExecStart=")).unwrap();
    let rest = line.split("metrics-stack.sh").nth(1).unwrap();
    rest.split_whitespace().map(String::from).collect()
}

fn podman_run(args: &[String]) -> Vec<String> {
    podman_run_with(args, Some("60")).expect("podman run")
}

fn podman_run_with(args: &[String], flush: Option<&str>) -> Option<Vec<String>> {
    let t = testkit::TempDir::new("metrics-stack");
    let d = t.path();
    fs::create_dir_all(d.join("bin")).unwrap();
    fs::create_dir_all(d.join("metrics")).unwrap();
    fs::write(d.join("conf.sh"), "").unwrap();
    fs::write(d.join("metrics-stack.sh"), fs::read_to_string(root().join("spira/metrics-stack.sh")).unwrap()).unwrap();
    fs::write(d.join("metrics/stack.env"), fs::read_to_string(root().join("spira/metrics/stack.env")).unwrap()).unwrap();
    testkit::write_exe(d.join("bin/podman"), STUB_PODMAN);
    let path = format!("{}:{}", d.join("bin").display(), std::env::var("PATH").unwrap());
    let log = d.join("podman.log");
    let mut cmd = Command::new("bash");
    cmd.arg(d.join("metrics-stack.sh")).args(args).env_clear();
    if let Some(m) = flush {
        cmd.env("SPIRA_METRICS_FLUSH_MINUTES", m);
    }
    let out = cmd
        .env("PATH", path)
        .env("HOME", d.join("home"))
        .env("PODMAN_LOG", &log)
        .env("SPIRA_METRICS_DATA", d.join("data"))
        .env("SPIRA_OTEL_KEEP_DAYS", "9")
        .env("SPIRA_RUN", d.join("run"))
        .output()
        .unwrap();
    if out.status.code() == Some(2) {
        assert!(String::from_utf8_lossy(&out.stderr).contains("SPIRA_METRICS_FLUSH_MINUTES"), "{args:?}");
        assert!(!log.exists(), "podman ran: {args:?}");
        return None;
    }
    assert!(out.status.success(), "{args:?}");
    Some(fs::read_to_string(log).unwrap().lines().map(String::from).collect())
}

fn has(run: &[String], flag: &str) -> bool {
    run.iter().any(|a| a.starts_with(flag))
}

fn assert_store(unit_name: &str, image: &str) {
    assert_store_at(unit_name, image, "60");
    assert_store_at(unit_name, image, "7");
}

fn assert_store_at(unit_name: &str, image: &str, minutes: &str) {
    let flush = format!("-inmemoryDataFlushInterval={minutes}m");
    let run = podman_run_with(&unit_args(unit_name), Some(minutes)).expect("podman run");
    let at = |s: &str| run.iter().position(|a| a == s || a.starts_with(s)).unwrap_or_else(|| panic!("{s} missing: {run:?}"));
    let img = run.iter().position(|a| a.contains(image)).unwrap();
    assert!(at(&flush) > img, "{run:?}");
    for f in ["-storageDataPath=", "-retentionPeriod=", "-httpListenAddr="] {
        assert!(has(&run, f), "{f} missing: {run:?}");
    }
}

#[test_id(tyyalxu)]
#[test]
fn the_metrics_unit_passes_the_configured_flush_flag_after_the_image_with_its_other_flags() {
    assert_store("spira-metrics-vm.service", "victoria-metrics");
}

#[test_id(t5aud14)]
#[test]
fn the_logs_unit_passes_the_configured_flush_flag_after_the_image_with_its_other_flags() {
    assert_store("spira-metrics-logs.service", "victoria-logs");
}

#[test_id(tail4w2)]
#[test]
fn each_store_unit_comment_states_the_loss_as_twice_the_flush_key() {
    for (n, what) in [("spira-metrics-vm.service", "metrics"), ("spira-metrics-logs.service", "logs")] {
        let want = format!("loses up to twice SPIRA_METRICS_FLUSH_MINUTES of {what}.");
        assert!(unit(n).lines().any(|l| l.starts_with('#') && l.contains(&want)), "{n}");
    }
}

#[test_id(tuwvpiv)]
#[test]
fn the_collector_gets_no_flush_flag() {
    let run = podman_run_with(&["collector".to_string()], None).expect("podman run");
    assert!(run.iter().any(|a| a == "otel-collector"), "{run:?}");
    assert!(!has(&run, "-inmemoryDataFlushInterval"), "{run:?}");
}

#[test_id(t2fjda4)]
#[test]
fn each_store_unit_stops_with_one_sigterm_and_hides_no_exit_status() {
    for n in ["spira-metrics-vm.service", "spira-metrics-logs.service"] {
        let text = unit(n);
        let service: Vec<&str> = text
            .lines()
            .skip_while(|l| *l != "[Service]")
            .skip(1)
            .take_while(|l| !l.starts_with('['))
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect();
        assert!(service.contains(&"KillMode=mixed"), "{n}: KillMode=mixed missing");
        assert!(service.contains(&"TimeoutStopSec=90"), "{n}: TimeoutStopSec=90 missing");
        assert!(!service.iter().any(|l| l.starts_with("SuccessExitStatus")), "{n}");
    }
}

#[test_id(twh6f97)]
#[test]
fn a_store_without_a_positive_numeric_flush_key_exits_2_naming_it_and_runs_no_podman() {
    for part in ["vm", "logs"] {
        for v in [None, Some("0"), Some("00"), Some("x")] {
            assert!(podman_run_with(&[part.to_string()], v).is_none(), "{part} {v:?}");
        }
    }
}
