//! The impure boundary: every place this crate shells out to `bd`, `git`, `systemctl`,
//! `/proc`, or a `lib.sh` function it does not re-implement (DESIGN.md "Non-goals" — `lib.sh`
//! itself is frozen, wave 4's job, never this bead's). Mirrors the exact argv and env
//! contract `spira/lib.sh`'s `bdq`/`bdjson` and `spira/cockpit.sh`'s helpers used, so a
//! fixture or a fake binary already on `PATH` for the bash suites works unmodified against
//! this binary (parity evidence in the delivery report).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The keys `resolve()` computes (or this process itself derives) that [`bootstrap_config`]
/// deliberately never sets into THIS process's own environment, even though the `compgen
/// -v` dump it replaces used to (wave4-decomposition.md row (b): "Hazard: this exports
/// SPIRA_HOME, SPIRA_REPO, SPIRA_REPO_MAP and SPIRA_FAYTHS to every child cockpit-collect
/// spawns. That is exactly what conf.sh forbids" — conf.sh's own comment: an inherited
/// SPIRA_HOME/SPIRA_REPO is "the seam every test suite drives a fixture through", and
/// letting it leak to a child that was just told, by argument, to source a DIFFERENT
/// lib.sh (`lib_call`'s own `home` parameter) would have that child's nested conf.sh
/// silently keep the parent's stale value instead of deriving its own from the file it
/// was just pointed at). [`SPIRA_MAX_AEONS`] joins this set for a narrower reason: it is
/// host policy that must never leak to a child's environment either.
///
/// This set is about the EXPORT side only (this process's own `std::env`, and so every
/// child it spawns) — it has no bearing on reading a value in-process. `SPIRA_MAX_AEONS`
/// and `SPIRA_REPO_MAP` are both ordinary keys in `resolve()`'s own `values` map
/// (`resolve::set!` writes every key there; only the typed shell/export side omits this
/// set), so [`max_aeons`] and `repo_label_keys` read them straight through
/// `spira_config::process::cfg`, the one door, like any other registered key.
///
/// Anything this crate still needs from this set for the CHILD-PROCESS registry bridge is
/// read through [`Boot`]/[`repo_registry`] instead, never `std::env::var`.
const NEVER_EXPORTED: &[&str] = &["SPIRA_HOME", "SPIRA_REPO", "SPIRA_REPO_DERIVED", "SPIRA_REPO_MAP", "SPIRA_FAYTHS", "SPIRA_MAX_AEONS"];

/// [`bootstrap_config`]'s own answer, cached in-process (never in `std::env`) for the one
/// call site that still needs a [`NEVER_EXPORTED`] value for a CHILD process:
/// [`repo_registry`]. Unset outside `main()` — a unit test that exercises that function
/// directly (never calling `bootstrap_config` first) falls back to reading this process's
/// own environment exactly as it did before this bead, so no test needed rewiring for a
/// hazard that only matters once this binary starts spawning children.
struct Boot {
    repo_registry_env: BTreeMap<String, String>,
    repo_map_text: Option<String>,
}

static BOOT: OnceLock<Boot> = OnceLock::new();

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `SPIRA_HOME`, or — mirroring every other rewritten tool in this workspace (`gate-run`'s
/// `default_home`) — `<release>/spira` derived from this binary's own install location
/// (`<release>/bin/cockpit-collect`).
pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("SPIRA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().and_then(|d| d.parent()).map(|r| r.join("spira")))
        .unwrap_or_else(|| PathBuf::from("spira"))
}

/// `spira/cockpit.sh` sourced `lib.sh` unconditionally at its own top level, before
/// dispatching to any probe — and `lib.sh` itself sources `conf.sh`
/// (`spira/lib.sh:24`), which resolves the rest of the repo's own config (`SPIRA_WIKI`,
/// `SPIRA_MAIL`, `SPIRA_CI_PARK_MAX`, and everything else in `SPIRA_CONF_KEYS`) into that
/// process's environment. (`config-fence` flags naming the config file's name even in a
/// comment — this crate never opens it, only benefits from `conf.sh`'s own resolution, so
/// the file is described rather than named here.) Every `*_keys` function then read those as plain `${VAR:-...}`,
/// for free.
///
/// UNTIL WAVE 4.8, this ran a `bash -c '. lib.sh; compgen -v ...'` subprocess once at
/// startup and imported its dump — a re-import seam: conf.sh's own resolution had already
/// moved to `spira_config::resolve()` (sp-eekjm/sp-ubcgo), so this was shelling out purely
/// to re-derive, in bash, values a Rust call can compute directly. It also leaked
/// [`NEVER_EXPORTED`] to this process's own environment, and so to every child this crate
/// spawns (`lib_call`'s own nested `. lib.sh`, `bd`, `git`) — exactly the hazard that set's
/// own doc explains.
///
/// NOW: [`spira_config::resolve::resolve_for_process`] in-process, using this crate's own
/// [`home_dir`] and [`spira_config::resolve::derive_home_repo`]/[`derive_repo_filesystem`]
/// for the two per-copy facts `resolve()` itself never self-locates. Every OTHER resolved
/// key is imported into this process's own environment exactly as before (`conf.sh`'s own
/// `${VAR:=default}` pattern means a variable this process already set — explicitly, by a
/// caller or a test — is never overwritten, so this only ever *adds* resolved defaults,
/// never overrides an explicit value); [`NEVER_EXPORTED`] keys are cached in [`BOOT`]
/// instead, read back only by [`repo_registry`] and [`max_aeons`]. Best-effort: a missing
/// config document/registry or any other resolution failure leaves the environment
/// exactly as it was (the same as every `lib_call` already tolerates a bash failure
/// returning `None`).
pub fn bootstrap_config() {
    let home = home_dir();
    let env_map: BTreeMap<String, String> = std::env::vars().collect();
    let repo_derived = spira_config::resolve::derive_repo_filesystem(&home, &env_map);
    let repo = env_map
        .get("SPIRA_REPO")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_derived.clone());

    let home_repo_default = env_map.get("SPIRA_HOME").filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| home.to_string_lossy().into_owned());
    let mut repo_registry_env = env_map.clone();
    repo_registry_env.insert("SPIRA_HOME".into(), home_repo_default);
    repo_registry_env.insert("SPIRA_REPO".into(), repo.to_string_lossy().into_owned());
    repo_registry_env.insert("SPIRA_REPO_DERIVED".into(), repo_derived.to_string_lossy().into_owned());

    let resolved = spira_config::resolve::resolve_for_process(&home, &repo, &env_map).ok();

    let map_text = resolved
        .as_ref()
        .and_then(|r| {
            let p = r.get("SPIRA_REPO_MAP");
            (!p.is_empty()).then(|| p.to_string())
        })
        .and_then(|p| std::fs::read_to_string(p).ok());
    if let Some(r) = &resolved {
        repo_registry_env.insert("SPIRA_HOME_REPO".into(), r.get("SPIRA_HOME_REPO").to_string());
        repo_registry_env.insert("SPIRA_REPO_MAP".into(), r.get("SPIRA_REPO_MAP").to_string());
    }
    let _ = BOOT.set(Boot { repo_registry_env, repo_map_text: map_text });

    let Some(resolved) = resolved else { return };
    for (k, v) in importable(&resolved, &env_map) {
        std::env::set_var(k, v);
    }
}

/// The pure core of [`bootstrap_config`]'s import rule — every resolved key EXCEPT
/// [`NEVER_EXPORTED`] and one already present in `already_set` (conf.sh's own
/// `${VAR:=default}` pattern: a variable this process already had, explicitly, is never
/// overwritten). Split out as a `BTreeMap`-in-`BTreeMap`-out function, rather than inlined
/// into a loop over live `std::env` calls, so a unit test can check the rule itself without
/// mutating this process's real environment.
fn importable(resolved: &spira_config::resolve::Resolved, already_set: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    resolved
        .values
        .iter()
        .filter(|(k, _)| !NEVER_EXPORTED.contains(&k.as_str()))
        .filter(|(k, _)| !already_set.contains_key(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The repo registry (`spira_config::repos`, sp-37rmg "wave 4.11") — [`Boot`]'s cached
/// answer, or, for a unit test that exercises this function without ever calling
/// [`bootstrap_config`], this process's own environment, exactly as this read it before
/// this bead (see [`BOOT`]'s own doc). Replaces four of this crate's own `lib_call` round
/// trips (one bash subprocess each) with one file read — the repo registry is the
/// most-called family in the whole wave4 decomposition.
pub fn repo_registry() -> spira_config::repos::Registry {
    let home = home_dir();
    match BOOT.get() {
        // `Boot.repo_registry_env` already carries all four registry keys, correctly
        // resolved by `bootstrap_config` (which ran once, in-process, via
        // `resolve_for_process` — no bash at all) — `Registry::new` here is safe
        // (`#[doc(hidden)]`, test-only elsewhere) only because this specific caller
        // supplies an already-complete snapshot, not a bare environment.
        Some(b) => spira_config::repos::Registry::new(b.repo_map_text.as_deref(), &b.repo_registry_env, &home),
        // Never ran bootstrap_config (a unit test exercising this function alone):
        // Registry::from_env resolves the four keys in-process itself, the same one
        // production door every other crate uses now (sp-k6lku, following the
        // structural fix for sp-z3eyk).
        None => spira_config::repos::Registry::from_env(std::env::vars().collect(), &home),
    }
}

/// `SPIRA_MAX_AEONS`, the one door: it is an ordinary key in `resolve()`'s own `values`
/// (never exported to a CHILD process — see [`NEVER_EXPORTED`] — but that is a separate
/// concern from reading it here, in-process).
pub fn max_aeons() -> String {
    spira_config::process::cfg("SPIRA_MAX_AEONS").unwrap_or_default()
}

pub fn try_run_dir() -> Result<PathBuf, String> {
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    spira_config::resolve::resolve_run_dir(&env, &home_dir()).map_err(|e| format!("cockpit-collect: {e}"))
}

/// Panics when [`try_run_dir`] fails; `main` calls `try_run_dir` first and refuses, so only
/// a caller that skipped that check reaches the panic.
pub fn run_dir() -> PathBuf {
    try_run_dir().unwrap_or_else(|e| panic!("{e}"))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

/// `bdq <args>`: the harness's one chokepoint for invoking `bd`, reproduced byte-for-byte
/// enough to matter:
///   - `SPIRA_BDJSON_FIXTURE` set -> `bdsim.py <fixture> <args>` (the test seam every
///     `test-cockpit-*.sh` suite drives; inherited from this process's own environment so a
///     suite that exports it before calling this binary needs no other change — not a
///     registered config key, so this stays a raw env read).
///   - otherwise: refuse with no output when `SPIRA_DB` is empty (never fall through to bd's
///     own auto-discovery — sp-agdzk/sp-25b7s), then
///     `timeout ${BD_TIMEOUT:-180} $SPIRA_BD -C $SPIRA_DB <args>` (`SPIRA_DB`/`SPIRA_BD`
///     through the one door, `spira_config::process::cfg` — no default: an unresolvable
///     config is the same refusal as an empty `SPIRA_DB`, never a silent `bd` on `PATH`),
///     retried up to `SPIRA_BDQ_CONN_RETRIES` (default 2) times while stderr contains
///     "invalid connection".
/// Returns `None` on any failure (non-zero exit, refusal, spawn error) — the caller renders
/// `?`, never 0.
pub fn bdq(args: &[&str]) -> Option<String> {
    if let Ok(fixture) = std::env::var("SPIRA_BDJSON_FIXTURE") {
        if !fixture.is_empty() {
            let out = Command::new("bdsim.py")
                .arg(&fixture)
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            if out.status.success() {
                return Some(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            return None;
        }
    }
    let db = spira_config::process::cfg("SPIRA_DB").ok().filter(|v| !v.is_empty())?;
    let bd_bin = spira_config::process::cfg("SPIRA_BD").ok()?;
    let timeout_s = env_or("BD_TIMEOUT", "180");
    let tries: u32 = env_or("SPIRA_BDQ_CONN_RETRIES", "2").parse().unwrap_or(2).max(1);

    let mut attempt: u32 = 1;
    loop {
        let out = Command::new("timeout")
            .arg(&timeout_s)
            .arg(&bd_bin)
            .arg("-C")
            .arg(&db)
            .args(args)
            .stdin(Stdio::null())
            .output();
        let out = match out {
            Ok(o) => o,
            Err(_) => return None,
        };
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let rc = out.status.code().unwrap_or(1);
        // Collapsed onto bead::bdq::should_retry (sp-pwmlj, wave 4.15) — the same retry
        // decision bdq's own binary makes, rather than a second copy of it here.
        if !bead::bdq::should_retry(rc, attempt, tries, stderr.contains("invalid connection")) {
            return None;
        }
        attempt += 1;
    }
}

/// `json_only`: `sed -n '/^[[{]/,$p'` — drop any banner/warning lines a wrapper printed to
/// stdout before the first line that actually starts a JSON value. Collapsed onto
/// `bead::bdq::json_only` (sp-pwmlj, wave 4.15): this crate's own copy tolerated leading
/// whitespace before the `[`/`{` (`line.trim_start()` then `starts_with`), which `sed -n
/// '/^[[{]/,$p'` — and `bdq`'s own fence — do not; an indented JSON-looking line would have
/// been treated as the payload start here and correctly skipped by the real `bdq`/`bdjson`,
/// a real divergence this collapse fixes rather than a feature to keep.
pub fn json_only(s: &str) -> &str {
    bead::bdq::json_only(s)
}

/// `bdjson <args>` == `bdq <args> --json 2>/dev/null | json_only`.
pub fn bdjson(args: &[&str]) -> Option<String> {
    let mut full: Vec<&str> = args.to_vec();
    full.push("--json");
    bdq(&full).map(|s| json_only(&s).to_string())
}

/// Parse a `bdjson`/`bdq --json` response into a `Vec<Value>`, the shape every probe needs:
/// bd returns either a bare object or an array. Empty/unparseable input (a refusal, per
/// `bdjson`'s contract) returns `None`, never an empty vec — the two are different claims.
pub fn bd_rows(raw: Option<String>) -> Option<Vec<serde_json::Value>> {
    let raw = raw?;
    if raw.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    Some(match v {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    })
}

/// Generic bridge to a `lib.sh` function, the same seam `gate-run`'s `Real` uses for
/// `repo_root`/`spira_landref` (its `REPO_CONTEXT`/`LANDREF_SNIPPET`), generalised: `lib.sh`
/// (wave 4, `law-rust-rewrites-start-from-intent` proposes it last and explicitly "leave
/// lib.sh alone" for now) stays the one place these helpers are defined; this crate calls
/// them exactly as `spira/cockpit.sh` did, never re-derives their logic.
pub fn lib_call(home: &Path, func: &str, args: &[&str]) -> Option<String> {
    lib_call_with_stdin(home, func, args, None)
}

pub fn lib_call_with_stdin(
    home: &Path,
    func: &str,
    args: &[&str],
    stdin: Option<&str>,
) -> Option<String> {
    const SNIPPET: &str = r#"set -uo pipefail
. "$1/lib.sh" >/dev/null 2>&1 || exit 96
f="$2"; shift 2
"$f" "$@"
"#;
    let mut cmd = spira_config::bounded::bounded("bash");
    cmd.arg("-c")
        .arg(SNIPPET)
        .arg("cockpit-collect-lib-bridge")
        .arg(home)
        .arg(func)
        .args(args)
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
        // release's bin/+spira/ on the CHILD's PATH, never only inherited.
        .envs(spira_config::release_env::child_path_env_for_process())
        .stderr(Stdio::null());
    if let Some(input) = stdin {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = cmd.spawn().ok()?;
        use std::io::Write;
        child
            .stdin
            .as_mut()?
            .write_all(input.as_bytes())
            .ok()?;
        let out = child.wait_with_output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            None
        }
    } else {
        cmd.stdin(Stdio::null());
        let out = cmd.output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            None
        }
    }
}

/// `_tsd_slots_sample <fragment-file>` (wave4-decomposition.md row AC, wave 4.35, sp-kelr2):
/// turns the slots probe's own fragment into one `tsd-write` row, in-process — replacing
/// the one [`lib_call`] this crate used to make for it (every OTHER `lib_call` site in this
/// crate is a separate family, untouched by this bead). Reads the fragment directly, never
/// the merged `cockpit.env`: the fragment is this probe's own fresh sample, and the merge's
/// first-wins rule can otherwise repeat a stale one. Still shells to the `tsd-write` binary
/// itself (its flock-protected append is not duplicated here) rather than sourcing all of
/// `lib.sh` first just to reach it, as the bash shim did. Best-effort, like every tsd
/// producer: a failure here is never fatal to the probe.
pub fn tsd_slots_sample(run: &Path, frag: &Path) {
    let text = match std::fs::read_to_string(frag) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Last occurrence wins on a duplicate key, matching the bash's own `while read` loop
    // (each matching line simply overwrites the variable as the file is read through).
    let mut kv: BTreeMap<&str, &str> = BTreeMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            kv.insert(k, v);
        }
    }
    let field = |key: &str| -> String { kv.get(key).copied().unwrap_or("?").to_string() };
    let live = field("SP_SLOTS_LIVE");
    let ceiling = field("SP_SLOTS_CEILING");
    let lanes_live = field("SP_SLOTS_LANES_LIVE");
    let ready = field("SP_SLOTS_READY");
    let paused = field("SP_SLOTS_CAPACITY_PAUSED");
    let _ = spira_config::bounded::bounded("tsd-write")
        .arg("--family")
        .arg("slots")
        .arg("--root")
        .arg(run)
        .args(["--field", &format!("live={live}")])
        .args(["--field", &format!("ceiling={ceiling}")])
        .args(["--field", &format!("lanes_live={lanes_live}")])
        .args(["--field", &format!("ready={ready}")])
        .args(["--field", &format!("capacity_paused={paused}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `git -C <repo> <args>`, stdout on success, `None` on any non-zero exit or spawn failure.
pub fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// `systemctl --user is-active <unit>` -> `Some(true/false)`, or `None` when the unit is
/// unknown to systemd. A unit that cannot be found must never render as "not active"
/// (law-absence-needs-a-positive-control): the caller renders `?`, not `0`.
pub fn unit_active(unit: &str) -> Option<bool> {
    if unit == "?" {
        return None;
    }
    let out = spira_config::bounded::bounded("systemctl")
        .args(["--user", "is-active", unit])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() && !out.status.success() {
        return None;
    }
    Some(s == "active")
}

pub fn unit_show_invocation_id(unit: &str) -> Option<String> {
    let out = spira_config::bounded::bounded("systemctl")
        .args(["--user", "show", unit, "-p", "InvocationID", "--value"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Run an already-on-`PATH` tool by bare name (release/overrides.sh/tokens.sh/yield.sh/
/// tsd-query.sh/cockpit-metrics.py/cockpit-sparklines.py/bdsim.py) — every one of these
/// remains exactly what it was: a separate component this bead does not own, invoked the
/// same way `spira/cockpit.sh` invoked it (law-units-build-what-they-exec: never construct
/// a path to it, resolve it off `PATH` like every other caller in the release).
pub fn run_tool(name: &str, args: &[&str], stdin: Option<&str>) -> Option<String> {
    let mut cmd = spira_config::bounded::bounded(name);
    cmd.args(args).stderr(Stdio::null());
    if let Some(input) = stdin {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = cmd.spawn().ok()?;
        use std::io::Write;
        child.stdin.as_mut()?.write_all(input.as_bytes()).ok()?;
        let out = child.wait_with_output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            None
        }
    } else {
        cmd.stdin(Stdio::null());
        let out = cmd.output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            None
        }
    }
}

pub fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim_end_matches('\n').to_string())
}

pub fn mtime_age_secs(path: &Path) -> Option<i64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = m.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((now() - secs).max(0))
}

/// `/proc/<pid>` exists.
pub fn proc_exists(pid: i64) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// `/proc/<pid>/cmdline`, NUL-joined bytes turned into a space-joined string.
pub fn proc_cmdline(pid: i64) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let parts: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    Some(parts.join(" "))
}

/// `ps -o etimes= -p <pid>` — elapsed seconds since the process started. `None` when the
/// pid is gone or `ps` cannot be read.
pub fn proc_etimes(pid: i64) -> Option<i64> {
    let out = spira_config::bounded::bounded("ps")
        .args(["-o", "etimes=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

pub fn sleep(d: Duration) {
    std::thread::sleep(d);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_only_drops_banner_lines() {
        assert_eq!(json_only("warning: thing\n[1,2,3]\n"), "[1,2,3]\n");
        assert_eq!(json_only("{\"a\":1}"), "{\"a\":1}");
        assert_eq!(json_only("just noise"), "");
        assert_eq!(json_only(""), "");
    }

    // Pins the sp-pwmlj collapse: before it, this function's own copy tolerated leading
    // whitespace before `[`/`{` — `sed -n '/^[[{]/,$p'` (and bdq's real fence) do not.
    #[test]
    fn json_only_requires_column_one_same_as_the_real_fence() {
        assert_eq!(json_only("  [1]\n"), "");
    }

    #[test]
    fn bd_rows_distinguishes_refusal_from_empty_array() {
        assert!(bd_rows(None).is_none());
        assert!(bd_rows(Some("".to_string())).is_none());
        assert!(bd_rows(Some("   ".to_string())).is_none());
        assert_eq!(bd_rows(Some("[]".to_string())), Some(vec![]));
        assert_eq!(
            bd_rows(Some(r#"{"id":"sp-1"}"#.to_string())).map(|v| v.len()),
            Some(1)
        );
    }

    fn resolved_with(pairs: &[(&str, &str)]) -> spira_config::resolve::Resolved {
        spira_config::resolve::Resolved {
            values: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            warnings: vec![],
        }
    }

    #[test]
    fn importable_never_exports_the_forbidden_keys_even_when_resolved() {
        let r = resolved_with(&[
            ("SPIRA_HOME", "/should-never-leak"),
            ("SPIRA_REPO", "/should-never-leak"),
            ("SPIRA_REPO_DERIVED", "/should-never-leak"),
            ("SPIRA_REPO_MAP", "/should-never-leak"),
            ("SPIRA_FAYTHS", "builder groomer"),
            ("SPIRA_MAX_AEONS", "12"),
            ("SPIRA_WIKI", "/var/spira/wiki"),
        ]);
        let got = importable(&r, &BTreeMap::new());
        for forbidden in NEVER_EXPORTED {
            assert!(!got.contains_key(*forbidden), "{forbidden} leaked: {got:?}");
        }
        assert_eq!(got.get("SPIRA_WIKI"), Some(&"/var/spira/wiki".to_string()));
    }

    #[test]
    fn importable_never_overrides_an_already_set_key() {
        let r = resolved_with(&[("SPIRA_ASK_LABEL", "from-resolve")]);
        let already = BTreeMap::from([("SPIRA_ASK_LABEL".to_string(), "from-a-test-fixture".to_string())]);
        let got = importable(&r, &already);
        assert!(!got.contains_key("SPIRA_ASK_LABEL"), "{got:?}");
    }

    #[test]
    fn importable_passes_through_every_other_resolved_key() {
        let r = resolved_with(&[("SPIRA_ASK_LABEL", "needs-ryan"), ("SPIRA_CI_PARK_MAX", "5")]); // literal-ok: fixture/fallback
        let got = importable(&r, &BTreeMap::new());
        assert_eq!(got.get("SPIRA_ASK_LABEL"), Some(&"needs-ryan".to_string())); // literal-ok: fixture/fallback
        assert_eq!(got.get("SPIRA_CI_PARK_MAX"), Some(&"5".to_string()));
    }

    // repo_registry_reads_live_env_when_boot_never_ran DELETED (per Ryan 2026-10-05, one
    // source of config): it asserted the registry picks up a SPIRA_REPO_MAP set in the
    // environment, which is exactly what the registry no longer reads.
    // max_aeons_falls_back_to_live_env_when_boot_never_ran DELETED (per Ryan 2026-10-05, one
    // source of config): it asserted that a bare `SPIRA_MAX_AEONS` env override reaches
    // `max_aeons()` without any `SPIRA_TOML` — exactly the behaviour the one-door law
    // removes. `max_aeons()` is now a one-line call through `spira_config::process::cfg`,
    // whose own resolution/caching is spira-config's tested responsibility; there is no
    // crate-local fallback logic left here to pin with a unit test, and `cfg`'s per-process
    // cache makes a fixture-driven unit test of this one-liner order-dependent rather than
    // meaningful (see the triage guide: only a fresh-binary test may vary config per case).
}
