//! sp-hh599, part 3: "the missing mechanism" — a test that runs a real timer/service
//! unit's binary under THAT UNIT'S OWN RENDERED `Environment=` (exactly as systemd would
//! exec it: `env -i` plus the rendered lines, never this test process's own PATH/HOME) and
//! asserts it gets past config resolution. This is the test sp-kgzql (a Rust binary's own
//! release root never resolved, so a child it shelled into had no `spira-config` on PATH)
//! and sp-ivfu3 (`SPIRA_RUN`/`SPIRA_INSTANCE` read as a bare, wrongly-defaulted env var)
//! would both have caught, and the one this bead's own bug — `spira-claim` refusing
//! outright because `spira-sentinel.service`/`spira-summon.service` carry `SPIRA_RELEASE`/
//! `PATH` and nothing else — slipped through for exactly the same reason: nothing exec'd
//! these binaries under their REAL rendered environment before now.
//!
//! GENERIC, not sentinel-specific: [`run_unit`] takes a template name and an argv suffix,
//! renders the template with [`release::units::render`] (reused rather than invented —
//! this is the same renderer `release activate`/`install-tarball` use against a real
//! release), and execs the binary with EXACTLY that unit's `Environment=` lines via `env
//! -i`. The next binary that needs an unexported variable fails THIS test, not a
//! production pass — add one line to `CASES` (or a bespoke call, as the sentinel and
//! landing-pass cases below are) and it is covered.
//!
//! FIXTURE ONLY, never production (per this bead's own ground rules):
//!   - `HOME` is a fresh, empty tmp directory — never this box's real `$HOME` (which has a
//!     real host config document under `~/.config/spira/`, naming the real production run
//!     directory). With no such document anywhere under it, `spira_config::resolve` falls
//!     back to pure derived defaults scoped entirely inside this fixture `HOME`.
//!   - The fixture release's `spira/` is symlinked to this checkout's own (same trick
//!     `release/tests/session_hook_minimal_env.rs` already uses): the REAL `conf.sh`/
//!     `lib.sh`/`conf.d`/chamber, unmodified — `builder.fayth` genuinely exists, which is
//!     the whole point (this bead's bug is "a real fayth file, incorrectly reported
//!     absent").
//!   - `bd` is a STUB in the fixture's own `bin/` (first on the rendered PATH, so it wins
//!     over anything on this box's real PATH): unconditionally answers one ready bead,
//!     deterministically, never touching a real database. The bd SCHEMA PREFLIGHT inside
//!     `conf.sh` (`spira-config check-bd`) never even reaches it: it runs only when
//!     `<db>/.beads` exists on disk, and nothing does under a fresh fixture `HOME`
//!     (`spira_config::env_bootstrap::check_bd_schema`'s own early return) — so the stub
//!     need not emulate that query at all.
//!   - `systemd-run` is a STUB that only records its own invocation and exits 0 — "stub
//!     the actual systemd-run", exactly as this bead's own part 3 asks. Nothing summons a
//!     real aeon, and `aeon` itself is deliberately ABSENT from the fixture, so even the
//!     stub is never reached unless `aeon` is also staged (the `--summon` case below adds
//!     it on purpose, to prove the summon step is reached; the generic table cases do not,
//!     since reaching "aeon not found on PATH" is itself already proof of getting past
//!     config/no-fayth resolution).
//!   - `landing-pass --pass` is NEVER EXECUTED here: it rebases branches and pushes/opens
//!     pull requests for real (its own unit file's comment says so outright), so there is
//!     no safe fixture to run it against. It is covered the cheap, safe way instead — by
//!     asserting its REAL rendered template already supplies `SPIRA_HOME=` explicitly
//!     (unlike sentinel/summon/watchd, this unit was never part of this bead's bug, and
//!     this assertion is the regression guard that keeps it that way).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("release/ has a parent").to_path_buf()
}

/// Builds one workspace binary and returns its real artifact path, parsed from
/// `--message-format=json` (the target directory may be `CARGO_TARGET_DIR` or the gate's
/// own sccache-backed one — never assumed), the same approach
/// `release/tests/session_hook_minimal_env.rs` already uses.
fn build_bin(package: &str, bin: &str) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(&cargo)
        .args(["build", "--message-format=json", "-p", package, "--bin", bin])
        .current_dir(workspace_root())
        .output()
        .unwrap_or_else(|e| panic!("cannot run cargo build -p {package}: {e}"));
    assert!(out.status.success(), "cargo build -p {package} failed:\n{}", String::from_utf8_lossy(&out.stderr));
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-artifact")
            && v.get("target").and_then(|t| t.get("name")).and_then(serde_json::Value::as_str) == Some(bin)
        {
            if let Some(exe) = v.get("executable").and_then(serde_json::Value::as_str) {
                return PathBuf::from(exe);
            }
        }
    }
    panic!("{bin}'s binary artifact did not appear in cargo's own build output");
}

/// A tiny shell script, written executable, at `path`.
fn write_script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).unwrap();
}

/// One fixture release root: `<root>/bin/{sentinel,spira-claim,spira-config,watchd,bd,
/// systemd-run}`, `<root>/spira` symlinked to this checkout's own real `spira/`
/// (`conf.sh`/`lib.sh`/`conf.d`/chamber unmodified — the real `builder.fayth` is the point).
/// `aeon` is staged only when `with_aeon` is true (see this file's own top doc).
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    systemd_run_log: PathBuf,
    /// Kept alive for as long as the fixture is in use — `TempDir::drop` removes
    /// everything under it.
    _dir: testkit::TempDir,
}

fn build_fixture(tag: &str, with_aeon: bool) -> Fixture {
    // bd: unconditionally one ready bead, for ANY query — the bd schema preflight inside
    // conf.sh never reaches this stub at all (see this file's top doc); the only real
    // consumer is spira-claim's own `ready_count`/`fayth-ready`.
    build_fixture_with_bd(tag, with_aeon, "echo '[{\"id\":\"sp-fixture1\"}]'")
}

/// [`build_fixture`], with the `bd` stub's own script body overridable — sp-xsnid's own
/// label-aware stub (see `fixture_bd_counts_by_label`, below) needs to answer a DIFFERENT
/// count per `--label` query rather than one fixed bead for everything.
fn build_fixture_with_bd(tag: &str, with_aeon: bool, bd_body: &str) -> Fixture {
    let t = testkit::TempDir::new(&format!("sp-hh599-unit-env-{tag}"));
    let root = t.join("release");
    let home = t.join("home");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(workspace_root().join("spira"), root.join("spira")).expect("symlink spira/");

    std::fs::copy(build_bin("sentinel", "sentinel"), root.join("bin/sentinel")).expect("copy sentinel");
    std::fs::copy(build_bin("spira-claim", "spira-claim"), root.join("bin/spira-claim")).expect("copy spira-claim");
    std::fs::copy(build_bin("spira-config", "spira-config"), root.join("bin/spira-config")).expect("copy spira-config");
    std::fs::copy(build_bin("watchd", "watchd"), root.join("bin/watchd")).expect("copy watchd");

    write_script(&root.join("bin/bd"), bd_body);
    // systemd-run: records its own argv (one line) and exits 0 — "stub the actual
    // systemd-run" (this bead's own part 3).
    let systemd_run_log = t.join("systemd-run.log");
    write_script(
        &root.join("bin/systemd-run"),
        &format!("echo \"$@\" >> {} ; exit 0", shell_quote(&systemd_run_log.to_string_lossy())),
    );
    if with_aeon {
        write_script(&root.join("bin/aeon"), "exit 0");
    }
    Fixture { root, home, systemd_run_log, _dir: t }
}

/// A `bd` stub that answers a DIFFERENT, deterministic ready count depending on which
/// substring its own `--label` argument carries — standing in for a real store whose
/// beads are labelled so `builder`/`ops`/`spike` genuinely differ (sp-xsnid: the bug this
/// guards is every fayth reading the SAME count, the whole queue, because every label
/// resolved empty — a stub that answers the FIXED single-bead way `build_fixture` uses
/// cannot tell "correct and distinct" apart from "still widened", since both would NOT be
/// `234` for just one case; this one makes the counts distinguishable so comparing them
/// against each fayth's own, by-hand-resolved expectation is a real assertion, not a
/// coincidence). A query whose `--label` carries none of the three substrings — every
/// OTHER real persona's own label, and the no-partition fayths — answers 0, same as a
/// predicate with nothing to match.
const FIXTURE_BD_SCRIPT: &str = r#"
prev=""
label=""
for a in "$@"; do
    if [ "$prev" = "--label" ]; then label="$a"; fi
    prev="$a"
done
case "$label" in
    *incident*) n=5 ;;
    *spike*) n=3 ;;
    *plan*) n=7 ;;
    *) n=0 ;;
esac
i=0
printf '['
while [ "$i" -lt "$n" ]; do
    [ "$i" -gt 0 ] && printf ','
    printf '{"id":"sp-x%d"}' "$i"
    i=$((i + 1))
done
printf ']\n'
"#;

/// The expected ready count [`FIXTURE_BD_SCRIPT`] answers for a persona's OWN real
/// `FAYTH_LABELS` — the "fully resolved config" ground truth this test compares the
/// rendered-unit-env run against: `builder` (`$SPIRA_PLAN_LABEL` = `plan`) draws 7,
/// `ops` (`$SPIRA_INCIDENT_LABEL` = `incident`) draws 5, `spike` (`$SPIRA_SPIKE_LABEL` =
/// `spike`) draws 3, and every other real chamber persona — none of whose own labels
/// mention any of those three substrings — draws 0.
fn expected_fixture_count(fayth: &str) -> u64 {
    match fayth {
        "builder" => 7,
        "ops" => 5,
        "spike" => 3,
        _ => 0,
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Parse a rendered unit's `Environment=KEY=VALUE` lines — one variable per line, this
/// repo's own template shape (never several on one line).
fn unit_env(rendered: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for l in rendered.lines() {
        if let Some(rest) = l.strip_prefix("Environment=") {
            if let Some((k, v)) = rest.split_once('=') {
                m.insert(k.to_string(), v.to_string());
            }
        }
    }
    m
}

/// The first `ExecStart=` line's command, split on whitespace — every template this test
/// touches uses a plain `<bin> <flags...>` line, no quoting.
fn exec_start_argv(rendered: &str) -> Vec<String> {
    rendered
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Renders `template` from this checkout's own `systemd/` against `fixture`'s release
/// root, execs `argv_override` (or the template's own `ExecStart=` argv when empty) under
/// `env -i` plus EXACTLY the rendered `Environment=` lines, `HOME=<fixture home>` and
/// `extra_env` (operational overrides a real unit never carries but this bead's own part 3
/// explicitly sanctions stubbing, e.g. none for the generic cases). Returns (exit code,
/// combined stdout+stderr).
fn run_unit(fixture: &Fixture, template: &str, argv_override: &[&str], extra_env: &[(&str, &str)]) -> (i32, String) {
    let text = std::fs::read_to_string(workspace_root().join("systemd").join(template))
        .unwrap_or_else(|e| panic!("cannot read systemd/{template}: {e}"));
    let mut host = BTreeMap::new();
    host.insert("SPIRA_RUN".to_string(), fixture.home.join(".local/share/spira/run").to_string_lossy().into_owned());
    host.insert("SPIRA_PATH_TAIL".to_string(), String::new());
    host.insert("SPIRA_DB".to_string(), fixture.home.join(".local/share/spira/db").to_string_lossy().into_owned());
    let rendered = release::units::render(template, &text, &fixture.root, &host, None, "prod")
        .unwrap_or_else(|e| panic!("rendering {template}: {e}"));
    let env = unit_env(&rendered);
    let argv = if argv_override.is_empty() { exec_start_argv(&rendered) } else { argv_override.iter().map(|s| s.to_string()).collect() };
    assert!(!argv.is_empty(), "{template}: no ExecStart= to run");
    let (bin, rest) = (argv[0].clone(), &argv[1..]);

    let mut cmd = Command::new("env");
    cmd.arg("-i").arg(format!("HOME={}", fixture.home.display()));
    for (k, v) in &env {
        cmd.arg(format!("{k}={v}"));
    }
    for (k, v) in extra_env {
        cmd.arg(format!("{k}={v}"));
    }
    cmd.arg(&bin);
    cmd.args(rest);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = cmd.output().unwrap_or_else(|e| panic!("cannot exec {bin} under env -i: {e}"));
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), combined)
}

/// Phrases that mean "this binary never got past resolving its own config/home" — the
/// generic refusal signatures this bead's bug, sp-kgzql and sp-ivfu3 each left behind.
/// None of these may appear in a passing case's output.
const RESOLUTION_REFUSALS: &[&str] = &[
    "SPIRA_HOME is not set",
    "cannot resolve SPIRA_HOME",
    "cannot find lib.sh",
    "conf.sh could not be sourced",
    "spira-config not found on PATH",
];

fn assert_past_resolution(template: &str, output: &str) {
    for marker in RESOLUTION_REFUSALS {
        assert!(!output.contains(marker), "{template}: did not get past config resolution — saw {marker:?} in:\n{output}");
    }
}

/// The generic table: every unit whose binary must resolve its own home WITHOUT being
/// handed `SPIRA_HOME` by the unit itself — `spira-sentinel.service`'s `ExecStart=sentinel`
/// (the plain full-pass entry point is deliberately NOT exercised here: it fans out into
/// strand/CHECK4/5/6/8, each needing its own binary on PATH, which is a much bigger fixture
/// for no more coverage of THIS bug than `--summon-only` already gives) is covered by the
/// dedicated `sentinel_summon_only_reaches_a_real_fayth_ready_query` test below instead, so
/// this table carries `spira-summon.service` and the watchd units.
#[test]
fn spira_summon_service_gets_past_config_resolution_with_no_spira_home_set() {
    let fx = build_fixture("summon-svc", false);
    let (code, out) = run_unit(&fx, "spira-summon.service", &[], &[]);
    assert_past_resolution("spira-summon.service", &out);
    // Positive proof, not just absence of the refusal: CHECK7 actually reached
    // `builder`'s real fayth and read ITS OWN SPIRA_READY_CACHE-free count
    // (sp-hh599: before this bead, this line would instead be "no fayth in the chamber —
    // skipped" for every single fayth, every single pass).
    assert!(out.contains("CHECK7 builder:"), "summon-only never reached CHECK7 builder at all:\n{out}");
    assert!(!out.contains("no fayth in the chamber"), "builder.fayth genuinely exists in the real chamber:\n{out}");
    let _ = code; // summon-only's own rc is incidental to this assertion
}

#[test]
fn spira_watch_notify_service_gets_past_config_resolution_with_no_spira_home_set() {
    let fx = build_fixture("watch-notify-svc", false);
    let (_code, out) = run_unit(&fx, "spira-notify.service", &[], &[]);
    assert_past_resolution("spira-notify.service", &out);
}

/// The templated `spira-watch@.service` (one per watcher, `%i` the watcher name) — same
/// generic PATH/`SPIRA_RELEASE`-only environment, exercised through the dispatcher
/// (`watchd exec <name>`) a real instance of this unit runs.
#[test]
fn spira_watch_at_service_gets_past_config_resolution_with_no_spira_home_set() {
    let text = std::fs::read_to_string(workspace_root().join("systemd/spira-watch@.service")).unwrap();
    assert!(text.contains("%i"), "this unit must still be the templated form this test assumes");
    let fx = build_fixture("watch-at-svc", false);
    let (_code, out) = run_unit(&fx, "spira-watch@.service", &["watchd", "exec", "no-such-watcher"], &[]);
    assert_past_resolution("spira-watch@.service", &out);
}

/// This bead's own named repro, directly: `sentinel --summon builder 1` under
/// `spira-summon.service`'s rendered environment, against a fixture whose `bd` answers one
/// ready bead for `builder`'s real fayth — reaches the summon step (attempts to resolve
/// `aeon`), rather than skipping the fayth as absent. `aeon` is staged too, specifically so
/// the attempt clears that hurdle and reaches the actual summon call — the real
/// `systemd-run` is stubbed, never invoked for real (this bead's own part 3: "stub the
/// actual systemd-run").
#[test]
fn sentinel_summon_cmd_reaches_the_summon_step_against_a_fixture_with_a_ready_bead() {
    let fx = build_fixture("summon-cmd", true);
    let bin_dir = fx.root.join("bin");
    let (code, out) = run_unit(&fx, "spira-summon.service", &["sentinel", "--summon", "builder", "1"], &[]);
    assert_past_resolution("spira-summon.service (--summon builder 1)", &out);
    assert!(!out.contains("no fayth in the chamber"), "{out}");
    // Reached the summon step: either it actually summoned (systemd-run stub invoked —
    // the log this fixture's stub writes is non-empty) or it logged the one OTHER reason
    // a real pass would still not summon (concurrency, pool) — anything but the old
    // "no fayth" short-circuit. The aeon stub above guarantees `aeon` resolves, so the
    // "aeon not found" refusal this crate's own unit tests exercise separately cannot fire
    // here.
    let invoked_systemd_run = std::fs::read_to_string(&fx.systemd_run_log).unwrap_or_default();
    assert!(
        out.contains("CHECK7 builder:") && (out.contains("summoning") || !invoked_systemd_run.trim().is_empty()),
        "summon_cmd did not visibly reach its own summon decision for builder:\n{out}\nsystemd-run log: {invoked_systemd_run:?}"
    );
    let _ = (code, bin_dir);
}

/// The control case: `spira-landing-pass.service` was NEVER part of this bead's bug — it
/// already carries `Environment=SPIRA_HOME=` explicitly — and it is never safe to actually
/// exec (`landing-pass --pass` rebases and pushes for real). Proven the cheap, static way:
/// the real template, rendered against a fixture release exactly like every other case
/// here, supplies a non-empty `SPIRA_HOME`. If a future edit ever drops that line, THIS is
/// what fails, not a production pass discovering it the way sp-hh599 itself was found.
#[test]
fn spira_landing_pass_service_already_carries_spira_home_explicitly_and_is_never_executed() {
    let text = std::fs::read_to_string(workspace_root().join("systemd/spira-landing-pass.service")).unwrap();
    let fx = build_fixture("landing-pass-static", false);
    let mut host = BTreeMap::new();
    host.insert("SPIRA_RUN".to_string(), fx.home.join(".local/share/spira/run").to_string_lossy().into_owned());
    host.insert("SPIRA_PATH_TAIL".to_string(), String::new());
    host.insert("SPIRA_REPO_MAP".to_string(), "/not/the/default/location".to_string());
    host.insert("SPIRA_DB".to_string(), fx.home.join(".local/share/spira/db").to_string_lossy().into_owned());
    let rendered = release::units::render("spira-landing-pass.service", &text, &fx.root, &host, None, "prod").expect("render landing-pass");
    let env = unit_env(&rendered);
    let home = env.get("SPIRA_HOME").cloned().unwrap_or_default();
    assert!(!home.is_empty(), "spira-landing-pass.service must set SPIRA_HOME explicitly — it is not part of this bead's self-resolution fix:\n{rendered}");
}

// =========================================================================================
// sp-xsnid, part 3: the predicate itself, not just whether it resolves at all. sp-hh599
// proved every binary GETS PAST config resolution under the rendered unit env; this
// proves what it gets PAST IT TO is the SAME partition a fully resolved config would
// hand back — never a predicate that silently widened to the whole ready queue because a
// bare config reference resolved empty.
// =========================================================================================

/// Every REAL `.fayth` basename under this checkout's own chamber — never a fixture list
/// that could drift from the real one, since the whole point is proving the real chamber's
/// own personas behave correctly under the real rendered unit env.
fn real_chamber_fayths() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(workspace_root().join("spira/chamber"))
        .expect("read the real chamber")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            (p.extension().and_then(|x| x.to_str()) == Some("fayth"))
                .then(|| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
                .flatten()
        })
        .collect();
    names.sort();
    names
}

/// sp-xsnid's own repro, generalized: `spira-claim fayth-ready <fayth>` under
/// `spira-summon.service`'s rendered environment (no `SPIRA_HOME`, no label exported —
/// exactly `spira-sentinel-prod.service`'s own shape) must answer EXACTLY the count a
/// fully resolved config would — [`expected_fixture_count`], computed from each real
/// persona's OWN known label variable (`builder`→`SPIRA_PLAN_LABEL`, `ops`→
/// `SPIRA_INCIDENT_LABEL`, `spike`→`SPIRA_SPIKE_LABEL`) against [`FIXTURE_BD_SCRIPT`]'s
/// label-aware store — never the bug's own symptom, every persona reading the SAME,
/// WRONG, whole-queue count.
#[test]
fn every_real_fayth_ready_count_matches_the_fully_resolved_config_under_the_rendered_unit_env() {
    let fx = build_fixture_with_bd("predicate-parity", false, FIXTURE_BD_SCRIPT);
    let mut seen_distinct_values = std::collections::HashSet::new();
    for fayth in real_chamber_fayths() {
        let (code, out) = run_unit(&fx, "spira-summon.service", &["spira-claim", "fayth-ready", &fayth], &[]);
        assert_past_resolution(&format!("fayth-ready {fayth}"), &out);
        let want = expected_fixture_count(&fayth);
        assert_eq!(
            (code, out.as_str()),
            (0, want.to_string().as_str()),
            "{fayth}: rc/count must match the fully resolved config exactly, not widen to the whole ready queue"
        );
        seen_distinct_values.insert(want);
    }
    // The bug this guards made EVERY fayth read the SAME number (the whole queue). A
    // fixture where the expected counts are not all identical is what makes the equality
    // assertions above actually discriminate that from "coincidentally still correct".
    assert!(seen_distinct_values.len() > 1, "the fixture must give distinct expected counts, or a widen-to-everything bug would pass unnoticed");
}

/// [`build_fixture_with_bd`], but `spira/` is a REAL directory holding a symlink to every
/// real top-level entry (never a single symlink to the whole tree, which would make
/// `chamber/` itself a symlink — this checkout's own real chamber, which this test must
/// never write into) and `chamber/` is itself a real directory of symlinks to every real
/// `.fayth`/`.md` file PLUS one PLANTED extra, so the planted-refusal test below needs no
/// real chamber mutation.
fn build_fixture_with_planted_fayth(tag: &str, planted_name: &str, planted_body: &str) -> Fixture {
    let fx = build_fixture_with_bd(tag, false, FIXTURE_BD_SCRIPT);
    let real_spira = workspace_root().join("spira");
    let spira_link = fx.root.join("spira");
    std::fs::remove_file(&spira_link).expect("remove the whole-tree symlink build_fixture_with_bd made");
    std::fs::create_dir_all(&spira_link).unwrap();
    for entry in std::fs::read_dir(&real_spira).unwrap().filter_map(|e| e.ok()) {
        let name = entry.file_name();
        if name.to_str() == Some("chamber") {
            continue;
        }
        std::os::unix::fs::symlink(entry.path(), spira_link.join(&name)).unwrap();
    }
    let chamber_link = spira_link.join("chamber");
    std::fs::create_dir_all(&chamber_link).unwrap();
    for entry in std::fs::read_dir(real_spira.join("chamber")).unwrap().filter_map(|e| e.ok()) {
        std::os::unix::fs::symlink(entry.path(), chamber_link.join(entry.file_name())).unwrap();
    }
    std::fs::write(chamber_link.join(format!("{planted_name}.fayth")), planted_body).unwrap();
    fx
}

/// The other half of sp-xsnid's part 3: a PLANTED fayth whose `FAYTH_LABELS` references a
/// config variable that has no `conf.d` default AT ALL (so "fully resolved config" still
/// leaves it unresolved — a genuine config gap, not this bead's own narrow-overlay bug)
/// must REFUSE under the rendered unit env, never answer a count at all — rc 3, the one
/// `sentinel::summon::fayth_ready` maps to a LOUD `CLAIM-ERROR`, never rc 2 ("no fayth")
/// and never a silent widen.
#[test]
fn a_planted_unresolvable_predicate_refuses_under_the_rendered_unit_env() {
    let fx = build_fixture_with_planted_fayth(
        "planted-refusal",
        "spxsnidplanted",
        "FAYTH_NAME=spxsnidplanted\nFAYTH_LABELS=\"$SPIRA_SPXSNID_PLANTED_LABEL\"\nFAYTH_EXCLUDE_LABELS=\"spira-poison\"\n",
    );
    let (code, out) = run_unit(&fx, "spira-summon.service", &["spira-claim", "fayth-ready", "spxsnidplanted"], &[]);
    assert_eq!(code, 3, "a planted unresolvable reference must refuse (rc 3), not widen or read as no-fayth:\n{out}");
    assert!(out.contains("SPIRA_SPXSNID_PLANTED_LABEL"), "the refusal must name the exact unresolved reference:\n{out}");
    assert!(!out.contains("no fayth in the chamber"), "{out}");
}

// =========================================================================================
// sp-8bhnr (P0, LOOP-STOPPING): the landing pass resolves 'spira' to a release directory,
// not its configured checkout. The static check above
// (`spira_landing_pass_service_already_carries_spira_home_explicitly...`) only ever
// covered `spira-landing-pass.service`'s OWN template; the real defect is in CHECK6's
// *dynamic* dispatch (`sentinel/src/dispatch.rs`'s `systemd-run --setenv=...`, never a
// static unit file), which forwards sentinel's OWN resolved `SPIRA_HOME`/`SPIRA_REPO`/
// `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO` into the worker it starts. Sentinel's own `SPIRA_HOME`
// under ITS unit is a release's bundled, non-checkout `spira/` (exactly `build_fixture`'s
// own `root/spira`, were it not a symlink into this live checkout — see
// `non_checkout_release_spira` below for why this test cannot reuse `build_fixture`
// as-is), so sentinel's own `SPIRA_REPO` derives to the release ROOT. `landing-pass --pass`
// is still never executed here (same ground rule as the rest of this file: it rebases and
// pushes for real) — this proves the SHARED mechanism every land mode funnels through
// instead, `spira_config::repos::Registry` (`landing-pass/src/real.rs`'s own
// `repo_registry`, `spira-config repo root` as its CLI door), by exec'ing the REAL
// `spira-config` binary under exactly the env CHECK6 would hand the worker.
// =========================================================================================

/// A release `spira/` that genuinely is NOT a git checkout — unlike every other fixture in
/// this file, which symlinks `root/spira` to this checkout's own real `spira/` (itself
/// part of a real git working tree, so `git -C root/spira rev-parse --show-toplevel` would
/// happily succeed and defeat the whole point here). Only `conf.d` is borrowed, by symlink
/// — the one subdirectory `spira_config::resolve` reads unconditionally — so `resolve()`
/// still succeeds while the directory itself sits in a bare tmp tree with no `.git`
/// anywhere above it.
fn non_checkout_release_spira(release: &Path) -> PathBuf {
    let release_spira = release.join("spira");
    std::fs::create_dir_all(&release_spira).unwrap();
    std::os::unix::fs::symlink(workspace_root().join("spira/conf.d"), release_spira.join("conf.d")).unwrap();
    release_spira
}

/// This bead's own named repro, end to end against the REAL compiled `spira-config`
/// binary: CHECK6 forwards its own resolved `SPIRA_REPO` (the release root — sentinel's
/// `SPIRA_HOME` is a non-checkout release `spira/`, so `derive_repo_filesystem` falls back
/// to its parent) and `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`, but never `SPIRA_REPO_DERIVED`
/// (`sentinel/src/dispatch.rs`'s `setenv` list omits it) into the landing worker, which
/// shares that SAME `SPIRA_HOME`. Before the fix, `repo_override` read the missing
/// `SPIRA_REPO_DERIVED` as THIS process's own deliberate override and handed back the
/// release root for the home repo outright — never consulting the real per-repository
/// row — and `landing-pass`'s own CHECK6 then saw the release directory, not a checkout,
/// and skipped it, exactly as the 07:36:57Z–07:37:16Z pass this bead is named for did.
#[test]
fn landing_worker_env_resolves_spira_to_its_configured_checkout_not_the_release_root() {
    let t = testkit::TempDir::new("sp-8bhnr-landing-worker");
    let release = t.join("spira-releases/deadbeef");
    let release_spira = non_checkout_release_spira(&release);

    // The REAL configured checkout for the home repo 'spira' — a per-repository catalog
    // naming it, and a real git repo carrying the fixture branch landing-pass's own CHECK6
    // must see.
    let checkout = t.join("checkouts/spira");
    std::fs::create_dir_all(&checkout).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git").arg("-C").arg(&checkout).args(args).output().expect("git");
        assert!(out.status.success(), "git -C {} {:?} failed: {}", checkout.display(), args, String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "fixture root", "--author=Fixture <fixture@example.com>"]);
    git(&["checkout", "-q", "-b", "spira/sp-x"]);
    git(&["commit", "-q", "--allow-empty", "-m", "fixture work", "--author=Fixture <fixture@example.com>"]);

    let home = t.join("userhome");
    let catalog = home.join(".config/spira/catalog");
    std::fs::create_dir_all(catalog.parent().unwrap()).unwrap();
    std::fs::write(&catalog, format!("spira | {} | queue.local | local/main |  |\n", checkout.display())).unwrap();

    let spira_config_bin = build_bin("spira-config", "spira-config");

    // Exactly CHECK6's own forwarded keys (`dispatch.rs`'s `setenv` list), plus `HOME` —
    // never `SPIRA_REPO_DERIVED`.
    let mut cmd = Command::new("env");
    cmd.arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg(format!("SPIRA_RELEASE={}", release.display()))
        .arg(format!("PATH={}", release.join("bin").display()))
        .arg(format!("SPIRA_HOME={}", release_spira.display()))
        .arg(format!("SPIRA_REPO={}", release.display()))
        .arg(format!("SPIRA_REPO_MAP={}", catalog.display()))
        .arg("SPIRA_HOME_REPO=spira")
        .arg(&spira_config_bin)
        .arg("repo")
        .arg("root")
        .arg("spira")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd.output().expect("exec spira-config repo root spira");
    let resolved = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        resolved,
        checkout.display().to_string(),
        "'spira' must resolve to its configured checkout, not the release root forwarded as SPIRA_REPO:\nstdout={resolved:?}\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The positive control this bead's own test list asks for: once correctly resolved,
    // the fixture `spira/sp-x` branch landing-pass's own CHECK6 (`git.spira_refs`) looks
    // for is actually there to be seen.
    let refs = Command::new("git").arg("-C").arg(&resolved).args(["for-each-ref", "--format=%(refname:short)", "refs/heads/spira/"]).output().expect("git for-each-ref");
    let seen = String::from_utf8_lossy(&refs.stdout);
    assert!(seen.lines().any(|l| l == "spira/sp-x"), "the fixture branch must be visible in the resolved checkout: {seen:?}");

/// Config refusals a unit's own binary printed in production when it read a registry key
/// from the bare environment (law-a-binary-resolves-the-config-it-reads): none may appear
/// when the binary runs under its rendered `Environment=`.
const UNRESOLVED_CONFIG_REFUSALS: &[&str] = &[
    "SPIRA_RELEASES is not set",
    "SPIRA_DB is not set",
    "SPIRA_RUN is not set",
    "bead store not configured",
    "cannot resolve",
];

/// (template, package, bin) — each unit whose binary reads a registry key the unit itself
/// does not carry.
const CONFIG_READING_UNITS: &[(&str, &str, &str)] = &[
    ("spira-skew.service", "skew", "skew"),
    ("spira-gh-intake.service", "gh-intake", "gh-intake"),
    ("spira-mail-tidy.service", "mail", "mail"),
];

#[test]
fn config_reading_units_resolve_their_keys_under_the_rendered_unit_env() {
    for (template, package, bin) in CONFIG_READING_UNITS {
        let fx = build_fixture(&format!("cfg-{bin}"), false);
        std::fs::copy(build_bin(package, bin), fx.root.join("bin").join(bin)).unwrap_or_else(|e| panic!("copy {bin}: {e}"));
        let (_code, out) = run_unit(&fx, template, &[], &[]);
        for marker in UNRESOLVED_CONFIG_REFUSALS {
            assert!(!out.contains(marker), "{template}: unit env left a key unresolved — saw {marker:?} in:\n{out}");
        }
    }
}
