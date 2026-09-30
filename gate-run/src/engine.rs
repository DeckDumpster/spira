//! The pure decision surface (DESIGN.md "Design"): slugging, key comparison, and — given a
//! snapshot of the state directory and `/proc` — which of the six outcomes applies and what
//! text names it. No file IO and no process inspection happens in this module; `real.rs`
//! gathers a `Snapshot` and `main.rs` renders what this module decides.

/// `printf '%s.%s' "$REPO_NAME" "$BR" | tr -c 'A-Za-z0-9._-' '_'` — one state directory per
/// branch per repository, because a branch name is only unique within its own checkout, and
/// every character outside a safe set folded so a branch with a slash (every branch here has
/// one) does not become a directory tree.
pub fn slug(repo_name: &str, branch: &str) -> String {
    format!("{repo_name}.{branch}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `"<branch commit> <landing-ref commit, or - if unresolvable>"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub tip: String,
    pub base: String,
}

impl Key {
    pub fn new(tip: impl Into<String>, base: Option<String>) -> Key {
        Key {
            tip: tip.into(),
            base: base.unwrap_or_else(|| "-".to_string()),
        }
    }
    pub fn format(&self) -> String {
        format!("{} {}", self.tip, self.base)
    }
    /// A recorded key file's text is stale when it does not equal this one, character for
    /// character (the bash's `[ "$(cat key)" != "$key" ]`).
    pub fn is_stale(&self, recorded: &str) -> bool {
        recorded != self.format()
    }
}

/// What the state directory and `/proc` say, gathered by `real.rs` before any decision.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub dir_exists: bool,
    /// A live process is recorded (`pid` exists, `/proc/<pid>` exists, and its cmdline still
    /// names this binary — `alive()` in the bash).
    pub managed_alive: bool,
    pub pid: Option<i64>,
    pub started: Option<u64>,
    pub recorded_key: Option<String>,
    /// `rc` file content, parsed. `None` when absent (still running or never ran).
    pub rc: Option<i32>,
    pub suites: Option<String>,
    pub out_full: String,
}

/// The last `n` lines of `s`, newline-joined (`tail -n`).
pub fn tail_n(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Outcome of a finished (or died) run — `report()` in the bash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Passed { suites: String, tail: String },
    Failed { rc: i32, out: String },
    Died,
}

pub fn elapsed(now: u64, started: Option<u64>) -> u64 {
    match started {
        Some(s) if now >= s => now - s,
        _ => 0,
    }
}

/// `report()`: given the state directory's `rc` file (or its absence with no live process),
/// decide the verdict. `clear_on_wait` mirrors the bash's own asymmetry: a `wait`-mode caller
/// clears the died state after reading it so the next call starts over; a `--status` probe
/// changes nothing.
pub fn decide_report(snap: &Snapshot) -> Verdict {
    match snap.rc {
        Some(0) => Verdict::Passed {
            suites: snap.suites.clone().unwrap_or_else(|| "-".into()),
            tail: tail_n(&snap.out_full, 5),
        },
        Some(rc) => Verdict::Failed { rc, out: snap.out_full.clone() },
        None => Verdict::Died,
    }
}

/// The 20-line tail printed to stderr alongside a `Died` verdict (the bash's `tail -20 "$OUT"
/// >&2`), kept separate from `decide_report` because it is diagnostic output, not part of the
/// verdict the exit code stands for.
pub fn died_tail(snap: &Snapshot) -> String {
    tail_n(&snap.out_full, 20)
}

/// Render `report()`'s three outcomes into (stdout, exit code) exactly as the bash printed
/// them. `branch`/`repo`/`elapsed_s` are folded in here because the bash interpolated them at
/// print time, not at decision time.
pub fn render_verdict(branch: &str, repo: &str, elapsed_s: u64, key: &str, v: &Verdict) -> (String, i32) {
    match v {
        Verdict::Passed { suites, tail } => {
            let mut out = format!("gate-run: PASSED {branch} in {repo} after {elapsed_s}s\n");
            out += &format!("gate-run: key {key}\n");
            out += &format!("gate-run: gate PASS covered suites: {suites}\n");
            if !tail.is_empty() {
                out += tail;
                if !tail.ends_with('\n') {
                    out.push('\n');
                }
            }
            (out, 0)
        }
        Verdict::Failed { rc, out } => {
            let mut o = format!("gate-run: FAILED {branch} in {repo} after {elapsed_s}s (gate.sh exit {rc})\n");
            o += &format!("gate-run: key {key}\n");
            o += out;
            (o, 1)
        }
        Verdict::Died => {
            // The bash prints this line to stdout and the 20-line tail to stderr; callers
            // here capture combined output, so both are folded into the same string in the
            // order the bash wrote them.
            let o = format!("gate-run: the gate run for {branch} died after {elapsed_s}s without recording a verdict\n");
            (o, 5)
        }
    }
}

/// `--status` mode's whole decision tree, given one snapshot and (separately, because it
/// needs a fresh `/proc` scan the caller may skip once something upstream already answered)
/// whether an unmanaged `gate.sh` for this branch was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusOutcome {
    StillRunning { stale: bool },
    StaleVerdict { recorded_key: String },
    Verdict,
    /// Decided outside `decide_status`, by a caller that scans `/proc` only once this
    /// function has already ruled out a managed run and a finished one (DESIGN.md "Fail-
    /// closed properties kept": `unmanaged_gate()` can never misreport a run this binary is
    /// itself managing).
    NoGate,
}

pub fn decide_status(key: &Key, snap: &Snapshot) -> StatusOutcome {
    if snap.managed_alive {
        let stale = match &snap.recorded_key {
            Some(k) => key.is_stale(k),
            None => false,
        };
        return StatusOutcome::StillRunning { stale };
    }
    if snap.dir_exists {
        if snap.rc.is_some() {
            if let Some(k) = &snap.recorded_key {
                if key.is_stale(k) {
                    return StatusOutcome::StaleVerdict { recorded_key: k.clone() };
                }
            }
        }
        return StatusOutcome::Verdict;
    }
    StatusOutcome::NoGate
}

/// `alive()`: a live process is recorded only when the pid file names a `/proc` entry whose
/// argv still identifies it as one of ours — never from the directory's existence alone
/// (a stale pid file, or one recycled by an unrelated program, must never read as live).
pub fn is_managed_alive(pid: Option<i64>, proc_exists: bool, cmdline: Option<&str>) -> bool {
    match (pid, proc_exists, cmdline) {
        (Some(_), true, Some(cmd)) => cmd.contains("gate-run"),
        _ => false,
    }
}

/// `unmanaged_gate()`: a `gate.sh` running for this exact branch, found by argv rather than a
/// pattern over every process on the box. The branch is matched with a leading space, the same
/// guard the bash used, so a branch name that is a substring of another branch's does not
/// false-match.
pub fn find_unmanaged(branch: &str, self_pid: i64, procs: &[(i64, String)]) -> Option<i64> {
    let needle = format!(" {branch}");
    procs
        .iter()
        .filter(|(pid, _)| *pid != self_pid)
        .find(|(_, cmd)| cmd.contains("gate.sh") && cmd.contains(&needle))
        .map(|(pid, _)| *pid)
}

/// `_suites="$(grep -m1 '^gate: gate PASS covered suites: ' "$OUT")"` — the first matching
/// line from the gate's own combined output, or `-` when the gate did not print one (every
/// FAIL leaves this at `-` rather than a stale scope from a prior PASS).
pub fn extract_suites(gate_out: &str) -> String {
    gate_out
        .lines()
        .find_map(|l| l.strip_prefix("gate: gate PASS covered suites: "))
        .unwrap_or("-")
        .to_string()
}

pub fn render_still_running(branch: &str, key: &str, elapsed_s: u64, pid: i64, stale: bool) -> (String, i32) {
    let suffix = if stale { " (started for an earlier commit)" } else { "" };
    let mut o = format!("gate-run: still running for {branch}{suffix} — {elapsed_s}s so far, pid {pid}\n");
    o += &format!("gate-run: key {key}\n");
    (o, 2)
}

pub fn render_stale_verdict(branch: &str, recorded_key: &str, key: &str) -> (String, i32) {
    let o = format!(
        "gate-run: the recorded verdict for {branch} is for a different tree — key was {recorded_key}, now {key}. A rebase, a new commit, or the base moving invalidated it.\n"
    );
    (o, 4)
}

pub fn render_unmanaged(branch: &str, key: &str, pid: i64) -> (String, i32) {
    let mut o = format!("gate-run: a gate.sh for {branch} is running outside this runner, pid {pid} — its verdict reaches nobody\n");
    o += &format!("gate-run: key {key}\n");
    (o, 2)
}

pub fn render_no_gate(branch: &str, key: &str) -> (String, i32) {
    (format!("gate-run: no gate has run or finished for {branch} — key {key}\n"), 3)
}

pub fn render_wait_started(branch: &str, repo: &str, poll: u64) -> String {
    format!("gate-run: started the gate for {branch} in {repo} — this call waits up to {poll}s\n")
}

pub fn render_wait_timeout(branch: &str, poll: u64, elapsed_s: u64) -> (String, i32) {
    let mut o = format!("gate-run: still running for {branch} after {elapsed_s}s — run the same command again\n");
    o += &format!("gate-run: each call waits up to {poll}s; do not end your turn while this is unfinished\n");
    (o, 2)
}

/// Printed to stderr when a run recorded under one key is found stale on a `wait` call, just
/// before it is stopped and a fresh one started.
pub fn render_stale_restart() -> &'static str {
    "gate-run: the recorded run is for another commit — starting a new one\n"
}

/// Printed to stderr when the previous run's state directory has neither a live process nor a
/// recorded exit code — it did not survive, so a fresh one is started rather than answering
/// from (or silently re-polling) a corpse.
pub fn render_gone_restart(branch: &str) -> String {
    format!("gate-run: the previous run for {branch} did not finish and is gone — starting a new one\n")
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
