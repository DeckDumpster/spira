//! The impure boundary: the capacity pause (shared, cross-process state this crate must
//! not re-derive — `aeon` reads and writes the same pause file, `aeon::capacity` is now
//! both crates' shared implementation, wave 4.26), the three external tools archivist.sh
//! always shelled out to (`ctx-meter.sh`, `archive.sh`, `mail`), and the agent process
//! itself. Production shells out for real (except capacity, in-process now); every test
//! in this crate runs against a recording `FakeSeam`.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct AgentSpec<'a> {
    pub agent_bin: &'a str,
    pub timeout_secs: u64,
    pub system_flag: &'a str,
    pub sysfile: &'a Path,
    pub model: &'a str,
    pub wiki_dir: Option<&'a str>,
    pub task_stdin: &'a str,
    pub logfile: &'a Path,
    pub cwd: &'a Path,
    pub mail_from: &'a str,
}

pub trait Seam {
    /// `capacity_paused` — `Some(seconds_left)` while the account's window is shut,
    /// `None` once it has reopened (or was never shut).
    fn capacity_paused(&self) -> Option<i64>;
    /// `capacity_pause_set <epoch> <reason>`.
    fn capacity_pause_set(&self, at: i64, why: &str);
    /// `capacity_reset_at <logfile>` — `Some(epoch)` if the session ended because the
    /// account refused it, `None` for anything else (including "cannot tell").
    fn capacity_reset_at(&self, logfile: &Path) -> Option<i64>;
    /// A resolved `SPIRA_*` config key (e.g. `SPIRA_ARCHIVIST_EVERY`), after sourcing
    /// lib.sh/conf.sh. Empty if unset.
    fn conf(&self, key: &str) -> String;
    /// Every `SPIRA_*` shell variable after sourcing lib.sh — exported or not. conf.sh
    /// and lib.sh set many keys (`SPIRA_RUN`, `SPIRA_CHAMBER`, `SPIRA_TOKEN_PROJECTS`,
    /// every `SPIRA_ARCHIVIST_*` default) without exporting them, so this process's own
    /// environment cannot be trusted for them the way `sentinel`'s S0 probe already
    /// established — one seam call at startup, reused for the whole run rather than one
    /// `conf()` round trip per key.
    fn probe(&self) -> HashMap<String, String>;
    /// `ctx-meter.sh env <transcript>`, parsed into its `KEY=value` lines.
    fn ctx_meter_env(&self, transcript: &Path) -> HashMap<String, String>;
    /// `archive.sh lineage <session> --json`, raw JSON-lines rows.
    fn archive_lineage(&self, session: &str) -> String;
    /// `mail send operator --from "Archivist <archivist@spira>" --subject <subject>
    /// --kind note --digest`, body on stdin. `Ok(true)`: sent. `Ok(false)`: refused by
    /// mail's own lint (not this crate's concern to retry). `Err`: could not run it.
    fn mail_send_digest(&self, subject: &str, body: &str) -> Result<bool, String>;
    /// Run the agent once, stdout+stderr both to `spec.logfile`, stdin `spec.task_stdin`.
    /// Returns the process exit code (124 is `timeout`'s own convention for "killed by
    /// the clock", unchanged from the bash this replaces).
    fn run_agent(&self, spec: &AgentSpec) -> i32;
    /// `TZ=<tz> date +%F` — today's date in the configured timezone, for the digest's
    /// once-a-calendar-day dedup. Shelled out rather than re-implemented: the IANA tz
    /// database's DST rules are exactly the kind of thing this crate should not carry a
    /// second, partial copy of.
    fn today(&self, tz: &str) -> String;
}

pub struct RealSeam {
    pub lib_sh: PathBuf,
}

impl RealSeam {
    /// `SPIRA_CAPACITY_PAUSE`/`SPIRA_CAPACITY_BACKOFF`/`SPIRA_TRACE_MARK`: lib.sh
    /// literals, not spira-config registry keys (wave4-decomposition.md (b)), so they
    /// come from `conf()` (still a bash round trip — `RealSeam`'s wider "retire conf
    /// re-import seams" is bead 8, not this one) with the same defaults lib.sh always
    /// applied.
    fn capacity_pause_file(&self, run: &str) -> PathBuf {
        let v = self.conf("SPIRA_CAPACITY_PAUSE");
        if v.is_empty() { PathBuf::from(run).join("capacity-pause") } else { PathBuf::from(v) }
    }
    fn capacity_backoff(&self) -> i64 {
        self.conf("SPIRA_CAPACITY_BACKOFF").trim().parse().unwrap_or(900)
    }
    fn trace_mark(&self) -> String {
        let v = self.conf("SPIRA_TRACE_MARK");
        if v.is_empty() { "=== spira attempt".to_string() } else { v }
    }

    fn lib_call(&self, body: &str) -> Result<(bool, String), String> {
        // Sourcing's own stdout goes to OUR stderr (never silently dropped — conf.sh's
        // deprecation warnings live here, e.g. SPIRA_CLAUDE) and is inherited so it
        // still reaches whoever is reading this process's output, the same seam idiom
        // `sentinel::seams::PROBE` uses (`. "$LIB" >&2 || exit 97`).
        let script = format!(r#". "$0" >&2 || exit 97; {body}"#);
        let o = Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(&self.lib_sh)
            // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
            // release's bin/+spira/ on the CHILD's PATH, never only inherited.
            .envs(spira_config::release_env::child_path_env_for_process())
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("lib.sh: {e}"))?;
        Ok((o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned()))
    }
}

impl Seam for RealSeam {
    // wave 4.26: family K (capacity pause) moved to `aeon::capacity`, called in-process
    // here instead of shelling into lib.sh. aeon is the probe's one owner now
    // (wave4-decomposition.md (c)3) — this is a pure READ (never `capacity_probe_maybe`,
    // never a file removal), so a refusal archivist's own session hits still gets
    // recorded (`capacity_pause_set`, below — recording evidence is not probing), but the
    // early-lift probe itself runs only from aeon's own call sites.
    fn capacity_paused(&self) -> Option<i64> {
        let run = self.conf("SPIRA_RUN");
        let now = aeon::util::now_epoch();
        match aeon::capacity::pause_state(&self.capacity_pause_file(&run)) {
            aeon::capacity::PauseState::Paused { until, .. } if until > now => Some(until - now),
            // Fail closed (never "OK"/None on an unreadable file — wave4-decomposition.md
            // (c)3): treat as paused for at least one backoff interval, never as open.
            aeon::capacity::PauseState::Unknown => Some(self.capacity_backoff()),
            _ => None,
        }
    }

    fn capacity_pause_set(&self, at: i64, why: &str) {
        let run = self.conf("SPIRA_RUN");
        let pause = self.capacity_pause_file(&run);
        let ledger = PathBuf::from(&run).join("aeon-ledger.log");
        let now = aeon::util::now_epoch();
        let _ = aeon::capacity::pause_set(&pause, &ledger, now, self.capacity_backoff(), at, why);
    }

    fn capacity_reset_at(&self, logfile: &Path) -> Option<i64> {
        aeon::capacity::reset_at(logfile, &self.trace_mark())
    }

    fn conf(&self, key: &str) -> String {
        self.lib_call(&format!(r#"printf '%s' "${{{key}:-}}""#)).map(|(_, out)| out).unwrap_or_default()
    }

    fn probe(&self) -> HashMap<String, String> {
        // See `lib_call`: sourcing's stdout goes to our stderr, inherited, so conf.sh's
        // own warnings (SPIRA_CLAUDE's deprecation notice among them) are never silently
        // dropped just because this process happens to be reading lib.sh's variables.
        let script = r#". "$0" >&2 || exit 97; for _v in $(compgen -v SPIRA_); do printf '%s\0' "$_v=${!_v:-}"; done"#;
        let o = Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(&self.lib_sh)
            // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
            // release's bin/+spira/ on the CHILD's PATH, never only inherited.
            .envs(spira_config::release_env::child_path_env_for_process())
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output();
        let mut m = HashMap::new();
        if let Ok(o) = o {
            for entry in o.stdout.split(|b| *b == 0) {
                if entry.is_empty() {
                    continue;
                }
                let s = String::from_utf8_lossy(entry);
                if let Some((k, v)) = s.split_once('=') {
                    m.insert(k.to_string(), v.to_string());
                }
            }
        }
        m
    }

    fn ctx_meter_env(&self, transcript: &Path) -> HashMap<String, String> {
        let o = Command::new("ctx-meter.sh").arg("env").arg(transcript).stdin(Stdio::null()).output();
        let mut m = HashMap::new();
        if let Ok(o) = o {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Some((k, v)) = line.split_once('=') {
                    m.insert(k.to_string(), v.to_string());
                }
            }
        }
        m
    }

    fn archive_lineage(&self, session: &str) -> String {
        Command::new("archive.sh")
            .args(["lineage", session, "--json"])
            .stdin(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    fn mail_send_digest(&self, subject: &str, body: &str) -> Result<bool, String> {
        let mut child = Command::new("mail")
            .args(["send", "operator", "--from", "Archivist <archivist@spira>", "--subject", subject, "--kind", "note", "--digest"])
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("mail: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(body.as_bytes());
        }
        let st = child.wait().map_err(|e| format!("mail: {e}"))?;
        Ok(st.success())
    }

    fn run_agent(&self, spec: &AgentSpec) -> i32 {
        let log = match std::fs::File::create(spec.logfile) {
            Ok(f) => f,
            Err(_) => return 1,
        };
        let log_err = match log.try_clone() {
            Ok(f) => f,
            Err(_) => return 1,
        };
        let mut c = Command::new("timeout");
        c.arg(spec.timeout_secs.to_string())
            .arg(spec.agent_bin)
            .args(["-p", "--output-format", "stream-json", "--verbose", "--system-prompt-snapshot", "on"])
            .arg(spec.system_flag)
            .arg(spec.sysfile)
            .args(["--model", spec.model])
            .args(["--allowedTools", "Bash,Read,Grep,Glob,Write"])
            .arg("--dangerously-skip-permissions");
        if let Some(w) = spec.wiki_dir {
            c.args(["--add-dir", w]);
        }
        c.current_dir(spec.cwd)
            .env("SPIRA_MAIL_FROM", spec.mail_from)
            .stdin(Stdio::piped())
            .stdout(log)
            .stderr(log_err);
        let mut child = match c.spawn() {
            Ok(c) => c,
            Err(_) => return 1,
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(spec.task_stdin.as_bytes());
        }
        child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
    }

    fn today(&self, tz: &str) -> String {
        Command::new("date")
            .arg("+%F")
            .env("TZ", tz)
            .stdin(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }
}

/// `SPIRA_HOME` from the environment, else the first directory holding `lib.sh` among
/// the release and cargo layouts around this executable.
pub fn locate_home(env_home: Option<&str>, exe: &Path) -> Option<PathBuf> {
    if let Some(h) = env_home.filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(h));
    }
    let dir = exe.parent()?;
    [dir.join("../spira"), dir.join("../../spira"), dir.join("../../../spira")]
        .into_iter()
        .find(|c| c.join("lib.sh").is_file())
        .map(|c| c.canonicalize().unwrap_or(c))
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct FakeSeam {
        pub paused: RefCell<Option<i64>>,
        pub pause_calls: RefCell<Vec<(i64, String)>>,
        pub reset_at: RefCell<Option<i64>>,
        pub confs: RefCell<HashMap<String, String>>,
        pub ctx_env: RefCell<HashMap<String, HashMap<String, String>>>,
        pub lineage: RefCell<HashMap<String, String>>,
        pub digests_sent: RefCell<Vec<(String, String)>>,
        pub digest_accepts: RefCell<bool>,
        pub agent_rc: RefCell<i32>,
        pub agent_calls: RefCell<Vec<String>>,
        pub today_value: RefCell<String>,
    }

    impl FakeSeam {
        pub fn new() -> FakeSeam {
            let s = FakeSeam::default();
            *s.digest_accepts.borrow_mut() = true;
            s
        }
    }

    impl Seam for FakeSeam {
        fn capacity_paused(&self) -> Option<i64> {
            *self.paused.borrow()
        }
        fn capacity_pause_set(&self, at: i64, why: &str) {
            self.pause_calls.borrow_mut().push((at, why.to_string()));
        }
        fn capacity_reset_at(&self, _logfile: &Path) -> Option<i64> {
            *self.reset_at.borrow()
        }
        fn conf(&self, key: &str) -> String {
            self.confs.borrow().get(key).cloned().unwrap_or_default()
        }
        fn probe(&self) -> HashMap<String, String> {
            self.confs.borrow().clone()
        }
        fn ctx_meter_env(&self, transcript: &Path) -> HashMap<String, String> {
            self.ctx_env.borrow().get(&transcript.to_string_lossy().into_owned()).cloned().unwrap_or_default()
        }
        fn archive_lineage(&self, session: &str) -> String {
            self.lineage.borrow().get(session).cloned().unwrap_or_default()
        }
        fn mail_send_digest(&self, subject: &str, body: &str) -> Result<bool, String> {
            self.digests_sent.borrow_mut().push((subject.to_string(), body.to_string()));
            Ok(*self.digest_accepts.borrow())
        }
        fn run_agent(&self, spec: &AgentSpec) -> i32 {
            self.agent_calls.borrow_mut().push(spec.task_stdin.to_string());
            *self.agent_rc.borrow()
        }
        fn today(&self, _tz: &str) -> String {
            self.today_value.borrow().clone()
        }
    }
}
