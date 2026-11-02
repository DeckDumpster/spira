//! `spira/schema.sh` and `spira/schema-apply.sh` against a planted `bd`: the declaration
//! resolves and fails closed, `check` tells an absent addition from a store it could not ask,
//! and `apply` puts the model into a store once, says so, and refuses an engine it cannot use.
//!
//! Both scripts run from a copy beside a planted `conf.sh`, a `bd` that keeps its store as files
//! and a `timeout` that runs its command with no timer, so nothing here reads the box's config,
//! starts a database or waits on a clock: a probe that timed out is the planted `bd` exiting
//! 124, as `timeout` reports one. (A real `timeout` can also cost a fixed poll interval per call,
//! which alone would put one `check` over a unit test's budget.) Each test runs one script once.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use testkit::TempDir;

/// The planted `bd`. `-C <db>` names a directory holding the store: one file per config key,
/// an empty `_is_work` or `spira_priority_range` once that addition is in. A file named for a
/// failure plants it: `embedded`, `refuse` (an `alter table` fails with its contents),
/// `sql-error` (every count probe fails with its contents), `count` (every count probe answers
/// it), `stall-<key>` (reading that key times out). Every call is appended to `calls`. A probe
/// is answered only when it asks the exact question, so a query that lost a predicate fails.
const BD: &str = r#"#!/usr/bin/env bash
db=""; [ "${1:-}" = -C ] && { db="$2"; shift 2; }
printf '%s\n' "$*" >>"$db/calls"
case "${1:-}" in
sql)
    q="$2"
    [ -f "$db/embedded" ] && { echo "Error: 'bd sql' is not yet supported in embedded mode" >&2; exit 1; }
    case "$q" in
    "select 1;") printf '+---+\n| 1 |\n1\n'; exit 0 ;;
    "alter table issues add column _is_work "*|"alter table issues add constraint spira_priority_range "*)
        [ -f "$db/refuse" ] && { cat "$db/refuse" >&2; exit 1; }
        case "$q" in
        *"_is_work"*) f=_is_work ;;
        *) f=spira_priority_range ;;
        esac
        [ -f "$db/$f" ] && { echo "Error: $f already exists" >&2; exit 1; }
        : >"$db/$f"; exit 0 ;;
    esac
    [ -f "$db/sql-error" ] && { cat "$db/sql-error" >&2; exit 1; }
    case "$q" in
    "select count(*) from information_schema.columns where table_name='issues' and column_name='_is_work';") f=_is_work ;;
    "select count(*) from information_schema.table_constraints where table_name='issues' and constraint_type='CHECK' and constraint_name='spira_priority_range';") f=spira_priority_range ;;
    *) echo "planted bd: unexpected query: $q" >&2; exit 1 ;;
    esac
    n=0; [ -f "$db/$f" ] && n=1
    [ -f "$db/count" ] && n="$(cat "$db/count")"
    printf '+-------+\n| count |\n%s\n' "$n" ;;
config)
    case "$2" in
    get) [ -f "$db/stall-$3" ] && exit 124
         cat "$db/$3" 2>/dev/null; echo ;;
    set) printf '%s' "$4" >"$db/$3" ;;
    esac ;;
*) echo "planted bd: unexpected call: $*" >&2; exit 1 ;;
esac
"#;

/// The planted `timeout`: the deadline is dropped and the command's own status passes through.
const TIMEOUT: &str = "#!/bin/sh\nshift\nexec \"$@\"\n";

/// The ask name the planted `conf.sh` configures — not the shipped default, so a resolution
/// that ignored the config would show here.
const ASK: &str = "ask-from-planted-conf";

/// What `schema.sh custom-types` and `schema.sh statuses` declare, as the store holds them.
const TYPES: &str = "escalation,event,gate,proposal";
const STATUSES: &str = "approved,premise_rejected,retired,archived";

struct Harness {
    t: TempDir,
}

fn tree_script(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira").join(name);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The whitespace-separated words of each line of `out`.
fn rows(out: &str) -> Vec<Vec<&str>> {
    out.lines().map(|l| l.split_whitespace().collect()).collect()
}

/// The lines of `contract` output under the heading that starts with `heading`.
fn section<'a>(out: &'a str, heading: &str) -> Vec<&'a str> {
    out.lines()
        .skip_while(|l| !l.starts_with(heading))
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .collect()
}

impl Harness {
    fn new() -> Harness {
        let t = TempDir::new("schema-sh");
        for d in ["spira", "bin", "db", "home"] {
            fs::create_dir_all(t.path().join(d)).unwrap();
        }
        testkit::write_exe(t.path().join("spira/schema.sh"), &tree_script("schema.sh"));
        testkit::write_exe(t.path().join("spira/schema-apply.sh"), &tree_script("schema-apply.sh"));
        fs::write(t.path().join("spira/conf.sh"), format!("SPIRA_ASK_LABEL={ASK}\n")).unwrap();
        testkit::write_exe(t.path().join("bin/bd"), BD);
        testkit::write_exe(t.path().join("bin/timeout"), TIMEOUT);
        Harness { t }
    }

    /// A store already carrying the whole declared model.
    fn correct() -> Harness {
        let h = Harness::new();
        h.plant("types.custom", TYPES);
        h.plant("status.custom", STATUSES);
        h.plant("_is_work", "");
        h.plant("spira_priority_range", "");
        h
    }

    fn db(&self) -> PathBuf {
        self.t.path().join("db")
    }

    fn plant(&self, file: &str, body: &str) {
        fs::write(self.db().join(file), body).unwrap();
    }

    fn unplant(&self, file: &str) {
        fs::remove_file(self.db().join(file)).unwrap();
    }

    fn has(&self, file: &str) -> bool {
        self.db().join(file).exists()
    }

    fn read(&self, file: &str) -> String {
        fs::read_to_string(self.db().join(file)).unwrap_or_default()
    }

    /// `<script> <args>`: (exit code, stdout, stderr).
    fn run(&self, script: &str, args: &[&str]) -> (i32, String, String) {
        let root = self.t.path();
        let out = Command::new("bash")
            .arg(root.join("spira").join(script))
            .args(args)
            .env_clear()
            .env("PATH", format!("{}:{}:/usr/bin:/bin", root.join("spira").display(), root.join("bin").display()))
            .env("HOME", root.join("home"))
            .env("SPIRA_DB", self.db())
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn schema(&self, args: &[&str]) -> (i32, String, String) {
        self.run("schema.sh", args)
    }

    /// `schema-apply.sh`, its stdout and stderr together as an operator reads them.
    fn apply(&self) -> (i32, String) {
        let (rc, out, err) = self.run("schema-apply.sh", &[]);
        (rc, format!("{out}{err}"))
    }

    /// The writes the planted store saw: every `config set` and `alter table`.
    fn writes(&self) -> Vec<String> {
        self.read("calls").lines().filter(|l| l.starts_with("config set") || l.contains("alter table")).map(str::to_string).collect()
    }
}

// covers: UC-config-store-preflight-19
#[test]
fn the_contract_maps_every_kind_to_its_type_and_resolves_every_name() {
    let (rc, out, err) = Harness::new().schema(&["contract"]);
    assert_eq!(rc, 0, "{err}");
    let kinds = rows(&out);
    for (kind, ty) in [
        ("work", "task"),
        ("event", "event"),
        ("escalation", "escalation"),
        ("proposal", "proposal"),
        ("idea", "proposal"),
        ("insight", "chore"),
        ("gate", "gate"),
    ] {
        assert!(kinds.contains(&vec![kind, "->", "issue_type", ty]), "kind {kind} must map to {ty}:\n{out}");
    }
    let names = section(&out, "NAMES");
    assert!(names.len() >= 10, "the NAMES section lists every declared name:\n{out}");
    for l in &names {
        assert_eq!(l.split_whitespace().count(), 2, "a declared name resolved to nothing: [{l}]");
    }
    assert!(names.iter().any(|l| l.split_whitespace().eq(["ask", ASK])), "the ask name comes from the config:\n{out}");
    let dims: Vec<&str> = section(&out, "DIMENSIONS").iter().filter_map(|l| l.split_whitespace().next()).collect();
    assert!(dims.contains(&"repo") && dims.contains(&"gate"), "{dims:?}");
}

// covers: UC-config-store-preflight-19
#[test]
fn an_undeclared_kind_or_name_fails_closed_with_nothing_on_stdout() {
    let h = Harness::new();
    for args in [&["type-of", "not-a-kind"][..], &["name", "definitely-not-a-name"], &["name", ""]] {
        let (rc, out, err) = h.schema(args);
        assert_ne!(rc, 0, "{args:?} must fail closed, printed {out:?}");
        assert!(out.is_empty(), "{args:?} printed {out:?}");
        assert!(err.contains("schema: no such"), "{args:?} must say why: {err}");
    }
}

// covers: UC-config-store-preflight-19
#[test]
fn custom_types_register_every_non_builtin_kind_and_never_chore() {
    let (rc, out, err) = Harness::new().schema(&["custom-types"]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(out.lines().collect::<Vec<_>>().join(","), TYPES, "chore is bd's own type, never registered");
}

// covers: UC-config-store-preflight-19
#[test]
fn check_passes_on_a_store_carrying_the_declared_model() {
    let (rc, out, err) = Harness::correct().schema(&["check"]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("store matches the declaration"), "{out}");
}

// covers: UC-config-store-preflight-19
#[test]
fn check_reports_a_store_without_is_work_as_missing() {
    let h = Harness::correct();
    h.unplant("_is_work");
    let (rc, _, err) = h.schema(&["check"]);
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("MISSING generated column: _is_work"), "{err}");
}

// covers: UC-config-store-preflight-19
#[test]
fn check_reports_a_store_without_the_priority_check_as_missing() {
    let h = Harness::correct();
    h.unplant("spira_priority_range");
    let (rc, _, err) = h.schema(&["check"]);
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("MISSING constraint: spira_priority_range"), "{err}");
}

// covers: UC-config-store-preflight-19
#[test]
fn a_probe_that_timed_out_is_cannot_tell_never_missing() {
    let h = Harness::correct();
    h.plant("stall-types.custom", "");
    let (rc, _, err) = h.schema(&["check"]);
    assert_eq!(rc, 3, "{err}");
    assert!(err.contains("cannot tell: types.custom timed out after "), "{err}");
    assert!(!err.contains("MISSING"), "a store that did not answer is not a store that lacks: {err}");
}

// covers: UC-config-store-preflight-19
#[test]
fn drift_outranks_cannot_tell_and_both_are_printed() {
    let h = Harness::correct();
    h.plant("stall-types.custom", "");
    h.unplant("_is_work");
    let (rc, _, err) = h.schema(&["check"]);
    assert_eq!(rc, 1, "a known absence is drift even beside a probe that could not answer: {err}");
    assert!(err.contains("MISSING generated column: _is_work"), "{err}");
    assert!(err.contains("cannot tell: types.custom timed out"), "{err}");
}

// covers: UC-config-store-preflight-19
#[test]
fn the_contract_shows_a_store_read_that_timed_out_as_cannot_tell() {
    let h = Harness::correct();
    h.plant("stall-types.custom", "");
    let (_, out, _) = h.schema(&["contract"]);
    assert!(out.contains("custom types:    schema: cannot tell: types.custom timed out"), "{out}");
}

// covers: UC-config-store-preflight-19
#[test]
fn a_probe_that_errored_is_cannot_tell_carrying_the_engines_error() {
    let h = Harness::correct();
    h.plant("sql-error", "Error: connection refused");
    let (rc, _, err) = h.schema(&["check"]);
    assert_eq!(rc, 3, "{err}");
    assert!(err.contains("cannot tell: _is_work probe: Error: connection refused"), "{err}");
    assert!(!err.contains("MISSING"), "{err}");
}

// covers: UC-config-store-preflight-19
#[test]
fn a_probe_answering_no_count_is_cannot_tell() {
    let h = Harness::correct();
    h.plant("count", "oops");
    let (rc, _, err) = h.schema(&["check"]);
    assert_eq!(rc, 3, "{err}");
    assert!(err.contains("cannot tell: _is_work probe returned no count: "), "{err}");
    assert!(!err.contains("MISSING"), "{err}");
}

// covers: UC-config-store-preflight-20
#[test]
fn apply_puts_the_whole_model_into_a_bare_store_and_verifies_it() {
    let h = Harness::new();
    let (rc, out) = h.apply();
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("_is_work: adding generated column"), "{out}");
    assert!(out.contains("spira_priority_range: adding CHECK"), "{out}");
    assert!(out.contains("store matches the declaration"), "the closing check's verdict is shown: {out}");
    assert_eq!(h.read("types.custom"), TYPES);
    assert_eq!(h.read("status.custom"), STATUSES);
    assert!(h.has("_is_work") && h.has("spira_priority_range"), "{out}");
    let calls = h.read("calls");
    assert!(
        calls.contains("_is_work tinyint as (case when issue_type in ('task','bug','feature','epic','chore','spike') then 1 else 0 end) stored;"),
        "the column is computed from the declared work types:\n{calls}"
    );
    assert!(calls.contains("check (priority between 0 and 4);"), "{calls}");
}

// covers: UC-config-store-preflight-20
#[test]
fn apply_on_a_store_already_carrying_the_model_writes_nothing() {
    let h = Harness::correct();
    let (rc, out) = h.apply();
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("custom types already exact") && out.contains("custom statuses already exact"), "{out}");
    assert!(out.contains("_is_work already present") && out.contains("spira_priority_range already present"), "{out}");
    assert_eq!(h.writes(), Vec::<String>::new(), "a second apply must change nothing");
}

// covers: UC-config-store-preflight-20
#[test]
fn a_refused_statement_is_printed_with_the_engines_error_and_nothing_claims_applied() {
    let h = Harness::new();
    h.plant("refuse", "engine says no: planted");
    let (rc, out) = h.apply();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("alter table issues add column _is_work"), "the failing statement is named: {out}");
    assert!(out.contains("engine says no: planted"), "the engine's error is carried: {out}");
    assert!(out.contains("NOT applied"), "{out}");
    assert!(!out.contains("applied. verifying"), "{out}");
}

// covers: UC-config-store-preflight-20
#[test]
fn apply_whose_closing_check_cannot_tell_says_applied_not_verified() {
    let h = Harness::correct();
    h.plant("stall-types.custom", "");
    let (rc, out) = h.apply();
    assert_eq!(rc, 4, "{out}");
    assert!(out.contains("applied, NOT verified"), "{out}");
    assert!(out.contains("this is not drift"), "{out}");
}

// covers: UC-config-store-preflight-21
#[test]
fn apply_refuses_an_embedded_store_naming_cause_and_remedy() {
    let h = Harness::new();
    h.plant("embedded", "");
    let (rc, out) = h.apply();
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("EMBEDDED mode"), "the engine is named as the cause: {out}");
    assert!(out.contains("_is_work") && out.contains("spira_priority_range"), "what could not be applied is named: {out}");
    assert!(out.contains("server-mode"), "the remedy is named: {out}");
    assert!(!out.contains("applied. verifying"), "it must not claim it applied anything: {out}");
    assert_eq!(h.writes(), Vec::<String>::new(), "{out}");
}
