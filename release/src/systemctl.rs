//! systemd, behind a trait: the real one runs `systemctl --user` (or `$SPIRA_SYSTEMCTL`);
//! unit tests use a fake.

use std::process::Command;

/// What `systemctl show` says about a unit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnitState {
    /// `ActiveState`: active, inactive, activating, failed, ...
    pub active: String,
    /// `Result`: success, exit-code, ...
    pub result: String,
    /// `Type`: simple, oneshot, notify, ...
    pub kind: String,
}

pub trait Systemctl {
    fn daemon_reload(&self) -> Result<(), String>;
    fn state(&self, unit: &str) -> Result<UnitState, String>;
    fn restart(&self, unit: &str) -> Result<(), String>;
    /// Names of the active units matching `glob` (`systemctl --user list-units
    /// --state=active --no-legend <glob>`), excluding a transient one (a `systemd-run`
    /// unit with no file of its own — `install::install`'s restart-all step must never
    /// touch one; DESIGN.md "install-tarball", the same exclusion `spira/activate.sh` made).
    fn list_active(&self, glob: &str) -> Result<Vec<String>, String>;
    /// `systemctl --user cat <unit>`: the merged unit text (base file plus every drop-in),
    /// whatever the instance's own run state — `intake::install`'s verify step reads this to
    /// confirm systemd actually shows the drop-in it just wrote (DESIGN.md "intake").
    fn cat(&self, unit: &str) -> Result<String, String>;
    /// `systemctl --user disable --now <unit>`: stop and disable in one call — sp-xtdqi-2,
    /// `activate::switch` retiring a unit whose template's gate has closed. Idempotent: a
    /// unit that was never enabled, or already stopped, is not an error.
    fn disable_now(&self, unit: &str) -> Result<(), String>;
}

pub struct RealSystemctl {
    pub program: String,
}

impl RealSystemctl {
    pub fn from_env() -> RealSystemctl {
        RealSystemctl { program: std::env::var("SPIRA_SYSTEMCTL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "systemctl".into()) }
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new(&self.program)
            .arg("--user")
            .args(args)
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.program))?;
        if !out.status.success() {
            return Err(format!(
                "{} --user {} failed ({}): {}",
                self.program,
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

/// Parse `Key=Value` lines from `systemctl show -p ...`.
pub fn parse_show(text: &str) -> UnitState {
    let mut s = UnitState::default();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k {
                "ActiveState" => s.active = v.to_string(),
                "Result" => s.result = v.to_string(),
                "Type" => s.kind = v.to_string(),
                _ => {}
            }
        }
    }
    s
}

impl Systemctl for RealSystemctl {
    fn daemon_reload(&self) -> Result<(), String> {
        self.run(&["daemon-reload"]).map(|_| ())
    }
    fn state(&self, unit: &str) -> Result<UnitState, String> {
        self.run(&["show", unit, "-p", "ActiveState", "-p", "Result", "-p", "Type"]).map(|t| parse_show(&t))
    }
    fn restart(&self, unit: &str) -> Result<(), String> {
        self.run(&["restart", unit]).map(|_| ())
    }
    fn cat(&self, unit: &str) -> Result<String, String> {
        self.run(&["cat", unit])
    }
    fn disable_now(&self, unit: &str) -> Result<(), String> {
        self.run(&["disable", "--now", unit]).map(|_| ())
    }
    fn list_active(&self, glob: &str) -> Result<Vec<String>, String> {
        let out = self.run(&["list-units", "--state=active", "--no-legend", glob])?;
        let mut names = Vec::new();
        for line in out.lines() {
            let Some(unit) = line.split_whitespace().next() else { continue };
            // A transient unit (systemd-run, e.g. spira-landing or spira-aeon-*) has no unit
            // file of its own — restarting it re-execs whatever command line it was launched
            // with, which named files in the release being replaced (sp-hvtdj).
            let ufs = self.run(&["show", unit, "-p", "UnitFileState", "--value"]).unwrap_or_default();
            if ufs.trim() != "transient" {
                names.push(unit.to_string());
            }
        }
        Ok(names)
    }
}
