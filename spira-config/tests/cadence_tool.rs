//! `spira/cadence.sh` is the subject, so the script itself runs under `bash` against a stub
//! `systemctl` that models the one property that matters: a timer with no calendar anchor in its
//! drop-in has no next elapse once restarted. No shell suite may grow.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
use std::process::Command;

const SYSTEMCTL: &str = r#"#!/bin/bash
[ "$1" = --user ] && shift
verb="$1"; shift
dropin="$STUB_UNITS/${1:-}.d/cadence.conf"
anchored() { [ -f "$dropin" ] && grep -q '^OnCalendar=.' "$dropin"; }
case "$verb" in
is-active) [ -z "${STUB_INACTIVE:-}" ] ;;
show)
    case "$*" in
    *"-p Unit"*) echo "${1%.timer}.service" ;;
    *ActiveState*) echo inactive ;;
    *NextElapseUSecRealtime*)
        if [ -n "${STUB_NO_NEXT:-}" ] && [ -f "$dropin" ]; then echo n/a
        elif [ -f "$dropin" ] && ! anchored; then echo n/a
        else echo "Tue 2030-01-01 00:00:00 UTC"; fi ;;
    *NextElapseUSecMonotonic*)
        if [ -f "$dropin" ] && ! anchored; then echo infinity; else echo n/a; fi ;;
    esac ;;
*) exit 0 ;;
esac
"#;

struct Box {
    _dir: testkit::TempDir,
    units: PathBuf,
    env: Vec<(&'static str, String)>,
}

fn timer_box() -> Box {
    let dir = testkit::TempDir::new("cadence-tool");
    let d = dir.path();
    let release = d.join("release");
    std::fs::create_dir_all(release.join("bin")).unwrap();
    std::os::unix::fs::symlink(testkit::bin_exe("spira-config"), release.join("bin/spira-config")).unwrap();
    let units = d.join("units");
    std::fs::create_dir(&units).unwrap();
    std::fs::write(units.join("spira-groom-t1.timer"), "[Timer]\nOnBootSec=5min\nOnUnitActiveSec=1h\n").unwrap();
    std::fs::write(units.join("spira-sweep-t1.timer"), "[Timer]\nOnBootSec=5min\nOnUnitActiveSec=1h\n").unwrap();
    testkit::write_exe(d.join("systemctl"), SYSTEMCTL);
    let toml = spira_config::process::fixture_toml(d, &[]);
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    let env = vec![
        ("PATH", format!("{}:/usr/bin:/bin", release.join("bin").display())),
        ("HOME", d.display().to_string()),
        ("SPIRA_RELEASE", release.display().to_string()),
        ("SPIRA_HOME", home.display().to_string()),
        ("SPIRA_TOML", toml.display().to_string()),
        ("SPIRA_INSTANCE", "t1".into()),
        ("SPIRA_SYSTEMD_USER_DIR", units.display().to_string()),
        ("SPIRA_SYSTEMCTL", d.join("systemctl").display().to_string()),
        ("STUB_UNITS", units.display().to_string()),
    ];
    Box { _dir: dir, units, env }
}

/// Runs `snippet` after sourcing the script once: its own sourcing guard keeps `main` from
/// running, and every command is a call of the script's own function, in a subshell as the
/// script's `exit` demands. Sourcing is most of the cost of a run, so the cases share one.
fn cadence(b: &Box, snippet: &str) -> String {
    let script = testkit::manifest_dir().parent().unwrap().join("spira/cadence.sh");
    let spelled = script.display().to_string();
    let mut c = Command::new("bash");
    c.env_clear().args(["-c", &format!(". \"${{0%/*}}//cadence.sh\"\n{snippet}"), &spelled]);
    for (k, v) in b.env.iter() {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn dropin(b: &Box, unit: &str) -> Option<String> {
    std::fs::read_to_string(Path::new(&b.units).join(format!("{unit}.d/cadence.conf"))).ok()
}

// covers: UC-instance-lifecycle-32
#[test_id(tz2u4w1)]
#[test]
fn a_cadence_change_keeps_an_anchor_and_a_change_that_leaves_no_next_elapse_is_refused_and_reverted() {
    let b = timer_box();
    let out = cadence(
        &b,
        r#"
( cmd_set groom 7h --why slower ); echo "set-rc=$?"
( STUB_NO_NEXT=1 cmd_set sweep 2h ); echo "refused-rc=$?"
( cmd_set nosuchunit 6h ); echo "unknown-rc=$?"
( STUB_INACTIVE=1 cmd_verify ); echo "empty-rc=$?"
( cmd_verify ); echo "armed-rc=$?"
"#,
    );
    for want in ["set-rc=0", "refused-rc=1", "unknown-rc=1", "empty-rc=1", "armed-rc=0"] {
        assert!(out.contains(want), "{want}: {out}");
    }
    assert!(out.contains("REFUSED"), "{out}");
    assert!(out.contains("no installed timer matches"), "{out}");
    assert!(out.contains("refusing to report all-clear"), "{out}");
    let text = dropin(&b, "spira-groom-t1.timer").expect("the drop-in is written");
    assert!(text.contains("OnUnitActiveSec=\nOnBootSec=\n"), "the template's schedule is cleared first: {text}");
    assert!(text.contains("OnUnitActiveSec=7h\n") && text.contains("OnCalendar=*-*-* 00:00:00\n"), "a span with no calendar form keeps a daily anchor: {text}");
    assert!(text.contains("# Why: slower") && text.contains("REVERT:"), "{text}");
    assert_eq!(dropin(&b, "spira-sweep-t1.timer"), None, "the refused drop-in is reverted");
}
