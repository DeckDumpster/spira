//! The `Units` port over a `systemctl` program, for a host where an override names one.
//!
//! D-Bus ([`crate::units::SystemdUnits`]) is the backend; this one exists so a test double or a
//! wrapper named by `SPIRA_SYSTEMCTL` keeps deciding what a tool sees instead of being bypassed
//! onto the live user manager. The text `systemctl show` prints is read here, once, into the
//! port's typed [`UnitDetail`]; no caller reads it.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::units::{state_of_word, Exit, UnitCompletions, UnitDetail, UnitFacts, UnitHandle, UnitRan, UnitState, Units};
use crate::{Result, RuntimeError};

pub const PROGRAM_KEY: &str = "SPIRA_SYSTEMCTL";
const DEFAULT_PROGRAM: &str = "systemctl";
const CALL_BUDGET: Duration = Duration::from_secs(20);

const PROPS: [&str; 12] = ["Id", "LoadState", "UnitFileState", "ActiveState", "SubState", "Result", "MainPID", "NRestarts", "InvocationID", "ExecMainCode", "ExecMainStatus", "Environment"];

pub struct SystemctlUnits {
    program: String,
    budget: Duration,
}

impl SystemctlUnits {
    pub fn new(program: &str) -> Self {
        SystemctlUnits { program: program.into(), budget: CALL_BUDGET }
    }

    /// The program `SPIRA_SYSTEMCTL` names, else `systemctl` from `PATH`.
    pub fn from_env() -> Self {
        Self::new(&std::env::var(PROGRAM_KEY).ok().filter(|p| !p.is_empty()).unwrap_or_else(|| DEFAULT_PROGRAM.into()))
    }

    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// `systemctl --user <args>`; stdout on a zero exit, a fault otherwise.
    fn run(&self, args: &[&str]) -> Result<String> {
        let what = format!("{} --user {}", self.program, args.join(" "));
        let mut child = Command::new(&self.program).arg("--user").args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| RuntimeError::new(format!("{what}: {e}")))?;
        let mut out = child.stdout.take().ok_or_else(|| RuntimeError::new(format!("{what}: no stdout")))?;
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = out.read_to_string(&mut text);
            text
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) if start.elapsed() >= self.budget => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RuntimeError::new(format!("{what}: over its {} ms budget", self.budget.as_millis())));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => return Err(RuntimeError::new(format!("{what}: {e}"))),
            }
        };
        let text = reader.join().unwrap_or_default();
        if status.success() {
            Ok(text)
        } else {
            Err(RuntimeError::new(format!("{what}: exited {}", status.code().map_or("by signal".to_string(), |c| c.to_string()))))
        }
    }

    fn show(&self, units: &[&str]) -> Result<Vec<UnitDetail>> {
        let mut args = vec!["show"];
        args.extend_from_slice(units);
        let props: Vec<String> = PROPS.iter().map(|p| format!("--property={p}")).collect();
        args.extend(props.iter().map(String::as_str));
        args.push("--no-pager");
        Ok(parse_show(&self.run(&args)?))
    }

    fn verb(&self, verb: &str, h: &UnitHandle) -> Result<()> {
        self.run(&[verb, &h.name]).map(|_| ())
    }
}

/// `systemctl show` output: one block per unit, blank-line separated, `Key=value` lines.
pub fn parse_show(text: &str) -> Vec<UnitDetail> {
    text.split("\n\n").filter_map(detail_of_block).collect()
}

fn detail_of_block(block: &str) -> Option<UnitDetail> {
    let get = |k: &str| block.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).map(str::trim);
    let word = |k: &str| get(k).filter(|v| !v.is_empty()).map(String::from);
    let name = word("Id")?;
    let load_state = word("LoadState");
    if load_state.as_deref().is_none_or(|l| l == "not-found") {
        return Some(UnitDetail { load_state, ..UnitDetail::absent(&name) });
    }
    let active_state = word("ActiveState");
    let number = |k: &str| get(k).and_then(|v| v.parse::<i64>().ok());
    let status = number("ExecMainStatus").and_then(|v| i32::try_from(v).ok());
    let exit = match (number("ExecMainCode"), status) {
        (Some(1), Some(st)) => Some(Exit::Code(st)),
        (Some(2 | 3), Some(st)) => Some(Exit::Signal(st)),
        (Some(c), _) if c > 0 => Some(Exit::Unknown),
        _ => None,
    };
    let invocation = word("InvocationID").filter(|i| i.bytes().any(|b| b != b'0'));
    Some(UnitDetail {
        state: active_state.as_deref().map_or(UnitState::Inactive, state_of_word),
        exit,
        load_state,
        unit_file_state: word("UnitFileState"),
        active_state,
        sub_state: word("SubState"),
        result: word("Result"),
        main_pid: number("MainPID").and_then(|p| u32::try_from(p).ok()).filter(|p| *p != 0),
        n_restarts: number("NRestarts").and_then(|n| u32::try_from(n).ok()),
        invocation_id: invocation,
        environment: get("Environment").map(environment_words).unwrap_or_default(),
        ..UnitDetail::absent(&name)
    })
}

/// `Environment=` prints its entries space-separated, each quoted when it holds a space.
fn environment_words(value: &str) -> Vec<String> {
    value.split_whitespace().map(|w| w.trim_matches(['"', '\'']).to_string()).filter(|w| w.contains('=')).collect()
}

/// `list-unit-files --no-legend`: the first column of each row is a unit file's name.
fn unit_file_rows(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            Some((cols.next()?.to_string(), cols.next().unwrap_or_default().to_string()))
        })
        .collect()
}

impl Units for SystemctlUnits {
    fn start(&self, spec: &crate::units::UnitSpec) -> Result<UnitHandle> {
        Err(RuntimeError::refused(format!("{} cannot start task {}: transient units need the manager's own API", self.program, spec.task)))
    }

    fn stop(&self, h: &UnitHandle) -> Result<()> {
        self.verb("stop", h)
    }

    fn restart(&self, h: &UnitHandle) -> Result<()> {
        self.verb("restart", h)
    }

    fn facts(&self, h: &UnitHandle) -> Result<UnitFacts> {
        self.detail(h).map(|d| d.facts())
    }

    fn detail(&self, h: &UnitHandle) -> Result<UnitDetail> {
        Ok(self.show(&[&h.name])?.into_iter().next().unwrap_or_else(|| UnitDetail::absent(&h.name)))
    }

    fn list(&self, glob: &str) -> Result<Vec<UnitDetail>> {
        let mut out: std::collections::BTreeMap<String, UnitDetail> = self.show(&[glob])?.into_iter().filter(|d| d.state != UnitState::Absent).map(|d| (d.name.clone(), d)).collect();
        for (name, state) in unit_file_rows(&self.run(&["list-unit-files", "--no-legend", "--no-pager", glob])?) {
            out.entry(name.clone()).or_insert_with(|| UnitDetail {
                state: UnitState::Inactive,
                unit_file_state: Some(state).filter(|s| !s.is_empty()),
                active_state: Some("inactive".into()),
                sub_state: Some("dead".into()),
                ..UnitDetail::absent(&name)
            });
        }
        Ok(out.into_values().collect())
    }

    fn start_installed(&self, h: &UnitHandle) -> Result<()> {
        self.verb("start", h)
    }

    fn enable(&self, h: &UnitHandle) -> Result<()> {
        self.verb("enable", h)
    }

    fn disable(&self, h: &UnitHandle) -> Result<()> {
        self.verb("disable", h)
    }

    fn reset_failed(&self, h: &UnitHandle) -> Result<()> {
        self.verb("reset-failed", h)
    }
}

impl UnitCompletions for SystemctlUnits {
    fn ran(&self) -> Result<Vec<UnitRan>> {
        Err(RuntimeError::refused(format!("{} cannot report which runs ended", self.program)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(dir: &std::path::Path, body: &str) -> String {
        let p = dir.join("systemctl");
        testkit::write_exe(&p, &format!("#!/bin/sh\n{body}\n"));
        p.to_string_lossy().into_owned()
    }

    const SHOW: &str = "Id=spira-a.service\nLoadState=loaded\nUnitFileState=enabled\nActiveState=failed\nSubState=failed\nResult=signal\nMainPID=0\nNRestarts=3\nInvocationID=0123456789abcdef0123456789abcdef\nExecMainCode=2\nExecMainStatus=15\nEnvironment=SPIRA_RELEASE=/r/abc \"B=two words\"\n\nId=gone.service\nLoadState=not-found\nActiveState=inactive\n";

    #[test]
    fn a_show_block_becomes_a_typed_detail_and_a_missing_unit_is_absent() {
        let d = parse_show(SHOW);
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].state, d[0].exit, d[0].result.as_deref(), d[0].n_restarts, d[0].main_pid), (UnitState::Failed, Some(Exit::Signal(15)), Some("signal"), Some(3), None));
        assert_eq!(d[0].invocation_id.as_deref(), Some("0123456789abcdef0123456789abcdef"));
        assert_eq!(d[0].environment[0], "SPIRA_RELEASE=/r/abc");
        assert_eq!(d[1].state, UnitState::Absent);
    }

    #[test]
    fn detail_asks_the_named_program_and_a_silent_program_means_absent() {
        let tmp = testkit::TempDir::new("sysctl-units");
        let dir = tmp.path().to_path_buf();
        let silent = SystemctlUnits::new(&stub(&dir, "exit 0"));
        assert_eq!(silent.facts(&UnitHandle::named("x.service")).unwrap().state, UnitState::Absent);
        let loud = SystemctlUnits::new(&stub(&dir, &format!("cat <<'EOF'\n{SHOW}EOF")));
        assert_eq!(loud.facts(&UnitHandle::named("spira-a.service")).unwrap().state, UnitState::Failed);
    }

    #[test]
    fn a_program_that_fails_is_an_error_never_an_empty_answer() {
        let tmp = testkit::TempDir::new("sysctl-units");
        let dir = tmp.path().to_path_buf();
        let u = SystemctlUnits::new(&stub(&dir, "exit 1"));
        assert!(u.list("spira-*").is_err());
        assert!(u.detail(&UnitHandle::named("x.service")).is_err());
    }

    #[test]
    fn a_program_over_its_budget_is_stopped_and_reported() {
        let tmp = testkit::TempDir::new("sysctl-units");
        let dir = tmp.path().to_path_buf();
        let u = SystemctlUnits::new(&stub(&dir, "sleep 30")).with_budget(Duration::from_millis(100));
        let e = u.detail(&UnitHandle::named("x.service")).unwrap_err();
        assert!(e.0.contains("budget"), "{e}");
    }

    #[test]
    fn list_joins_loaded_units_with_installed_unit_files() {
        let tmp = testkit::TempDir::new("sysctl-units");
        let dir = tmp.path().to_path_buf();
        let body = format!("case \"$2\" in\n  show) cat <<'EOF'\n{SHOW}EOF\n;;\n  list-unit-files) printf 'spira-a.service enabled -\\nspira-off.service disabled -\\n';;\nesac");
        let u = SystemctlUnits::new(&stub(&dir, &body));
        let rows: Vec<(String, UnitState)> = u.list("spira-*").unwrap().into_iter().map(|d| (d.name, d.state)).collect();
        assert_eq!(rows, [("spira-a.service".to_string(), UnitState::Failed), ("spira-off.service".to_string(), UnitState::Inactive)]);
    }
}
