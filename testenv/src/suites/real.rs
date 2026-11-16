//! The ports against the host (DESIGN-suites.md §4, §5).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::ports::*;

/// host-check.sh's wall (DESIGN-suites.md §6 D5).
pub const HOST_CHECK_WALL: Duration = Duration::from_secs(30);

pub struct Real {
    suite_dir: PathBuf,
    /// incident.sh: the SPIRA_INCIDENT test seam, else found on PATH. None = not on PATH.
    incident: Option<PathBuf>,
    /// The PATH this run was given; host-check.sh is looked up on it at each call.
    path: String,
}

impl Real {
    pub fn new(s: &Settings, env: &dyn Fn(&str) -> Option<String>) -> Real {
        let get = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let path = env("PATH").unwrap_or_default();
        Real {
            incident: get("SPIRA_INCIDENT").or_else(|| crate::util::which_in(&path, "incident.sh")),
            suite_dir: s.suite_dir.clone(),
            path,
        }
    }
}

/// Wait for `child` up to `wall`; kill it past the wall. Some(exit code) when it exited.
fn wait_wall(child: &mut Child, wall: Duration) -> Option<i32> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return st.code(),
            Ok(None) if start.elapsed() < wall => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

impl Clock for Real {
    fn now(&self) -> u64 {
        crate::batch::now_epoch()
    }
}

impl Emit for Real {
    fn out(&self, line: &str) {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    }
    fn err(&self, line: &str) {
        eprintln!("{line}");
    }
}

// ------------------------------------------------------------------------ the lib.sh seam

/// The record separator before the seam's answer, so a lib.sh log line is never read as it.
pub const MARK: char = '\u{1e}';

/// S1 `conf`: a fixed script on stdin, then one NUL-terminated value (the directory that
/// holds lib.sh). Nothing in argv or the environment (law-payloads-go-on-stdin). bash reads
/// a non-seekable stdin a byte at a time, so the brace group is parsed and run before the
/// value after it is consumed by the `read` inside.
pub const CONF_SCRIPT: &str = r#"{
set -uo pipefail
IFS= read -r -d '' __home
exec </dev/null
. "$__home/lib.sh" || exit 96
__n="${SPIRA_HOME_REPO:-}"; [ -n "$__n" ] || __n="$(spira_home_repo 2>/dev/null)"
printf '\036'
printf 'home_repo=%s\0' "$__n"
printf 'scope_set=%s\0' "${SPIRA_SCOPE_LABEL+1}"
printf 'scope=%s\0' "${SPIRA_SCOPE_LABEL:-}"
printf 'db=%s\0' "${SPIRA_DB:-}"
exit 0
}
"#;

/// Parse the seam's stdout: everything before the last MARK is log noise.
pub fn parse_conf(stdout: &str) -> Result<Conf, String> {
    let Some(i) = stdout.rfind(MARK) else {
        return Err("the conf seam printed no answer".into());
    };
    let ans = &stdout[i + MARK.len_utf8()..];
    let kv: std::collections::BTreeMap<&str, &str> = ans
        .split('\0')
        .filter_map(|r| r.split_once('='))
        .collect();
    let g = |k: &str| kv.get(k).copied().unwrap_or("");
    if g("home_repo").is_empty() {
        return Err("the home repository is unset after sourcing lib.sh".into());
    }
    Ok(Conf {
        home_repo: g("home_repo").to_string(),
        scope_label: (g("scope_set") == "1").then(|| g("scope").to_string()),
        db: g("db").to_string(),
    })
}

pub fn run_conf_seam(lib_dir: &Path) -> Result<Conf, String> {
    // batch-job: runs a gate, build or forge script that takes as long as its work
    let mut child = Command::new("bash")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
        // release's bin/+spira/ on the CHILD's PATH, never only inherited.
        .envs(spira_config::release_env::child_path_env_for_process()?)
        .spawn()
        .map_err(|e| format!("bash: {e}"))?;
    let mut input = CONF_SCRIPT.as_bytes().to_vec();
    input.extend_from_slice(lib_dir.as_os_str().as_encoded_bytes());
    input.push(0);
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(&input);
    }
    let mut out = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        let _ = so.read_to_end(&mut out);
    }
    let rc = child.wait().ok().and_then(|s| s.code()).unwrap_or(127);
    let out = String::from_utf8_lossy(&out);
    if let Some(i) = out.rfind(MARK) {
        let logs = &out[..i];
        if !logs.is_empty() {
            eprint!("{logs}");
        }
    }
    if rc != 0 {
        return Err(format!("lib.sh conf seam exited {rc}"));
    }
    parse_conf(&out)
}

impl LibSeam for Real {
    fn conf(&self) -> Result<Conf, String> {
        run_conf_seam(&self.suite_dir)
    }
}

// ----------------------------------------------------------------------------- scripts

impl Intake for Real {
    fn file(&self, f: &FlakeFiling) -> Result<String, String> {
        let Some(incident) = self.incident.as_ref().filter(|i| fs::File::open(i).is_ok()) else {
            return Err(format!(
                "no intake (incident.sh is not on PATH) — {} flake finding reaches nobody",
                f.suite
            ));
        };
        // batch-job: runs a gate, build or forge script that takes as long as its work
        let mut child = Command::new("bash")
            .arg(incident)
            .arg("file")
            .arg(&f.title)
            .arg("-")
            .env("SPIRA_INCIDENT_TYPE", "bug")
            .env("SPIRA_INCIDENT_PRIORITY", f.priority.to_string())
            .env("SPIRA_INCIDENT_ACTOR", "suites")
            .env("SPIRA_INCIDENT_LABELS", &f.labels)
            .env("SPIRA_INCIDENT_REPO", &f.repo)
            .env("SPIRA_INCIDENT_REF", &f.reference)
            .env("SPIRA_FENRIR_EXEMPT", "1")
            .env("SPIRA_INCIDENT_PATH", &f.path)
            .env("SPIRA_INCIDENT_CAUSE", "suite-flaky")
            .env("SPIRA_DB", &f.db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("incident.sh: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(f.payload.as_bytes());
        }
        let mut out = String::new();
        if let Some(mut so) = child.stdout.take() {
            let _ = so.read_to_string(&mut out);
        }
        match child.wait().ok().and_then(|s| s.code()) {
            Some(0) => Ok(out),
            _ => Err(format!(
                "the intake could not file flake finding for {} — it stays spooled and drain will retry",
                f.suite
            )),
        }
    }
}

impl HostCheck for Real {
    fn count(&self, flag: &str) -> Option<String> {
        // host-check.sh on the launcher's PATH (sp-gypjk); absent or not executable is None.
        let script = crate::util::which_in(&self.path, "host-check.sh")?;
        // batch-job: child is spawned or exec-replaced, not awaited under a deadline
        let mut child = Command::new("bash")
            .arg(&script)
            .arg(flag)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut so = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = so.read_to_string(&mut s);
            s
        });
        let rc = wait_wall(&mut child, HOST_CHECK_WALL);
        let out = reader.join().unwrap_or_default();
        (rc == Some(0))
            .then(|| out.trim_end_matches('\n').to_string())
            .filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_conf_needs_the_mark_and_a_home_repo() {
        assert!(parse_conf("no mark").is_err());
        assert!(parse_conf("\u{1e}home_repo=\0").is_err());
        let c = parse_conf("log\n\u{1e}home_repo=x\0scope_set=1\0scope=s\0").unwrap();
        assert_eq!((c.home_repo.as_str(), c.scope_label.as_deref()), ("x", Some("s")));
    }
}
