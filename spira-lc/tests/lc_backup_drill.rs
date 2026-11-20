//! The shell script is the subject (the binary-spawning exception): its drill functions are
//! extracted and run under `bash` against stub `dolt` and `spira-lc`. No shell suite may grow.

use std::process::Command;

const SCRIPT: &str = include_str!("../../spira/lc-backup.sh");
const FUNCTIONS: &[&str] = &[
    "_lcb_dbs", "_lcb_db_dir", "_lcb_stamp", "_lcb_require_db", "_lcb_sql", "_lcb_lock", "_lcb_work", "_lcb_sync_dir",
    "_lcb_verdict", "_lcb_drill_fail", "_lcb_restored_sql", "_lcb_query_report", "_lcb_drill_db_locked", "_lcb_drill_db", "cmd_drill",
];
const STUBS: &str = r#"#!/bin/bash
case "$(basename "$0")" in
dolt)
    case "$1 $2" in
    "backup restore") mkdir -p "$4"; exit 0 ;;
    esac
    q="$5"
    case "$q" in
    *"SELECT commit_hash"*) printf 'commit_hash\n%s\n' "$STUB_RESTORED_HEAD" ;;
    *"FROM dolt_log WHERE"*) [ -z "${STUB_CONTAINS_ERR:-}" ] || { echo "$STUB_CONTAINS_ERR" >&2; exit 3; }; printf 'c\n%s\n' "$STUB_CONTAINS" ;;
    *"SHOW FULL TABLES"*) printf 'Tables\nt1\n' ;;
    *"COUNT(*) FROM \`t1\`"*) printf 'c\n5\n' ;;
    esac ;;
spira-lc)
    echo "$*" >> "$STUB_LOG"
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
}

fn drill(args: &str, restored: &str, contains: &str, contains_err: &str) -> Run {
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
    let mut script = String::from("_lcb_import_drill_stamps() { :; }\n_lcb_drill_wait_quiet() { :; }\n");
    for f in FUNCTIONS {
        script += &function(f);
        script += "\n";
    }
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
        .env("STUB_LOG", d.join("assemble.log"))
        .env("STUB_MANIFEST", &manifest)
        .env("STUB_SET_HEAD", "hhhh")
        .env("STUB_RESTORED_HEAD", restored)
        .env("STUB_CONTAINS", contains)
        .env("STUB_CONTAINS_ERR", contains_err)
        .env("MANIFEST", &manifest)
        .output()
        .unwrap();
    Run {
        code: out.status.code(),
        out: String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
        verdict: std::fs::read_to_string(d.join("run/lc-backup/alpha/drill.verdict")).unwrap_or_default(),
        assembled: std::fs::read_to_string(d.join("assemble.log")).unwrap_or_default(),
    }
}

#[test]
fn a_restored_head_newer_than_the_stamped_head_that_contains_it_passes_naming_both() {
    let r = drill("--db alpha", "rrrr", "1", "");
    assert_eq!(r.code, Some(0), "{}", r.out);
    assert!(r.verdict.contains("restored head rrrr is newer than the stamped head hhhh"), "{}", r.verdict);
}

#[test]
fn a_stamped_head_absent_from_the_restored_history_fails_naming_both_heads_and_the_manifest() {
    let r = drill("--db alpha", "rrrr", "0", "");
    assert_eq!(r.code, Some(1), "{}", r.out);
    for want in ["hhhh", "rrrr", "20261120T164737186Z-hhhh", "(no stderr)", "exit 0"] {
        assert!(r.verdict.contains(want), "{want} missing: {}", r.verdict);
    }
    assert!(r.verdict.lines().all(|l| !l.trim_end().ends_with(':')), "{}", r.verdict);
}

#[test]
fn a_failing_restored_query_carries_its_stderr_and_exit_code() {
    let r = drill("--db alpha", "rrrr", "", "boom");
    assert_eq!(r.code, Some(1), "{}", r.out);
    assert!(r.verdict.contains("boom") && r.verdict.contains("exit 3"), "{}", r.verdict);
}

#[test]
fn a_named_manifest_is_assembled_by_name_and_its_head_comes_from_the_file_name() {
    let r = drill("--db alpha --manifest \"$MANIFEST\"", "hhhh", "1", "");
    assert_eq!(r.code, Some(0), "{}", r.out);
    assert!(r.assembled.contains("--manifest ") && r.assembled.contains("20261120T164737186Z-hhhh") && !r.assembled.contains("--state"), "{}", r.assembled);
}

#[test]
fn a_manifest_without_a_database_is_refused() {
    let r = drill("--manifest \"$MANIFEST\"", "hhhh", "1", "");
    assert_eq!(r.code, Some(2), "{}", r.out);
}
