//! The shell script is the subject (the binary-spira-spawning exception): `metrics-stack.sh` runs
//! under `bash` beside an empty `conf.sh` and the real `metrics/stack.env`, with a stub `podman`
//! that records its arguments.
// covers: systemd/spira-metrics-vm.service systemd/spira-metrics-logs.service
#[macro_use]
extern crate spira_test_id;
use std::fs;
use std::process::Command;

const FLUSH: &str = "-inmemoryDataFlushInterval=60s";
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
    let st = Command::new("bash")
        .arg(d.join("metrics-stack.sh"))
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("HOME", d.join("home"))
        .env("PODMAN_LOG", &log)
        .env("SPIRA_METRICS_DATA", d.join("data"))
        .env("SPIRA_OTEL_KEEP_DAYS", "9")
        .env("SPIRA_RUN", d.join("run"))
        .status()
        .unwrap();
    assert!(st.success(), "{args:?}");
    fs::read_to_string(log).unwrap().lines().map(String::from).collect()
}

fn has(run: &[String], flag: &str) -> bool {
    run.iter().any(|a| a.starts_with(flag))
}

fn assert_store(unit_name: &str, image: &str) {
    let run = podman_run(&unit_args(unit_name));
    let at = |s: &str| run.iter().position(|a| a == s || a.starts_with(s)).unwrap_or_else(|| panic!("{s} missing: {run:?}"));
    let img = run.iter().position(|a| a.contains(image)).unwrap();
    assert!(at(FLUSH) > img, "{run:?}");
    for f in ["-storageDataPath=", "-retentionPeriod=", "-httpListenAddr="] {
        assert!(has(&run, f), "{f} missing: {run:?}");
    }
}

#[test_id(tyyalxu)]
#[test]
fn the_metrics_unit_passes_the_60s_flush_flag_after_the_image_with_its_other_flags() {
    assert_store("spira-metrics-vm.service", "victoria-metrics");
}

#[test_id(t5aud14)]
#[test]
fn the_logs_unit_passes_the_60s_flush_flag_after_the_image_with_its_other_flags() {
    assert_store("spira-metrics-logs.service", "victoria-logs");
}

#[test_id(tail4w2)]
#[test]
fn each_store_unit_comment_states_the_60s_loss() {
    for (n, what) in [("spira-metrics-vm.service", "metrics"), ("spira-metrics-logs.service", "logs")] {
        let want = format!("loses up to 60 s of {what}");
        assert!(unit(n).lines().any(|l| l.starts_with('#') && l.contains(&want)), "{n}");
    }
}

#[test_id(tuwvpiv)]
#[test]
fn the_collector_gets_no_flush_flag() {
    let run = podman_run(&["collector".to_string()]);
    assert!(run.iter().any(|a| a == "otel-collector"), "{run:?}");
    assert!(!has(&run, "-inmemoryDataFlushInterval"), "{run:?}");
}
