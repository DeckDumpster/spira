//! The shell script is the subject (the binary-spawning exception): its drill functions are
//! extracted and run under `bash` against stub `dolt` and `spira-lc`. No shell suite may grow.

#[macro_use]
extern crate spira_test_id;
use std::process::Command;

const SCRIPT: &str = include_str!("../../spira/lc-backup.sh");
const FUNCTIONS: &[&str] = &[
    "_lcb_dbs", "_lcb_db_dir", "_lcb_stamp", "_lcb_require_db", "_lcb_sql", "_lcb_lock", "_lcb_work", "_lcb_sync_dir",
    "_lcb_verdict", "_lcb_drill_fail", "_lcb_restored_sql", "_lcb_text", "_lcb_query_report", "_lcb_lap", "_lcb_phases_note", "_lcb_drill_db_locked", "_lcb_drill_db", "_lcb_drill_control", "cmd_drill",
];
const STUBS: &str = r#"#!/bin/bash
advance() { echo "$(( $(cat "$STUB_CLOCK") + ${1:-0} ))" > "$STUB_CLOCK"; }
case "$(basename "$0")" in
dolt)
    case "$1 $2" in
    "backup restore") advance "$STUB_ADV_RESTORE"; [ -z "${STUB_RESTORE_FAIL:-}" ] || exit 5; mkdir -p "$4"; exit 0 ;;
    esac
    q="$5"
    case "$q" in
    *"SELECT commit_hash"*) [ -z "${STUB_HEAD_ERR:-}" ] || { echo "$STUB_HEAD_ERR" >&2; exit 4; }; printf 'commit_hash\n%s\n' "$STUB_RESTORED_HEAD" ;;
    *"FROM dolt_log WHERE"*) [ -z "${STUB_CONTAINS_ERR:-}" ] || { echo "$STUB_CONTAINS_ERR" >&2; exit 3; }; printf 'c\n%s\n' "$STUB_CONTAINS" ;;
    *"SHOW FULL TABLES"*) printf 'Tables\nt1\n' ;;
    *"COUNT(*) FROM \`t1\`"*) case "$PWD" in */scratch/*) advance "$STUB_ADV_RESTORED" ;; *) advance "$STUB_ADV_LIVE" ;; esac; printf 'c\n5\n' ;;
    esac ;;
spira-lc)
    echo "$*" >> "$STUB_LOG"
    [ "$1" != admin-drill ] || { printf '%s\n' "$STUB_DRILL_OUT"; exit 0; }
    sleep "${STUB_REAL_SLEEP:-0}"; advance "$STUB_ADV_ASSEMBLE"
    m="$STUB_MANIFEST"
    while [ $# -gt 0 ]; do [ "$1" = --manifest ] && m="$2"; shift; done
    echo "$STUB_SET_HEAD 1 1024 $m" ;;
esac
"#;

fn function(name: &str) -> String {
    let at = SCRIPT.find(&format!("\n{name}() {{")).unwrap() + 1;
    let first = SCRIPT[at..].lines().next().unwrap();
    if first.ends_with('}') {
        return first.to_string();
    }
    SCRIPT[at..at + SCRIPT[at..].find("\n}\n").unwrap() + 2].to_string()
}

struct Run {
    code: Option<i32>,
    out: String,
    verdict: String,
    assembled: String,
    drill_ok: bool,
    control: String,
}

#[derive(Default)]
struct Knobs {
    restore: u64,
    live: u64,
    restored: u64,
    assemble: u64,
    real_assemble_sleep: u64,
}

const DRILL_OUT: &str = "sqlite-checkpoint: /x/control.db: 482 passes\ndrill ok: snapshot at 0, restored through 6, 6 keys compared";

fn drill(args: &str, restored: &str, contains: &str, contains_err: &str, head_err: &str, restore_fail: &str) -> Run {
    drill_with(args, restored, contains, contains_err, head_err, restore_fail, DRILL_OUT, &Knobs::default())
}

#[allow(clippy::too_many_arguments)]
fn drill_with(args: &str, restored: &str, contains: &str, contains_err: &str, head_err: &str, restore_fail: &str, drill_out: &str, knobs: &Knobs) -> Run {
    let tmp = testkit::TempDir::new("lc-backup-drill");
    let d = tmp.path();
    for sub in ["bin", "run", "data/alpha/.dolt", "sync", "scratch"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    for n in ["dolt", "spira-lc"] {
        testkit::write_exe(d.join("bin").join(n), STUBS);
    }
    std::fs::write(d.join("run/lc-backup.alpha.ok"), "1 hhhh -\n").unwrap();
    let manifest = d.join("20261120T164737186Z-hhhh");
    std::fs::write(&manifest, "").unwrap();
    let clock = d.join("clock");
    std::fs::write(&clock, "1700000000\n").unwrap();
    let mut script = String::from("_lcb_import_drill_stamps() { :; }\n_lcb_drill_wait_quiet() { :; }\n");
    for f in FUNCTIONS {
        script += &function(f);
        script += "\n";
    }
    script += "_lcb_now_ns() { echo \"$(( $(cat \"$STUB_CLOCK\") * 1000000000 ))\"; }\n";
    script += &format!("_LCB_DRILL_DEADLINE=$(( $(date +%s) + 5 ))\ncmd_drill {args}\n");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .current_dir(d)
        .env_clear()
        .env("PATH", format!("{}:/usr/local/bin:/usr/bin:/bin", d.join("bin").display()))
        .env("SPIRA_RUN", d.join("run"))
        .env("SPIRA_DOLT_DATA", d.join("data"))
        .env("SPIRA_LC_BACKUP_DB", "alpha")
        .env("SPIRA_LC_BACKUP_SYNC_DIR", d.join("sync"))
        .env("SPIRA_LC_BACKUP_DRILL_DIR", d.join("scratch"))
        .env("SPIRA_LC_BACKUP_DRILL_WAIT_SECS", "5")
        .env("SPIRA_LC_STORE_PATH", if args.contains("control") { "control-store" } else { "" })
        .env("STUB_DRILL_OUT", drill_out)
        .env("STUB_CLOCK", &clock)
        .env("STUB_ADV_RESTORE", knobs.restore.to_string())
        .env("STUB_ADV_LIVE", knobs.live.to_string())
        .env("STUB_ADV_RESTORED", knobs.restored.to_string())
        .env("STUB_ADV_ASSEMBLE", knobs.assemble.to_string())
        .env("STUB_REAL_SLEEP", knobs.real_assemble_sleep.to_string())
        .env("STUB_LOG", d.join("assemble.log"))
        .env("STUB_MANIFEST", &manifest)
        .env("STUB_SET_HEAD", "hhhh")
        .env("STUB_RESTORED_HEAD", restored)
        .env("STUB_CONTAINS", contains)
        .env("STUB_CONTAINS_ERR", contains_err)
        .env("STUB_HEAD_ERR", head_err)
        .env("STUB_RESTORE_FAIL", restore_fail)
        .env("MANIFEST", &manifest)
        .output()
        .unwrap();
    Run {
        code: out.status.code(),
        out: String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
        verdict: std::fs::read_to_string(d.join("run/lc-backup/alpha/drill.verdict")).unwrap_or_default(),
        drill_ok: d.join("run/lc-backup/alpha/drill.ok").exists(),
        control: std::fs::read_to_string(d.join("run/lc-backup/control/drill.verdict")).unwrap_or_default(),
        assembled: std::fs::read_to_string(d.join("assemble.log")).unwrap_or_default(),
    }
}

#[test_id(tt9uyw8)]
#[test]
fn a_restored_head_newer_than_the_stamped_head_that_contains_it_passes_naming_both() {
    let r = drill("--db alpha", "rrrr", "1", "", "", "");
    assert_eq!(r.code, Some(0), "{}", r.out);
    assert!(r.verdict.contains("restored head rrrr is newer than the stamped head hhhh"), "{}", r.verdict);
}

#[test_id(tvpf22g)]
#[test]
fn a_stamped_head_absent_from_the_restored_history_fails_naming_both_heads_and_the_manifest() {
    let r = drill("--db alpha", "rrrr", "0", "", "", "");
    assert_eq!(r.code, Some(1), "{}", r.out);
    for want in ["hhhh", "rrrr", "20261120T164737186Z-hhhh", "(no stderr)", "exit 0"] {
        assert!(r.verdict.contains(want), "{want} missing: {}", r.verdict);
    }
    assert!(r.verdict.lines().all(|l| !l.trim_end().ends_with(':')), "{}", r.verdict);
}

#[test_id(todwyl8)]
#[test]
fn a_failing_restored_query_carries_its_stderr_and_exit_code() {
    let r = drill("--db alpha", "rrrr", "", "boom", "", "");
    assert_eq!(r.code, Some(1), "{}", r.out);
    assert!(r.verdict.contains("boom") && r.verdict.contains("exit 3"), "{}", r.verdict);
}

#[test_id(tweqrbr)]
#[test]
fn a_named_manifest_is_assembled_by_name_and_its_head_comes_from_the_file_name() {
    let r = drill("--db alpha --manifest \"$MANIFEST\"", "hhhh", "1", "", "", "");
    assert_eq!(r.code, Some(0), "{}", r.out);
    assert!(r.assembled.contains("--manifest ") && r.assembled.contains("20261120T164737186Z-hhhh") && !r.assembled.contains("--state"), "{}", r.assembled);
}

#[test_id(tr9u3ul)]
#[test]
fn a_manifest_without_a_database_is_refused() {
    let r = drill("--manifest \"$MANIFEST\"", "hhhh", "1", "", "", "");
    assert_eq!(r.code, Some(2), "{}", r.out);
}

#[test_id(t3dq5yl)]
#[test]
fn a_failing_head_query_is_a_failure_carrying_its_stderr_even_when_containment_answers_one() {
    let r = drill("--db alpha", "", "1", "", "headboom", "");
    assert_eq!(r.code, Some(1), "{}", r.out);
    assert!(r.verdict.contains("headboom") && r.verdict.contains("exit 4"), "{}", r.verdict);
}

#[test_id(tmc49dy)]
#[test]
fn a_silent_restore_failure_still_explains_itself() {
    let r = drill("--db alpha", "rrrr", "1", "", "", "fail");
    assert_eq!(r.code, Some(1), "{}", r.out);
    assert!(r.verdict.contains("restore of 20261120T164737186Z-hhhh failed: (no output)"), "{}", r.verdict);
}

#[test_id(tcmvs0j)]
#[test]
fn only_a_drill_of_the_newest_backup_records_drill_ok() {
    assert!(drill("--db alpha", "hhhh", "1", "", "", "").drill_ok);
    assert!(!drill("--db alpha --manifest \"$MANIFEST\"", "hhhh", "1", "", "", "").drill_ok);
}

fn seconds_of(verdict: &str, phase: &str) -> bool {
    let at = verdict.find(&format!("({phase} ")).or_else(|| verdict.find(&format!(", {phase} "))).unwrap_or_else(|| panic!("{phase} missing: {verdict}"));
    let rest = &verdict[verdict[at..].find(phase).unwrap() + at + phase.len() + 1..];
    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
    !n.is_empty() && rest[n.len()..].starts_with('s')
}

#[test_id(tm7d3h0)]
#[test]
fn a_passing_dolt_drill_verdict_carries_the_seconds_of_each_phase() {
    let r = drill("--db alpha", "hhhh", "1", "", "", "");
    assert_eq!(r.code, Some(0), "{}", r.out);
    for phase in ["quiet wait", "lock wait", "assemble", "restore", "head check", "live counts", "restored counts"] {
        assert!(seconds_of(&r.verdict, phase), "{}", r.verdict);
    }
}

#[test_id(tszwx7v)]
#[test]
fn a_failing_dolt_drill_carries_the_phases_that_ran_before_the_failure() {
    let r = drill("--db alpha", "rrrr", "1", "", "", "fail");
    assert_eq!(r.code, Some(1), "{}", r.out);
    assert!(seconds_of(&r.verdict, "assemble") && seconds_of(&r.verdict, "restore"), "{}", r.verdict);
    assert!(!r.verdict.contains("head check"), "{}", r.verdict);
}

#[test_id(tzhqh1x)]
#[test]
fn the_control_verdict_keeps_the_drill_ok_line_and_its_seconds() {
    let r = drill("--db control", "hhhh", "1", "", "", "");
    assert_eq!(r.code, Some(0), "{}", r.out);
    assert!(r.control.starts_with("lc-backup: drill ok — control: drill ok: snapshot at 0, restored through 6, 6 keys compared"), "{}", r.control);
    assert!(seconds_of(&r.control, "admin-drill"), "{}", r.control);
}

#[test_id(tjruezk)]
#[test]
fn a_phase_clock_counts_the_seconds_the_phase_took() {
    let knobs = Knobs { restore: 3, live: 2, restored: 1, assemble: 0, real_assemble_sleep: 1 };
    let r = drill_with("--db alpha", "hhhh", "1", "", "", "", DRILL_OUT, &knobs);
    assert_eq!(r.code, Some(0), "{}", r.out);
    let want = "quiet wait 0s, lock wait 0s, assemble 0s, restore 3s, head check 0s, live counts 2s, restored counts 1s";
    assert!(r.verdict.contains(want), "{}", r.verdict);
}

#[test_id(tqjk9s9)]
#[test]
fn a_control_drill_without_a_drill_ok_line_fails_rather_than_passing_on_another_line() {
    let r = drill_with("--db control", "hhhh", "1", "", "", "", "sqlite-checkpoint: fine", &Knobs::default());
    assert_eq!(r.code, Some(1), "{}", r.out);
    assert!(r.control.starts_with("lc-backup: drill: control: admin-drill printed no drill ok: line"), "{}", r.control);
    assert!(seconds_of(&r.control, "admin-drill"), "{}", r.control);
}
