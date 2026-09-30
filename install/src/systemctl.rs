//! `systemctl --user`, behind a trait — the real one shells out (or to `$SPIRA_SYSTEMCTL`);
//! unit tests use a fake. A richer surface than `release::systemctl::Systemctl` (which only
//! `activate`/`install-tarball` need): this crate also enables, disables, kills and lists unit
//! files, so it carries its own trait rather than widening that one under every implementor.

use std::collections::BTreeMap;
use std::process::Command;

pub trait Systemctl {
    fn daemon_reload(&self) -> Result<(), String>;
    fn is_active(&self, unit: &str) -> bool;
    fn is_enabled(&self, unit: &str) -> Option<String>;
    fn enable(&self, unit: &str) -> Result<(), String>;
    fn enable_now(&self, unit: &str) -> Result<(), String>;
    fn disable_now(&self, unit: &str) -> Result<(), String>;
    /// `systemctl restart`. Some units (`dolt-beads.service`, `RefuseManualStop=yes`) refuse
    /// this outright; callers needing that unit's own kill+respawn path use [`Self::kill`].
    fn restart(&self, unit: &str) -> Result<(), String>;
    /// `systemctl kill` — bypasses job control, for a unit that refuses a restart job.
    fn kill(&self, unit: &str) -> Result<(), String>;
    /// `Type=` (empty when the unit cannot be shown).
    fn unit_type(&self, unit: &str) -> String;
    /// Names from `list-unit-files --no-legend <glob>` union `list-units --all --no-legend
    /// <glob>` — everything systemd currently knows about matching `glob`, deduplicated.
    fn list_matching(&self, glob: &str) -> Vec<String>;
}

pub struct RealSystemctl {
    pub program: String,
}

impl RealSystemctl {
    pub fn from_env() -> RealSystemctl {
        RealSystemctl { program: std::env::var("SPIRA_SYSTEMCTL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "systemctl".into()) }
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        match Command::new(&self.program).arg("--user").args(args).output() {
            Ok(out) => (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string()),
            Err(e) => (false, String::new(), format!("cannot run {}: {e}", self.program)),
        }
    }
}

impl Systemctl for RealSystemctl {
    fn daemon_reload(&self) -> Result<(), String> {
        let (ok, _, err) = self.run(&["daemon-reload"]);
        if ok {
            Ok(())
        } else {
            Err(err)
        }
    }
    fn is_active(&self, unit: &str) -> bool {
        self.run(&["is-active", unit]).1.trim() == "active"
    }
    fn is_enabled(&self, unit: &str) -> Option<String> {
        let (_, out, _) = self.run(&["is-enabled", unit]);
        let s = out.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }
    fn enable(&self, unit: &str) -> Result<(), String> {
        let (ok, _, err) = self.run(&["enable", unit]);
        if ok {
            Ok(())
        } else {
            Err(err)
        }
    }
    fn enable_now(&self, unit: &str) -> Result<(), String> {
        let (ok, _, err) = self.run(&["enable", "--now", unit]);
        if ok {
            Ok(())
        } else {
            Err(err)
        }
    }
    fn disable_now(&self, unit: &str) -> Result<(), String> {
        let (ok, _, err) = self.run(&["disable", "--now", unit]);
        if ok {
            Ok(())
        } else {
            Err(err)
        }
    }
    fn restart(&self, unit: &str) -> Result<(), String> {
        let (ok, _, err) = self.run(&["restart", unit]);
        if ok {
            Ok(())
        } else {
            Err(err)
        }
    }
    fn kill(&self, unit: &str) -> Result<(), String> {
        let (ok, _, err) = self.run(&["kill", unit]);
        if ok {
            Ok(())
        } else {
            Err(err)
        }
    }
    fn unit_type(&self, unit: &str) -> String {
        self.run(&["show", "-p", "Type", "--value", unit]).1.trim().to_string()
    }
    fn list_matching(&self, glob: &str) -> Vec<String> {
        let mut seen = std::collections::BTreeSet::new();
        for args in [["list-unit-files", "--no-legend", glob].as_slice(), ["list-units", "--all", "--no-legend", glob].as_slice()] {
            let (_, out, _) = self.run(args);
            for line in out.lines() {
                if let Some(name) = line.split_whitespace().next() {
                    seen.insert(name.to_string());
                }
            }
        }
        seen.into_iter().collect()
    }
}

/// An in-memory double for tests: unit name -> (active, enabled-string, type).
#[derive(Debug, Clone, Default)]
pub struct FakeSystemctl {
    pub units: std::cell::RefCell<BTreeMap<String, FakeUnit>>,
    pub reloads: std::cell::Cell<u32>,
    pub restarts: std::cell::RefCell<Vec<String>>,
    pub kills: std::cell::RefCell<Vec<String>>,
    pub enabled_now: std::cell::RefCell<Vec<String>>,
    pub disabled_now: std::cell::RefCell<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct FakeUnit {
    pub active: bool,
    pub enabled: Option<String>,
    pub kind: String,
    pub file_listed: bool,
}

impl FakeSystemctl {
    pub fn set(&self, unit: &str, u: FakeUnit) {
        self.units.borrow_mut().insert(unit.to_string(), u);
    }
}

impl Systemctl for FakeSystemctl {
    fn daemon_reload(&self) -> Result<(), String> {
        self.reloads.set(self.reloads.get() + 1);
        Ok(())
    }
    fn is_active(&self, unit: &str) -> bool {
        self.units.borrow().get(unit).map(|u| u.active).unwrap_or(false)
    }
    fn is_enabled(&self, unit: &str) -> Option<String> {
        self.units.borrow().get(unit).and_then(|u| u.enabled.clone())
    }
    fn enable(&self, unit: &str) -> Result<(), String> {
        self.units.borrow_mut().entry(unit.to_string()).or_default().enabled = Some("enabled".into());
        Ok(())
    }
    fn enable_now(&self, unit: &str) -> Result<(), String> {
        self.enabled_now.borrow_mut().push(unit.to_string());
        let mut m = self.units.borrow_mut();
        let e = m.entry(unit.to_string()).or_default();
        e.enabled = Some("enabled".into());
        e.active = true;
        Ok(())
    }
    fn disable_now(&self, unit: &str) -> Result<(), String> {
        self.disabled_now.borrow_mut().push(unit.to_string());
        let mut m = self.units.borrow_mut();
        let e = m.entry(unit.to_string()).or_default();
        e.enabled = Some("disabled".into());
        e.active = false;
        Ok(())
    }
    fn restart(&self, unit: &str) -> Result<(), String> {
        self.restarts.borrow_mut().push(unit.to_string());
        self.units.borrow_mut().entry(unit.to_string()).or_default().active = true;
        Ok(())
    }
    fn kill(&self, unit: &str) -> Result<(), String> {
        self.kills.borrow_mut().push(unit.to_string());
        self.units.borrow_mut().entry(unit.to_string()).or_default().active = true;
        Ok(())
    }
    fn unit_type(&self, unit: &str) -> String {
        self.units.borrow().get(unit).map(|u| u.kind.clone()).unwrap_or_default()
    }
    fn list_matching(&self, glob: &str) -> Vec<String> {
        let pat = glob.replace('*', "");
        let (pre, suf) = glob.split_once('*').unwrap_or((glob, ""));
        self.units
            .borrow()
            .iter()
            .filter(|(_, u)| u.file_listed)
            .map(|(n, _)| n.clone())
            .filter(|n| if pat.is_empty() { n.as_str() == glob } else { n.starts_with(pre) && n.ends_with(suf) })
            .collect()
    }
}
