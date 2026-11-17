//! Freezing the lifecycle store's same-user service so nothing restarts it, and thawing it.
//!
//! A plain stop does not hold: every unit that `Wants=` the service starts it again on its own
//! next start, and the runtime restarts within seconds. A `--runtime` mask does not hold either,
//! because a unit file under the user's config directory outranks the mask under `/run`. A
//! runtime drop-in is merged into the unit wherever the unit file lives, so a condition that can
//! never be met makes every start of the frozen units a no-op until the drop-in is removed.

use std::path::{Path, PathBuf};

pub const SERVICE: &str = "lc-serve.service";
pub const DROPIN_NAME: &str = "zz-spira-freeze.conf";
pub const FROZEN_CONDITION: &str = system_paths::FROZEN_CONDITION;

/// The drop-in written beside the frozen unit.
pub fn dropin() -> String {
    format!("# world freeze-store wrote this; world thaw-store removes it.\n[Unit]\nConditionPathExists={FROZEN_CONDITION}\n")
}

/// The user manager's runtime unit directory, `/run/user/<uid>/systemd/user`, derived from the
/// uid as the socket path is, so every caller of one operator names the same directory.
pub fn runtime_unit_dir(uid: u32) -> PathBuf {
    PathBuf::from(format!("{}/{uid}/systemd/user", system_paths::RUN_USER))
}

/// What a freeze acts on: the frozen service, and the runtime that wants it.
pub struct Store {
    pub unit_dir: PathBuf,
    pub runtime: String,
}

impl Store {
    pub fn new(unit_dir: PathBuf, instance: &str) -> Store {
        Store { unit_dir, runtime: spira_config::unit::unit_name("runtime", "service", instance) }
    }

    /// The units held frozen by a drop-in.
    pub fn frozen(&self) -> [&str; 1] {
        [SERVICE]
    }

    /// Every unit a freeze stops, the runtime first so it cannot start the service again.
    pub fn stopped(&self) -> [&str; 2] {
        [&self.runtime, SERVICE]
    }

    pub fn dropin_path(&self, unit: &str) -> PathBuf {
        self.unit_dir.join(format!("{unit}.d")).join(DROPIN_NAME)
    }
}

/// The user manager, as freeze and thaw drive it.
pub trait Units {
    /// `systemctl --user <args>`; whether it exited zero.
    fn run(&mut self, args: &[&str]) -> bool;
    /// The unit's `ActiveState`.
    fn active_state(&mut self, unit: &str) -> String;
}

/// The real user manager.
pub struct Systemd;

impl Units for Systemd {
    fn run(&mut self, args: &[&str]) -> bool {
        crate::sysctl::run_ok(args)
    }
    fn active_state(&mut self, unit: &str) -> String {
        crate::sysctl::run(&["show", "-p", "ActiveState", "--value", unit])
    }
}

fn write_dropin(path: &Path) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| format!("{}: no parent", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(path, dropin()).map_err(|e| format!("{}: {e}", path.display()))
}

fn reload(u: &mut dyn Units) -> Result<(), String> {
    if u.run(&["daemon-reload"]) {
        Ok(())
    } else {
        Err("systemctl --user daemon-reload failed".into())
    }
}

/// Writes the drop-ins, stops the runtime and the service, and reports frozen only once a
/// re-read finds both inactive. Returns the lines to print, or the refusal.
pub fn freeze(s: &Store, u: &mut dyn Units) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for unit in s.frozen() {
        let p = s.dropin_path(unit);
        write_dropin(&p)?;
        out.push(format!("  wrote {}", p.display()));
    }
    reload(u)?;
    for unit in s.stopped() {
        if !u.run(&["stop", unit]) {
            return Err(format!("systemctl --user stop {unit} failed; the drop-ins stay, run thaw-store to undo"));
        }
        out.push(format!("  stopped {unit}"));
    }
    for unit in s.stopped() {
        let state = u.active_state(unit);
        if state != "inactive" && state != "failed" {
            return Err(format!("{unit} is {state:?} after the stop: not frozen; the drop-ins stay, run thaw-store to undo"));
        }
    }
    out.push(format!("frozen: {} held by {DROPIN_NAME}, {} stopped", s.frozen().join(" and "), s.runtime));
    Ok(out)
}

/// Removes the drop-ins and starts the service, then the runtime; reports thawed only once a
/// re-read finds the service active.
pub fn thaw(s: &Store, u: &mut dyn Units) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for unit in s.frozen() {
        let p = s.dropin_path(unit);
        match std::fs::remove_file(&p) {
            Ok(()) => out.push(format!("  removed {}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", p.display())),
        }
    }
    reload(u)?;
    for unit in [SERVICE, &s.runtime] {
        if !u.run(&["start", unit]) {
            return Err(format!("systemctl --user start {unit} failed"));
        }
        out.push(format!("  started {unit}"));
    }
    let state = u.active_state(SERVICE);
    if state != "active" {
        return Err(format!("{SERVICE} is {state:?} after the start: not thawed"));
    }
    out.push(format!("thawed: {SERVICE} active, {} started", s.runtime));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use testkit::TempDir;

    /// A user manager over a runtime unit directory: a unit's conditions are read from its
    /// drop-ins at daemon-reload, a start of a unit whose condition fails is skipped, and a
    /// start of the runtime starts the service it wants.
    struct Fake {
        dir: PathBuf,
        runtime: String,
        active: BTreeSet<String>,
        loaded: BTreeMap<String, bool>,
        stuck: Option<String>,
        calls: Vec<String>,
    }

    impl Fake {
        fn new(s: &Store) -> Fake {
            let active = [SERVICE, &s.runtime].map(String::from).into();
            Fake { dir: s.unit_dir.clone(), runtime: s.runtime.clone(), active, loaded: BTreeMap::new(), stuck: None, calls: Vec::new() }
        }

        fn condition_met(&self, unit: &str) -> bool {
            self.loaded.get(unit).copied().unwrap_or(true)
        }

        fn start(&mut self, unit: &str) {
            if unit == self.runtime && self.condition_met(SERVICE) {
                self.active.insert(SERVICE.into());
            }
            if self.condition_met(unit) {
                self.active.insert(unit.into());
            }
        }
    }

    impl Units for Fake {
        fn run(&mut self, args: &[&str]) -> bool {
            self.calls.push(args.join(" "));
            match args {
                ["daemon-reload"] => {
                    self.loaded.clear();
                    let conf = std::fs::read_to_string(self.dir.join(format!("{SERVICE}.d")).join(DROPIN_NAME)).unwrap_or_default();
                    let met = conf.lines().filter_map(|l| l.strip_prefix("ConditionPathExists=")).all(|p| Path::new(p).exists());
                    self.loaded.insert(SERVICE.into(), met);
                }
                ["stop", unit] => {
                    if self.stuck.as_deref() != Some(*unit) {
                        self.active.remove(*unit);
                    }
                }
                ["start", unit] => self.start(unit),
                _ => return false,
            }
            true
        }
        fn active_state(&mut self, unit: &str) -> String {
            if self.active.contains(unit) { "active" } else { "inactive" }.into()
        }
    }

    fn store(dir: &TempDir) -> Store {
        Store::new(dir.path().join("systemd/user"), "test")
    }

    #[test]
    fn the_freeze_dropin_is_a_condition_no_path_meets_on_the_service() {
        let d = TempDir::new("freeze-dropin");
        let s = store(&d);
        assert_eq!(dropin().lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>(), ["[Unit]", &format!("ConditionPathExists={FROZEN_CONDITION}")]);
        assert!(!Path::new(FROZEN_CONDITION).exists());
        assert_eq!(s.frozen(), ["lc-serve.service"]);
        assert_eq!(s.stopped(), ["spira-runtime-test.service", "lc-serve.service"]);
        assert_eq!(s.dropin_path(SERVICE), d.path().join("systemd/user/lc-serve.service.d/zz-spira-freeze.conf"));
        assert_eq!(runtime_unit_dir(1000), PathBuf::from(format!("{}/1000/systemd/user", system_paths::RUN_USER)));
    }

    #[test]
    fn a_frozen_service_is_not_restarted_when_the_runtime_that_wants_it_starts() {
        let d = TempDir::new("freeze-holds");
        let s = store(&d);
        let mut u = Fake::new(&s);
        let out = freeze(&s, &mut u).unwrap();
        assert!(out.last().unwrap().starts_with("frozen: lc-serve.service held"), "{out:?}");
        assert_eq!(std::fs::read_to_string(s.dropin_path(SERVICE)).unwrap(), dropin());
        assert_eq!(u.calls, ["daemon-reload", "stop spira-runtime-test.service", "stop lc-serve.service"]);
        u.run(&["start", &s.runtime]);
        u.run(&["start", SERVICE]);
        assert_eq!(u.active_state(SERVICE), "inactive");
    }

    #[test]
    fn without_the_freeze_a_stopped_service_comes_back_with_the_runtime() {
        let d = TempDir::new("freeze-control");
        let s = store(&d);
        let mut u = Fake::new(&s);
        for unit in s.stopped() {
            u.run(&["stop", unit]);
        }
        u.run(&["start", &s.runtime]);
        assert_eq!(u.active_state(SERVICE), "active");
    }

    #[test]
    fn a_freeze_refuses_naming_the_unit_still_active_after_the_stop() {
        let d = TempDir::new("freeze-stuck");
        let s = store(&d);
        let mut u = Fake::new(&s);
        u.stuck = Some(SERVICE.into());
        let e = freeze(&s, &mut u).unwrap_err();
        assert!(e.starts_with("lc-serve.service is \"active\" after the stop: not frozen"), "{e}");
    }

    #[test]
    fn a_thaw_removes_the_dropins_and_starts_the_service_then_the_runtime() {
        let d = TempDir::new("freeze-thaw");
        let s = store(&d);
        let mut u = Fake::new(&s);
        freeze(&s, &mut u).unwrap();
        u.calls.clear();
        let out = thaw(&s, &mut u).unwrap();
        assert!(!s.dropin_path(SERVICE).exists());
        assert_eq!(u.calls, ["daemon-reload", "start lc-serve.service", "start spira-runtime-test.service"]);
        assert_eq!(out.last().unwrap(), "thawed: lc-serve.service active, spira-runtime-test.service started");
        assert_eq!(u.active_state(&s.runtime), "active");
    }
}
