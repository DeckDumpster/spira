//! The impure boundary: every place this crate shells out to `bd`, `git`, `systemctl`,
//! `/proc`, or a `lib.sh` function it does not re-implement (DESIGN.md "Non-goals" — `lib.sh`
//! itself is frozen, wave 4's job, never this bead's). Mirrors the exact argv and env
//! contract `spira/lib.sh`'s `bdq`/`bdjson` and `spira/cockpit.sh`'s helpers used, so a
//! fixture or a fake binary already on `PATH` for the bash suites works unmodified against
//! this binary (parity evidence in the delivery report).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
/// for free. This binary calls `lib.sh` functions one at a time through [`lib_call`], each
/// in its own short-lived bash subprocess — so without this bootstrap, none of that
/// cascade ever reaches THIS process's environment, and every config-default read here
/// would see an unset var where the bash saw a resolved one (found via parity testing
/// against the retired bash: `statute_keys`'s `SP_STATUTE_PAGE_N` read `?` here and a real
/// count there, because `SPIRA_WIKI` was never resolved).
///
/// Runs once at process start (`main()`, before any subcommand): source `lib.sh` in a
/// bash subprocess that inherits this process's own environment, dump the result, and
/// import every key back. `conf.sh`'s own `${VAR:=default}` pattern means a variable this
/// process already set (explicitly, by a caller or a test) is never overwritten — the
/// subprocess sees it as already-set and leaves it alone — so this only ever *adds*
/// resolved defaults, never overrides an explicit value. Best-effort: a missing `lib.sh`,
/// an unreadable config, or any other failure leaves the environment exactly as it was
/// (the same as every `lib_call` already tolerates a bash failure returning `None`).
pub fn bootstrap_config() {
    // `env -0` alone is not enough: `conf.sh` sets most of `SPIRA_CONF_KEYS` via a plain
    // `: "${VAR:=default}"`, with NO `export` (only a named subset — `SPIRA_WIKI`,
    // `SPIRA_ASK_LABEL`, `SPIRA_CI_PARK_MAX`, about 70 of the ~230 keys — ever gets
    // exported; `SPIRA_QUEUE_BATCH_MAX` and most queue/suite/batch keys do not). Those
    // reach `cockpit.sh`'s own code anyway because sourcing (`.`) shares the same shell's
    // variable table — no export needed for that. `compgen -v` + indirect expansion
    // (`${!_v}`) dumps every shell variable, exported or not, so this binary's `std::env`
    // sees the same values `cockpit.sh`'s bash code did, not only the ones bash would have
    // handed to a grandchild process.
    const SKIP: &str = "_|_v|PWD|OLDPWD|SHLVL|IFS|PS1|PS2|PS4|PROMPT_COMMAND|GROUPS|HOSTNAME|HOSTTYPE|MACHTYPE|OSTYPE|RANDOM|SECONDS|UID|EUID|PPID|LINENO|OPTIND|SHELLOPTS|PIPESTATUS|FUNCNAME|BASHPID|BASH|BASHOPTS|COMP_WORDBREAKS|BASH_ARGC|BASH_ARGV|BASH_LINENO|BASH_SOURCE|BASH_VERSINFO|BASH_VERSION|BASH_SUBSHELL|BASH_COMMAND";
    const SNIPPET: &str = r#"set -uo pipefail
. "$1/lib.sh" >/dev/null 2>&1 || exit 96
set +u
for _v in $(compgen -v); do
    case "|$2|" in *"|$_v|"*) continue ;; esac
    printf '%s\0' "${_v}=${!_v}" 2>/dev/null
done
"#;
    let home = home_dir();
    let out = Command::new("bash")
        .arg("-c")
        .arg(SNIPPET)
        .arg("cockpit-collect-bootstrap")
        .arg(&home)
        .arg(SKIP)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else { return };
    if !out.status.success() {
        return;
    }
    for entry in out.stdout.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(entry) else { continue };
        let Some((k, v)) = s.split_once('=') else { continue };
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
}

pub fn run_dir() -> PathBuf {
    std::env::var_os("SPIRA_RUN")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join("../run"))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

/// `bdq <args>`: the harness's one chokepoint for invoking `bd`, reproduced byte-for-byte
/// enough to matter:
///   - `SPIRA_BDJSON_FIXTURE` set -> `bdsim.py <fixture> <args>` (the test seam every
///     `test-cockpit-*.sh` suite drives; inherited from this process's own environment so a
///     suite that exports it before calling this binary needs no other change).
///   - otherwise: refuse with no output when `SPIRA_DB` is empty (never fall through to bd's
///     own auto-discovery — sp-agdzk/sp-25b7s), then
///     `timeout ${BD_TIMEOUT:-180} ${SPIRA_BD:-bd} -C $SPIRA_DB <args>`, retried up to
///     `SPIRA_BDQ_CONN_RETRIES` (default 2) times while stderr contains "invalid connection".
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
    let db = std::env::var("SPIRA_DB").ok().filter(|v| !v.is_empty())?;
    let bd_bin = env_or("SPIRA_BD", "bd");
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

/// The repo registry (`spira_config::repos`, sp-37rmg "wave 4.11"), built in-process from
/// THIS process's own environment — safe only because [`bootstrap_config`] has already
/// imported every shell variable `conf.sh`/`lib.sh` would have resolved (`SPIRA_HOME_REPO`,
/// `SPIRA_REPO`, `SPIRA_REPO_DERIVED`, `SPIRA_REPO_MAP`), exactly as `spira_repos`/
/// `repo_root`/`repo_land`/`spira_home_repo` read them in bash. Replaces four of this
/// crate's own `lib_call` round trips (one bash subprocess each, previously) with one file
/// read — the repo registry is the most-called family in the whole wave4 decomposition.
pub fn repo_registry() -> spira_config::repos::Registry {
    let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let home = home_dir();
    let map_text = env_map
        .get("SPIRA_REPO_MAP")
        .filter(|p| !p.is_empty())
        .and_then(|p| std::fs::read_to_string(p).ok());
    spira_config::repos::Registry::new(map_text.as_deref(), &env_map, &home)
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
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(SNIPPET)
        .arg("cockpit-collect-lib-bridge")
        .arg(home)
        .arg(func)
        .args(args)
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

/// `git -C <repo> <args>`, stdout on success, `None` on any non-zero exit or spawn failure.
pub fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
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
    let out = Command::new("systemctl")
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
    let out = Command::new("systemctl")
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
    let mut cmd = Command::new(name);
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
    let out = Command::new("ps")
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
}
