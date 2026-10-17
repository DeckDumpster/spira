//! systemd, behind a trait: the real one runs `systemctl --user` (or `$SPIRA_SYSTEMCTL`);
//! unit tests use a fake.


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

/// Hard ceiling on one unit's wait when systemd reports no finite start budget.
pub const MAX_UNIT_WAIT: std::time::Duration = std::time::Duration::from_secs(900);

/// Poll `units` until each reports `active` or `failed`. Each unit is waited for up to its
/// own start budget (`RestartSec` + `TimeoutStartSec`, sp-1fzo3z), never less than `cap`
/// and never more than [`MAX_UNIT_WAIT`]; a unit still `activating` at that bound is
/// returned as it stood.
pub fn await_settled(sc: &dyn Systemctl, units: &[String], cap: std::time::Duration) -> Vec<(String, Result<UnitState, String>)> {
    let start = std::time::Instant::now();
    let mut out: Vec<(String, Result<UnitState, String>)> = Vec::new();
    for u in units {
        let budget = sc.start_budget(u).unwrap_or(cap).max(cap).min(MAX_UNIT_WAIT);
        let deadline = start + budget;
        loop {
            let st = sc.state(u);
            let done = match &st {
                Ok(s) => matches!(s.active.as_str(), "active" | "failed"),
                Err(_) => true,
            };
            if done || std::time::Instant::now() >= deadline {
                out.push((u.clone(), st));
                break;
            }
            std::thread::sleep((budget / 50).clamp(std::time::Duration::from_millis(10), std::time::Duration::from_millis(250)));
        }
    }
    out
}

pub trait Systemctl {
    fn daemon_reload(&self) -> Result<(), String>;
    fn state(&self, unit: &str) -> Result<UnitState, String>;
    /// How long the unit may legitimately take to come up after a restart: its own
    /// `RestartSec` + `TimeoutStartSec` (sp-1fzo3z). None when systemd reports no finite
    /// budget, or the fake does not model one.
    fn start_budget(&self, _unit: &str) -> Option<std::time::Duration> {
        None
    }
    fn restart(&self, unit: &str) -> Result<(), String>;
    /// `systemctl --user kill`: bypasses job control, for a unit that refuses a restart job.
    fn kill(&self, unit: &str) -> Result<(), String>;
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
    /// Names of the failed units matching `glob`.
    fn list_failed(&self, _glob: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    /// `systemctl --user reset-failed <unit>`: clears a start-limit hit so a restart is not refused.
    fn reset_failed(&self, _unit: &str) -> Result<(), String> {
        Ok(())
    }
}

pub struct RealSystemctl {
    pub program: String,
    /// The generic per-call deadline, in `timeout` seconds.
    pub call_secs: String,
}

impl RealSystemctl {
    pub fn from_env() -> RealSystemctl {
        RealSystemctl {
            program: std::env::var("SPIRA_SYSTEMCTL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "systemctl".into()),
            call_secs: spira_config::bounded::CALL_TIMEOUT_SECS.into(),
        }
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.run_for(args, &self.call_secs)
    }

    fn run_for(&self, args: &[&str], secs: &str) -> Result<String, String> {
        let out = spira_config::bounded::bounded_for(&self.program, secs)
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
/// A systemd timespan as `systemctl show` prints it ("5s", "15min", "1min 30s", "100ms",
/// "infinity"). None for infinity or anything unparseable.
pub fn parse_timespan(v: &str) -> Option<std::time::Duration> {
    let v = v.trim();
    if v.is_empty() || v == "infinity" {
        return None;
    }
    let mut total = std::time::Duration::ZERO;
    for tok in v.split_whitespace() {
        let split = tok.find(|c: char| !c.is_ascii_digit() && c != '.')?;
        let (num, unit) = tok.split_at(split);
        let n: f64 = num.parse().ok()?;
        let secs = match unit {
            "us" | "usec" => n / 1_000_000.0,
            "ms" | "msec" => n / 1000.0,
            "s" | "sec" => n,
            "min" | "m" => n * 60.0,
            "h" | "hr" => n * 3600.0,
            "d" => n * 86400.0,
            _ => return None,
        };
        total += std::time::Duration::from_secs_f64(secs);
    }
    Some(total)
}

/// `RestartUSec` + `TimeoutStartUSec` from `systemctl show` text; None if either is infinite.
pub fn budget_from_show(text: &str) -> Option<std::time::Duration> {
    let (mut restart, mut start) = (None, None);
    for line in text.lines() {
        match line.split_once('=') {
            Some(("RestartUSec", v)) => restart = parse_timespan(v),
            Some(("TimeoutStartUSec", v)) => start = parse_timespan(v),
            _ => {}
        }
    }
    Some(restart? + start?)
}

/// `TimeoutStopUSec` + `TimeoutStartUSec` from `systemctl show` text: what a restart job may
/// take. None if either is infinite.
pub fn restart_budget_from_show(text: &str) -> Option<std::time::Duration> {
    let (mut stop, mut start) = (None, None);
    for line in text.lines() {
        match line.split_once('=') {
            Some(("TimeoutStopUSec", v)) => stop = parse_timespan(v),
            Some(("TimeoutStartUSec", v)) => start = parse_timespan(v),
            _ => {}
        }
    }
    Some(stop? + start?)
}

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
    fn start_budget(&self, unit: &str) -> Option<std::time::Duration> {
        self.run(&["show", unit, "-p", "RestartUSec", "-p", "TimeoutStartUSec"]).ok().and_then(|t| budget_from_show(&t))
    }
    fn restart(&self, unit: &str) -> Result<(), String> {
        let budget = self
            .run(&["show", unit, "-p", "TimeoutStopUSec", "-p", "TimeoutStartUSec"])
            .ok()
            .and_then(|t| restart_budget_from_show(&t))
            .map(|b| b.min(MAX_UNIT_WAIT));
        match budget {
            Some(b) if b.as_secs_f64() > self.call_secs.parse::<f64>().unwrap_or(0.0) => {
                self.run_for(&["restart", unit], &format!("{:.1}", b.as_secs_f64())).map(|_| ())
            }
            _ => self.run(&["restart", unit]).map(|_| ()),
        }
    }
    fn kill(&self, unit: &str) -> Result<(), String> {
        self.run(&["kill", unit]).map(|_| ())
    }
    fn cat(&self, unit: &str) -> Result<String, String> {
        self.run(&["cat", unit])
    }
    fn disable_now(&self, unit: &str) -> Result<(), String> {
        self.run(&["disable", "--now", unit]).map(|_| ())
    }
    fn reset_failed(&self, unit: &str) -> Result<(), String> {
        self.run(&["reset-failed", unit]).map(|_| ())
    }
    fn list_failed(&self, glob: &str) -> Result<Vec<String>, String> {
        let out = self.run(&["list-units", "--state=failed", "--no-legend", "--plain", glob])?;
        Ok(out.lines().filter_map(|l| l.split_whitespace().next()).map(String::from).collect())
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
