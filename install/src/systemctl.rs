//! `systemctl --user`, behind a trait — the real one shells out (or to `$SPIRA_SYSTEMCTL`);
//! unit tests use a fake. A richer surface than `release::systemctl::Systemctl` (which only
//! `activate`/`install-tarball` need): this crate also enables, disables, kills and lists unit
//! files, so it carries its own trait rather than widening that one under every implementor.

use std::collections::BTreeMap;

pub trait Systemctl {
    fn daemon_reload(&self) -> Result<(), String>;
    fn is_active(&self, unit: &str) -> bool;
    fn is_enabled(&self, unit: &str) -> Option<String>;
    fn enable(&self, unit: &str) -> Result<(), String>;
    fn enable_now(&self, unit: &str) -> Result<(), String>;
    /// [`Self::is_enabled`] for each of `units`, in order.
    fn is_enabled_many(&self, units: &[String]) -> Vec<Option<String>> {
        units.iter().map(|u| self.is_enabled(u)).collect()
    }
    /// [`Self::is_active`] for each of `units`, in order.
    fn is_active_many(&self, units: &[String]) -> Vec<bool> {
        units.iter().map(|u| self.is_active(u)).collect()
    }
    fn disable_now(&self, unit: &str) -> Result<(), String>;
    /// `systemctl disable` — unlike [`Self::disable_now`], leaves a running unit running
    /// (`unit-ensure`'s producer guard: withdrawing the opt-in stops it firing again, not
    /// mid-flight).
    fn disable(&self, unit: &str) -> Result<(), String>;
    /// `systemctl restart`. Some units (`dolt-beads.service`, `RefuseManualStop=yes`) refuse
    /// this outright; callers needing that unit's own kill+respawn path use [`Self::kill`].
    fn restart(&self, unit: &str) -> Result<(), String>;
    /// `systemctl start` — a oneshot unit blocks until it has finished.
    fn start(&self, unit: &str) -> Result<(), String>;
    /// `systemctl kill` — bypasses job control, for a unit that refuses a restart job.
    fn kill(&self, unit: &str) -> Result<(), String>;
    /// `Type=` (empty when the unit cannot be shown).
    fn unit_type(&self, unit: &str) -> String;
    /// Names from `list-unit-files --no-legend <glob>` union `list-units --all --no-legend
    /// <glob>` — everything systemd currently knows about matching `glob`, deduplicated.
    fn list_matching(&self, glob: &str) -> Vec<String>;
    /// Names from `list-units --state=active --no-legend <glob>` alone — currently RUNNING
    /// units matching `glob`, not merely known to systemd. [`crate::checks::live_dwarves`]'s one
    /// caller: `spira_live_dwarves` (lib.sh) queried `--state=active` and nothing else, and a
    /// unit file left on disk or loaded-but-dead from a prior run must not read as a live
    /// dwarf disrupting this install.
    fn list_active_matching(&self, glob: &str) -> Vec<String>;
}

pub struct RealSystemctl {
    pub program: String,
}

impl RealSystemctl {
    pub fn from_env() -> RealSystemctl {
        RealSystemctl { program: std::env::var("SPIRA_SYSTEMCTL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "systemctl".into()) }
    }

    /// One query for many units; `None` when stdout is not one line per unit (a unit
    /// systemd cannot find may print nothing), so the caller asks unit by unit.
    fn one_line_each(&self, verb: &str, units: &[String]) -> Option<Vec<String>> {
        if units.is_empty() {
            return Some(Vec::new());
        }
        let mut args: Vec<&str> = vec![verb];
        args.extend(units.iter().map(String::as_str));
        let out = self.run(&args).1;
        let lines: Vec<String> = out.lines().map(|l| l.trim().to_string()).collect();
        (lines.len() == units.len()).then_some(lines)
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        match spira_config::bounded::bounded(&self.program).arg("--user").args(args).output() {
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
    fn is_enabled_many(&self, units: &[String]) -> Vec<Option<String>> {
        match self.one_line_each("is-enabled", units) {
            Some(lines) => lines.into_iter().map(|l| Some(l).filter(|s| !s.is_empty())).collect(),
            None => units.iter().map(|u| self.is_enabled(u)).collect(),
        }
    }
    fn is_active_many(&self, units: &[String]) -> Vec<bool> {
        match self.one_line_each("is-active", units) {
            Some(lines) => lines.iter().map(|l| l == "active").collect(),
            None => units.iter().map(|u| self.is_active(u)).collect(),
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
    fn disable(&self, unit: &str) -> Result<(), String> {
        let (ok, _, err) = self.run(&["disable", unit]);
        if ok {
            Ok(())
        } else {
            Err(err)
        }
    }
    fn start(&self, unit: &str) -> Result<(), String> {
        let (ok, _, err) = self.run(&["start", unit]);
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
    fn list_active_matching(&self, glob: &str) -> Vec<String> {
        let (_, out, _) = self.run(&["list-units", "--state=active", "--no-legend", glob]);
        let mut seen = std::collections::BTreeSet::new();
        for line in out.lines() {
            if let Some(name) = line.split_whitespace().next() {
                seen.insert(name.to_string());
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
    pub started: std::cell::RefCell<Vec<String>>,
    pub order: std::cell::RefCell<Vec<String>>,
    pub kills: std::cell::RefCell<Vec<String>>,
    pub enabled_now: std::cell::RefCell<Vec<String>>,
    pub disabled_now: std::cell::RefCell<Vec<String>>,
    pub disabled: std::cell::RefCell<Vec<String>>,
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
        self.order.borrow_mut().push(format!("enable_now {unit}"));
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
    fn disable(&self, unit: &str) -> Result<(), String> {
        self.disabled.borrow_mut().push(unit.to_string());
        self.units.borrow_mut().entry(unit.to_string()).or_default().enabled = Some("disabled".into());
        Ok(())
    }
    fn start(&self, unit: &str) -> Result<(), String> {
        self.started.borrow_mut().push(unit.to_string());
        self.order.borrow_mut().push(format!("start {unit}"));
        Ok(())
    }
    fn restart(&self, unit: &str) -> Result<(), String> {
        self.restarts.borrow_mut().push(unit.to_string());
        self.order.borrow_mut().push(format!("restart {unit}"));
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
    fn list_active_matching(&self, glob: &str) -> Vec<String> {
        let pat = glob.replace('*', "");
        let (pre, suf) = glob.split_once('*').unwrap_or((glob, ""));
        self.units
            .borrow()
            .iter()
            .filter(|(_, u)| u.active)
            .map(|(n, _)| n.clone())
            .filter(|n| if pat.is_empty() { n.as_str() == glob } else { n.starts_with(pre) && n.ends_with(suf) })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(body: &str) -> (testkit::TempDir, RealSystemctl, std::path::PathBuf) {
        let dir = testkit::TempDir::new("systemctl-stub");
        let log = dir.path().join("calls");
        let prog = dir.path().join("systemctl");
        testkit::write_exe(&prog, &format!("#!/bin/sh\necho \"$*\" >> {}\n{body}\n", log.display()));
        (dir, RealSystemctl { program: prog.display().to_string() }, log)
    }

    fn calls(log: &std::path::Path) -> usize {
        std::fs::read_to_string(log).unwrap().lines().count()
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn many_units_are_asked_about_in_one_call_and_keep_their_own_answers() {
        let (_d, sc, log) = stub("case \"$2\" in is-enabled) ;; esac; case \"$2\" in is-active) printf 'active\\ninactive\\n';; is-enabled) printf 'enabled\\ndisabled\\n';; esac");
        let u = names(&["a.service", "b.service"]);
        assert_eq!(sc.is_active_many(&u), vec![true, false]);
        assert_eq!(sc.is_enabled_many(&u), vec![Some("enabled".into()), Some("disabled".into())]);
        assert_eq!(calls(&log), 2);
    }

    #[test]
    fn an_answer_with_a_line_missing_falls_back_to_one_call_per_unit() {
        let (_d, sc, log) = stub("case \"$*\" in *a.service*b.service*) printf 'enabled\\n';; *a.service) echo enabled;; esac");
        let got = sc.is_enabled_many(&names(&["a.service", "b.service"]));
        assert_eq!(got, vec![Some("enabled".into()), None]);
        assert_eq!(calls(&log), 3);
    }
}
