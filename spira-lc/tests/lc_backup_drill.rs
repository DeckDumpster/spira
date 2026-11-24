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

const REGISTRY_STUBS: &str = r#"#!/bin/bash
echo "$(basename "$0") $*" >> "$STUB_LOG"
case "$(basename "$0")" in
test-registry)
    [ -z "${STUB_BACKUP_FAIL:-}" ] || { echo "disk full" >&2; exit 1; }
    echo data > "$3"; echo "$3 5 2" ;;
spira-lc)
    [ -z "${STUB_PUT_FAIL:-}" ] || { echo denied >&2; exit 1; }
    echo archived ;;
esac
"#;

const CLOCK_PRELUDE: &str = r#"
_LCB_REGISTRY=test-registry
date() {
    case "$1" in
    +%s) cat "$CLOCK" ;;
    +%s%N) echo "$(cat "$CLOCK")000000000" ;;
    -u) shift; command date -u -d "@$(cat "$CLOCK")" "$@" ;;
    *) command date "$@" ;;
    esac
}
sleep() { echo $(( $(cat "$CLOCK") + $1 )) > "$CLOCK"; }
"#;

const SHELL_FUNCTIONS: &[&str] = &[
    "_lcb_dbs", "_lcb_stamp", "_lcb_lock", "_lcb_verdict", "_lcb_sync_dir", "_lcb_record_deferral", "_lcb_drill_wait_quiet",
    "_lcb_drill_db", "_lcb_lap", "_lcb_now_ns", "_lcb_phases_note", "_lcb_import_drill_stamps", "_lcb_run_registry", "_lcb_run_db",
    "_lcb_measure_db", "_lcb_measure", "_lcb_all_ok", "cmd_run", "cmd_age", "cmd_drill", "cmd_explain_drill", "_lcb_drill_control",
];

struct Box {
    tmp: testkit::TempDir,
}

impl Box {
    fn new() -> Self {
        let tmp = testkit::TempDir::new("lc-backup-shell");
        for sub in ["bin", "run", "sync", "dest"] {
            std::fs::create_dir_all(tmp.path().join(sub)).unwrap();
        }
        for n in ["test-registry", "spira-lc"] {
            testkit::write_exe(tmp.path().join("bin").join(n), REGISTRY_STUBS);
        }
        std::fs::write(tmp.path().join("clock"), "1700000000\n").unwrap();
        Box { tmp }
    }
    fn p(&self, rel: &str) -> std::path::PathBuf {
        self.tmp.path().join(rel)
    }
    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.p(rel)).unwrap_or_default()
    }
    fn run(&self, body: &str, env: &[(&str, &str)]) -> (Option<i32>, String) {
        let mut script = String::from(CLOCK_PRELUDE);
        for f in SHELL_FUNCTIONS {
            script += &function(f);
            script += "\n";
        }
        script += body;
        let d = self.tmp.path();
        let mut c = Command::new("bash");
        c.arg("-c").arg(script).current_dir(d).env_clear()
            .env("PATH", format!("{}:/usr/local/bin:/usr/bin:/bin", d.join("bin").display()))
            .env("CLOCK", d.join("clock"))
            .env("STUB_LOG", d.join("log"))
            .env("SPIRA_RUN", d.join("run"))
            .env("SPIRA_LC_BACKUP_DB", "alpha")
            .env("SPIRA_LC_BACKUP_URL", format!("file://{}", d.join("dest").display()))
            .env("SPIRA_LC_BACKUP_SYNC_DIR", d.join("sync"))
            .env("SPIRA_LC_BACKUP_DRILL_DIR", d.join("scratch"))
            .env("SPIRA_LC_BACKUP_DRILL_WAIT_SECS", "0")
            .env("SPIRA_LC_BACKUP_DRILL_POLL_SECS", "1")
            .env("SPIRA_LC_BACKUP_MAX_AGE", "1000")
            .env("SPIRA_LC_STORE_PATH", "");
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
    }
}

const NO_REFUSALS: &str = "_lcb_refuse_unconfigured() { return 0; }\n_lcb_refuse_same_disk() { return 0; }\n";

// covers: UC-instance-lifecycle-48
#[test_id(tjaa7jo)]
#[test]
fn a_drill_whose_store_stays_busy_past_the_cap_exits_red_with_a_deferred_verdict_naming_the_seconds_waited() {
    let b = Box::new();
    let (code, out) = b.run("_lcb_drill_busy() { echo busy; }\ncmd_drill", &[("SPIRA_LC_BACKUP_DRILL_WAIT_SECS", "3")]);
    assert_eq!(code, Some(1), "{out}");
    let v = b.read("run/lc-backup/alpha/drill.verdict");
    assert!(v.contains("DEFERRED past 3s") && v.contains("(quiet wait 3s)"), "{v}");
    assert!(b.p("run/lc-backup/alpha/drill.deferred").exists());
}

// covers: UC-instance-lifecycle-48
#[test_id(tmlyraj)]
#[test]
fn a_drill_whose_lock_stays_held_exits_red_with_a_deferred_verdict() {
    let b = Box::new();
    let held = b.p("run/lc-backup/alpha/.lock");
    std::fs::create_dir_all(held.parent().unwrap()).unwrap();
    let body = "_lcb_drill_busy() { :; }\nexec 8>\"$SPIRA_RUN/lc-backup/alpha/.lock\"; flock -n 8\ncmd_drill";
    let (code, out) = b.run(body, &[]);
    assert_eq!(code, Some(1), "{out}");
    assert!(b.read("run/lc-backup/alpha/drill.verdict").contains("DEFERRED"), "{out}");
    assert!(b.read("run/lc-backup/alpha/drill.verdict").contains("a run holds the lock"), "{out}");
}

// covers: UC-instance-lifecycle-48
#[test_id(t4x1hwd)]
#[test]
fn one_databases_deferral_leaves_another_databases_verdict_alone() {
    let b = Box::new();
    let body = "_lcb_drill_busy() { :; }\n_lcb_drill_db_locked() { echo \"ok $1\" > \"$(_lcb_verdict \"$1\")\"; }\n\
                mkdir -p \"$SPIRA_RUN/lc-backup/beta\"; exec 8>\"$SPIRA_RUN/lc-backup/beta/.lock\"; flock -n 8\ncmd_drill";
    let (code, out) = b.run(body, &[("SPIRA_LC_BACKUP_DB", "alpha beta")]);
    assert_eq!(code, Some(1), "{out}");
    assert!(b.read("run/lc-backup/alpha/drill.verdict").starts_with("ok alpha"), "{out}");
    assert!(b.read("run/lc-backup/beta/drill.verdict").contains("DEFERRED"), "{out}");
}

// covers: UC-instance-lifecycle-48
#[test_id(tl3iir7)]
#[test]
fn explain_drill_reports_the_deferral_to_the_operator() {
    let b = Box::new();
    let body = "_lcb_report() { echo \"$1\" > \"$SPIRA_RUN/reported\"; }\n_lcb_drill_busy() { echo busy; }\ncmd_drill >/dev/null 2>&1\nSERVICE_RESULT=exit-code cmd_explain_drill 2>/dev/null";
    b.run(body, &[]);
    assert!(b.read("run/reported").contains("DEFERRED"), "{}", b.read("run/reported"));
}

fn stage_verdict(b: &Box, db: &str, text: &str, mtime: u64) {
    let v = b.p(&format!("run/lc-backup/{db}/drill.verdict"));
    std::fs::create_dir_all(v.parent().unwrap()).unwrap();
    std::fs::write(&v, format!("{text}\n")).unwrap();
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime);
    std::fs::File::options().write(true).open(&v).unwrap().set_modified(at).unwrap();
}

fn import(b: &Box) -> String {
    let (code, out) = b.run(&format!("{NO_REFUSALS}_lcb_run_db() {{ :; }}\n_lcb_run_registry() {{ :; }}\ncmd_run"), &[("SPIRA_LC_STORE_PATH", "store")]);
    assert_eq!(code, Some(0), "{out}");
    out
}

// covers: UC-instance-lifecycle-48
#[test_id(tujvon6)]
#[test]
fn a_passing_verdict_without_a_stamp_is_imported_at_its_mtime_and_a_failing_or_deferred_one_is_not() {
    let b = Box::new();
    stage_verdict(&b, "control", "lc-backup: drill ok — control: fine", 1700000000);
    stage_verdict(&b, "alpha", "lc-backup: drill ok — alpha: fine", 1700000001);
    let out = import(&b);
    assert_eq!(b.read("run/lc-backup/control/drill.ok").trim(), "1700000000");
    assert_eq!(b.read("run/lc-backup/alpha/drill.ok").trim(), "1700000001");
    assert!(out.contains("lc-backup: drill: imported control's drill.ok from its passing verdict (1700000000)"), "{out}");

    let b = Box::new();
    stage_verdict(&b, "control", "lc-backup: drill: control: restore failed", 1700000000);
    stage_verdict(&b, "alpha", "lc-backup: drill: DEFERRED past 0s, never ran — x", 1700000000);
    import(&b);
    assert!(!b.p("run/lc-backup/control/drill.ok").exists() && !b.p("run/lc-backup/alpha/drill.ok").exists());
}

// covers: UC-instance-lifecycle-48
#[test_id(te4988f)]
#[test]
fn an_existing_drill_stamp_is_never_rewritten() {
    let b = Box::new();
    stage_verdict(&b, "control", "lc-backup: drill ok — control: fine", 1700000000);
    std::fs::write(b.p("run/lc-backup/control/drill.ok"), "1600000000\n").unwrap();
    import(&b);
    assert_eq!(b.read("run/lc-backup/control/drill.ok").trim(), "1600000000");
}

// covers: UC-instance-lifecycle-48
#[test_id(tvdwgmc)]
#[test]
fn a_drill_imports_a_passing_verdict_before_a_failing_control_drill_and_keeps_the_stamp() {
    let b = Box::new();
    stage_verdict(&b, "control", "lc-backup: drill ok — control: fine", 1700000000);
    let body = "spira-lc() { echo broken; return 1; }\ncmd_drill --db control";
    let (code, out) = b.run(body, &[("SPIRA_LC_STORE_PATH", "store")]);
    assert_eq!(code, Some(1), "{out}");
    assert_eq!(b.read("run/lc-backup/control/drill.ok").trim(), "1700000000", "{out}");
}

fn registry_run(b: &Box, env: &[(&str, &str)]) -> (Option<i32>, String) {
    b.run(&format!("{NO_REFUSALS}_lcb_run_db() {{ return \"${{DB_RC:-0}}\"; }}\ncmd_run"), env)
}

// covers: UC-instance-lifecycle-49
#[test_id(tgp1yqc)]
#[test]
fn a_registry_backup_is_uploaded_under_its_stamped_key_and_stamped_only_after_the_upload() {
    let b = Box::new();
    let (code, out) = registry_run(&b, &[]);
    assert_eq!(code, Some(0), "{out}");
    let log = b.read("log");
    let steps: Vec<String> = log.lines().map(|l| l.split(' ').take(2).collect::<Vec<_>>().join(" ")).collect();
    assert_eq!(steps, ["test-registry backup", "spira-lc admin-archive-put"], "{log}");
    assert!(log.contains(&format!("--out {}", b.p("sync/test-registry/20231114T221320000Z.sqlite").display())), "{log}");
    assert!(log.contains("--key test-registry-20231114T221320000Z.sqlite"), "{log}");
    assert_eq!(b.read("run/lc-backup.test-registry.ok"), "1700000000 test-registry-20231114T221320000Z.sqlite -\n");
}

// covers: UC-instance-lifecycle-49
#[test_id(tc5mi1y)]
#[test]
fn a_failed_registry_backup_names_its_step_exits_red_uploads_nothing_and_leaves_the_prior_stamp() {
    let b = Box::new();
    std::fs::write(b.p("run/lc-backup.test-registry.ok"), "1600000000 old -\n").unwrap();
    let (code, out) = registry_run(&b, &[("STUB_BACKUP_FAIL", "1")]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("lc-backup: test-registry: backup step failed: disk full"), "{out}");
    assert!(!b.read("log").contains("admin-archive-put"));
    assert_eq!(b.read("run/lc-backup.test-registry.ok"), "1600000000 old -\n");
}

// covers: UC-instance-lifecycle-49
#[test_id(ttyde85)]
#[test]
fn a_failed_registry_upload_names_its_step_exits_red_and_leaves_the_prior_stamp() {
    let b = Box::new();
    std::fs::write(b.p("run/lc-backup.test-registry.ok"), "1600000000 old -\n").unwrap();
    let (code, out) = registry_run(&b, &[("STUB_PUT_FAIL", "1")]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("lc-backup: test-registry: upload step failed: denied"), "{out}");
    assert_eq!(b.read("run/lc-backup.test-registry.ok"), "1600000000 old -\n");
}

// covers: UC-instance-lifecycle-49
#[test_id(tf1tbaj)]
#[test]
fn a_failing_database_still_lets_the_registry_ship_and_only_the_newest_local_copy_is_kept() {
    let b = Box::new();
    let (code, out) = registry_run(&b, &[("DB_RC", "1")]);
    assert_eq!(code, Some(1), "{out}");
    assert!(b.read("run/lc-backup.test-registry.ok").contains("test-registry-"), "{out}");
}

// covers: UC-instance-lifecycle-49
#[test_id(tj6se9s)]
#[test]
fn only_the_newest_local_registry_copy_and_no_orphan_temp_file_is_kept() {
    let b = Box::new();
    let dir = b.p("sync/test-registry");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("orphan.sqlite.tmp"), "").unwrap();
    std::fs::write(dir.join("19700101T000000000Z.sqlite"), "").unwrap();
    let (code, out) = registry_run(&b, &[]);
    assert_eq!(code, Some(0), "{out}");
    let kept: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(kept, ["20231114T221320000Z.sqlite"]);
}

// covers: UC-instance-lifecycle-48
#[test_id(tal0jdm)]
#[test]
fn the_drill_timer_has_one_weekly_slot() {
    let timer = include_str!("../../systemd/lc-backup-drill.timer");
    let slots: Vec<&str> = timer.lines().filter(|l| l.starts_with("OnCalendar=")).collect();
    assert!(slots.len() == 1 && slots[0].starts_with("OnCalendar=Sun "), "{slots:?}");
}

// covers: UC-instance-lifecycle-49
#[test_id(tdfcaq3)]
#[test]
fn age_prints_never_ok_stale_or_unreadable_for_the_registry_and_goes_red_on_all_but_ok() {
    let b = Box::new();
    std::fs::write(b.p("run/lc-backup.alpha.ok"), "1700000000 h -\n").unwrap();
    let age = |stamp: Option<&str>, args: &str| {
        if let Some(s) = stamp {
            std::fs::write(b.p("run/lc-backup.test-registry.ok"), s).unwrap();
        }
        b.run(&format!("cmd_age {args}"), &[])
    };
    let (code, out) = age(None, "--now 1700000100");
    assert!(out.contains("test-registry: never") && code != Some(0), "{code:?} {out}");
    let (code, out) = age(Some("1700000000 k -\n"), "--now 1700000100");
    assert!(out.contains("alpha: ok 100\ntest-registry: ok 100") && code == Some(0), "{code:?} {out}");
    let (code, out) = age(Some("1700000000 k -\n"), "--now 1700005000");
    assert!(out.contains("test-registry: stale 5000") && code != Some(0), "{code:?} {out}");
    let (code, out) = age(Some("garbage\n"), "--now 1700000100");
    assert!(out.contains("test-registry: unreadable") && code != Some(0), "{code:?} {out}");
}

// covers: UC-instance-lifecycle-49
#[test_id(t0ve2ke)]
#[test]
fn a_stale_database_is_listed_before_a_fresh_registry_and_keeps_age_red() {
    let b = Box::new();
    std::fs::write(b.p("run/lc-backup.alpha.ok"), "1600000000 h -\n").unwrap();
    std::fs::write(b.p("run/lc-backup.test-registry.ok"), "1700000000 k -\n").unwrap();
    let (code, out) = b.run("cmd_age --now 1700000100", &[]);
    assert_eq!(out, "alpha: stale 100000100\ntest-registry: ok 100\n");
    assert_eq!(code, Some(1));
}
