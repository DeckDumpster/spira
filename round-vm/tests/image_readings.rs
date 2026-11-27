//! The image time readings run for real against a stub `aws` and `fstrim`: a reading that
//! fails says so, and never changes how the image wait ends.

#[macro_use]
extern crate spira_test_id;
use std::process::Command;

const WAIT: &str = include_str!("../wait-image.sh");
const AMI_SETUP: &str = include_str!("../ami-setup.sh");

/// `describe-images` stays pending for one poll, then answers `$LAST`; `describe-snapshots` answers `$SIZE`, or fails when it is `fail`.
const AWS: &str = r#"#!/bin/bash
case "$2" in
describe-images)
    if [ -e "$STUB/polled" ]; then printf '%s\tsnap-1\n' "$LAST"; else : > "$STUB/polled"; printf 'pending\tNone\n'; fi ;;
describe-snapshots)
    [ "$SIZE" != fail ] || exit 254
    printf '45%%\t%s\n' "$SIZE" ;;
esac
"#;

fn run(name: &str, stub: &str, text: &str, script: &str, env: &[(&str, &str)]) -> (Option<i32>, String) {
    let tmp = testkit::TempDir::new("image-readings");
    testkit::write_exe(tmp.path().join(name), stub);
    // The wait counts its own sleeps toward the bound, so a no-op sleep spends it at once.
    testkit::write_exe(tmp.path().join("sleep"), "#!/bin/sh\nexit 0\n");
    let o = Command::new("bash")
        .args(["-c", &format!("exec 2>&1\n{script}"), text, "ami-1"])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", tmp.path().display()))
        .env("STUB", tmp.path())
        .envs(env.iter().copied())
        .output()
        .unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).into_owned())
}

fn wait(last: &str, size: &str, max: &str) -> (Option<i32>, String) {
    run("aws", AWS, "wait-image", WAIT, &[("LAST", last), ("SIZE", size), ("ROUND_AMI_WAIT_INTERVAL", "1"), ("ROUND_AMI_WAIT_MAX", max)])
}

#[test_id(t1dbbjd)]
#[test]
fn an_available_image_reports_the_snapshot_progress_time_and_size() {
    let (rc, out) = wait("available", "123", "60");
    assert_eq!(rc, Some(0), "{out}");
    assert!(out.contains("image ami-1 pending (0s) snapshot None progress unknown"), "{out}");
    assert!(out.contains("image ami-1 available after ") && out.contains("snapshot snap-1: 123 bytes"), "{out}");
    let (_, out) = wait("pending", "123", "2");
    assert!(out.contains("snapshot snap-1 45%"), "{out}");
}

#[test_id(t2qmc4m)]
#[test]
fn a_size_the_api_omits_is_told_apart_from_a_read_that_failed() {
    let (rc, out) = wait("available", "None", "60");
    assert_eq!((rc, out.contains("snap-1: size not reported")), (Some(0), true), "{out}");
    let (rc, out) = wait("available", "fail", "60");
    assert_eq!((rc, out.contains("snap-1: size unread (describe-snapshots failed)")), (Some(0), true), "{out}");
}

#[test_id(tseleoe)]
#[test]
fn the_snapshot_readings_leave_the_bound_and_the_failed_image_exits_as_they_were() {
    let (rc, out) = wait("failed", "fail", "60");
    assert_eq!((rc, out.contains("image ami-1 is failed")), (Some(1), true), "{out}");
    let (rc, out) = wait("pending", "fail", "0");
    assert_eq!((rc, out.contains("still pending after 0s")), (Some(1), true), "{out}");
}

#[test_id(tjzvikx)]
#[test]
fn only_a_disk_that_cannot_discard_lets_a_refused_trim_pass() {
    let tail = &AMI_SETUP[AMI_SETUP.find("if ! trimmed=").expect("ami-setup.sh trims the root")..];
    let trim = |says: &str, rc: &str| {
        run("fstrim", "#!/bin/sh\necho \"$SAYS\"\nexit \"$RC\"\n", "ami-setup", &format!("set -euo pipefail\n{tail}"), &[("SAYS", says), ("RC", rc)])
    };
    assert_eq!(trim("/: 5 bytes trimmed", "0"), (Some(0), "image-bytes /: 5 bytes trimmed\n".into()));
    let (rc, out) = trim("fstrim: /: the discard operation is not supported", "1");
    assert_eq!((rc, out.starts_with("image-bytes fstrim: ")), (Some(0), true), "{out}");
    let (rc, out) = trim("fstrim: /: FITRIM ioctl failed: Input/output error", "1");
    assert_eq!((rc, out.contains("image-bytes")), (Some(1), false), "{out}");
}
