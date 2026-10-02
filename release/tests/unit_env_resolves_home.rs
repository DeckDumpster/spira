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
//!     real `~/.config/spira/spira.toml` naming the real production run directory). With
//!     no `spira.toml` anywhere under it, `spira_config::resolve` falls back to pure
//!     derived defaults scoped entirely inside this fixture `HOME`.
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

    // bd: unconditionally one ready bead, for ANY query — the bd schema preflight inside
    // conf.sh never reaches this stub at all (see this file's top doc); the only real
    // consumer is spira-claim's own `ready_count`/`fayth-ready`.
    write_script(&root.join("bin/bd"), "echo '[{\"id\":\"sp-fixture1\"}]'");
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
    let (_code, out) = run_unit(&fx, "spira-watch-notify.service", &[], &[]);
    assert_past_resolution("spira-watch-notify.service", &out);
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
    host.insert("SPIRA_DB".to_string(), fx.home.join(".local/share/spira/db").to_string_lossy().into_owned());
    let rendered = release::units::render("spira-landing-pass.service", &text, &fx.root, &host, None, "prod").expect("render landing-pass");
    let env = unit_env(&rendered);
    let home = env.get("SPIRA_HOME").cloned().unwrap_or_default();
    assert!(!home.is_empty(), "spira-landing-pass.service must set SPIRA_HOME explicitly — it is not part of this bead's self-resolution fix:\n{rendered}");
}
