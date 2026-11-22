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
pub const MAINTENANCE_FILE: &str = "store-maintenance.json";

/// A deliberate store stop held by one process for the length of a command.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Maintenance {
    pub owner: String,
    pub why: String,
    pub since: u64,
    pub pid: u32,
}

pub fn maintenance_path(run_dir: &Path) -> PathBuf {
    run_dir.join(MAINTENANCE_FILE)
}

pub fn read_maintenance(run_dir: &Path) -> Option<Maintenance> {
    serde_json::from_slice(&std::fs::read(maintenance_path(run_dir)).ok()?).ok()
}

pub fn owner_alive(m: &Maintenance) -> bool {
    Path::new(system_paths::PROC).join(m.pid.to_string()).exists()
}

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

fn remove_maintenance_record(run_dir: &Path) -> Result<(), String> {
    let p = maintenance_path(run_dir);
    match std::fs::remove_file(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

/// Holds only the service stopped while `work` runs, then starts it whatever `work` returned.
/// The runtime keeps running. Returns `work`'s code and the lines to print; a start that does
/// not leave the service active keeps the record and refuses, naming `world thaw-store`.
pub fn maintain(s: &Store, u: &mut dyn Units, run_dir: &Path, m: &Maintenance, work: &mut dyn FnMut() -> i32) -> Result<(i32, Vec<String>), String> {
    let mut out = Vec::new();
    let record = serde_json::to_vec(m).map_err(|e| e.to_string())?;
    let rp = maintenance_path(run_dir);
    std::fs::write(&rp, record).map_err(|e| format!("{}: {e}", rp.display()))?;
    let hold = write_dropin(&s.dropin_path(SERVICE)).and_then(|()| reload(u)).and_then(|()| {
        if !u.run(&["stop", SERVICE]) {
            return Err(format!("systemctl --user stop {SERVICE} failed"));
        }
        let state = u.active_state(SERVICE);
        if state != "inactive" && state != "failed" {
            return Err(format!("{SERVICE} is {state:?} after the stop: not held"));
        }
        Ok(())
    });
    let rc = match &hold {
        Ok(()) => {
            out.push(format!("  stopped {SERVICE}, held by {DROPIN_NAME}"));
            work()
        }
        Err(_) => 0,
    };
    let release = release(s, u);
    if let Err(e) = hold {
        return match release {
            Ok(()) => {
                let _ = remove_maintenance_record(run_dir);
                Err(format!("{e}; the hold was released"))
            }
            Err(r) => Err(format!("{e}; {r}; the maintenance record stays, run world thaw-store")),
        };
    }
    release.map_err(|e| format!("{e}; the maintenance record stays, run world thaw-store"))?;
    remove_maintenance_record(run_dir)?;
    out.push(format!("  started {SERVICE}"));
    Ok((rc, out))
}

fn release(s: &Store, u: &mut dyn Units) -> Result<(), String> {
    let p = s.dropin_path(SERVICE);
    match std::fs::remove_file(&p) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", p.display())),
    }
    reload(u)?;
    if !u.run(&["reset-failed", SERVICE]) || !u.run(&["start", SERVICE]) {
        return Err(format!("systemctl --user start {SERVICE} failed"));
    }
    let state = u.active_state(SERVICE);
    if state != "active" {
        return Err(format!("{SERVICE} is {state:?} after the start"));
    }
    Ok(())
}

/// Removes the drop-ins and the maintenance record and starts the service, then the runtime; reports thawed only once a
/// re-read finds the service active.
pub fn thaw(s: &Store, u: &mut dyn Units, run_dir: &Path) -> Result<Vec<String>, String> {
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
    remove_maintenance_record(run_dir)?;
    for unit in [SERVICE, &s.runtime] {
        if !u.run(&["reset-failed", unit]) || !u.run(&["start", unit]) {
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
        start_attempts: u32,
    }

    impl Fake {
        fn new(s: &Store) -> Fake {
            let active = [SERVICE, &s.runtime].map(String::from).into();
            Fake { dir: s.unit_dir.clone(), runtime: s.runtime.clone(), active, loaded: BTreeMap::new(), stuck: None, calls: Vec::new(), start_attempts: 0 }
        }

        fn condition_met(&self, unit: &str) -> bool {
            self.loaded.get(unit).copied().unwrap_or(true)
        }

        /// One start attempt on the service: counted against the limit even when its condition
        /// skips it, and refused once five are spent until `reset-failed`.
        fn start_service(&mut self) -> bool {
            self.start_attempts += 1;
            if self.start_attempts > 5 {
                return false;
            }
            if self.condition_met(SERVICE) {
                self.active.insert(SERVICE.into());
            }
            true
        }

        fn start(&mut self, unit: &str) -> bool {
            if unit == SERVICE {
                return self.start_service();
            }
            if unit == self.runtime || unit == "cockpit-ensure.service" {
                self.start_service();
            }
            if self.condition_met(unit) {
                self.active.insert(unit.into());
            }
            true
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
                ["start", unit] => return self.start(unit),
                ["reset-failed", unit] => {
                    if *unit == SERVICE {
                        self.start_attempts = 0;
                    }
                }
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

    #[test_id(tkaq67y)]
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

    #[test_id(taysoie)]
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

    #[test_id(t7759mf)]
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

    #[test_id(tp1nfdf)]
    #[test]
    fn a_freeze_refuses_naming_the_unit_still_active_after_the_stop() {
        let d = TempDir::new("freeze-stuck");
        let s = store(&d);
        let mut u = Fake::new(&s);
        u.stuck = Some(SERVICE.into());
        let e = freeze(&s, &mut u).unwrap_err();
        assert!(e.starts_with("lc-serve.service is \"active\" after the stop: not frozen"), "{e}");
    }

    #[test_id(tchox89)]
    #[test]
    fn a_thaw_removes_the_dropins_and_starts_the_service_then_the_runtime() {
        let d = TempDir::new("freeze-thaw");
        let s = store(&d);
        let mut u = Fake::new(&s);
        freeze(&s, &mut u).unwrap();
        u.calls.clear();
        let out = thaw(&s, &mut u, d.path()).unwrap();
        assert!(!s.dropin_path(SERVICE).exists());
        assert_eq!(
            u.calls,
            ["daemon-reload", "reset-failed lc-serve.service", "start lc-serve.service", "reset-failed spira-runtime-test.service", "start spira-runtime-test.service"]
        );
        assert_eq!(out.last().unwrap(), "thawed: lc-serve.service active, spira-runtime-test.service started");
        assert_eq!(u.active_state(&s.runtime), "active");
    }

    #[test_id(te6u2zx)]
    #[test]
    fn thaw_starts_the_service_after_its_start_limit_is_spent() {
        let d = TempDir::new("freeze-limit");
        let s = store(&d);
        let mut u = Fake::new(&s);
        freeze(&s, &mut u).unwrap();
        for _ in 0..6 {
            u.run(&["start", "cockpit-ensure.service"]);
        }
        assert!(!u.run(&["start", SERVICE]), "the limit is spent");
        thaw(&s, &mut u, d.path()).unwrap();
        assert_eq!(u.active_state(SERVICE), "active");
    }

    fn who() -> Maintenance {
        Maintenance { owner: "concierge".into(), why: "compact".into(), since: 1_700_000_000, pid: std::process::id() }
    }

    #[test_id(t90glwe)]
    #[test]
    fn maintenance_holds_the_service_stopped_through_fifteen_pulled_starts_and_starts_it_after() {
        let d = TempDir::new("maintain-holds");
        let s = store(&d);
        let u = std::cell::RefCell::new(Fake::new(&s));
        // `work` and `maintain` both need the manager; the manager is shared through a cell.
        struct Shared<'a>(&'a std::cell::RefCell<Fake>);
        impl Units for Shared<'_> {
            fn run(&mut self, a: &[&str]) -> bool {
                self.0.borrow_mut().run(a)
            }
            fn active_state(&mut self, unit: &str) -> String {
                self.0.borrow_mut().active_state(unit)
            }
        }
        let mut states = Vec::new();
        let mut work = || {
            for _ in 0..15 {
                u.borrow_mut().run(&["start", "cockpit-ensure.service"]);
                states.push(u.borrow_mut().active_state(SERVICE));
            }
            assert!(maintenance_path(d.path()).exists());
            0
        };
        let (rc, _) = maintain(&s, &mut Shared(&u), d.path(), &who(), &mut work).unwrap();
        assert_eq!(rc, 0);
        assert_eq!(states, vec!["inactive"; 15]);
        let mut u = u.into_inner();
        assert_eq!(u.active_state(SERVICE), "active");
        assert_eq!(u.active_state(&s.runtime), "active");
        assert!(!u.calls.iter().any(|c| c == &format!("stop {}", s.runtime)));
        assert_eq!(u.calls[u.calls.len() - 2..], ["reset-failed lc-serve.service", "start lc-serve.service"]);
        assert!(!maintenance_path(d.path()).exists());
        assert!(!s.dropin_path(SERVICE).exists());
    }

    #[test_id(tenbj3q)]
    #[test]
    fn a_failed_command_still_ends_the_maintenance_and_returns_its_code() {
        let d = TempDir::new("maintain-fails");
        let s = store(&d);
        let mut u = Fake::new(&s);
        let (rc, _) = maintain(&s, &mut u, d.path(), &who(), &mut || 3).unwrap();
        assert_eq!(rc, 3);
        assert_eq!(u.active_state(SERVICE), "active");
        assert!(!maintenance_path(d.path()).exists());
    }

    #[test_id(tsezknw)]
    #[test]
    fn a_start_that_fails_keeps_the_record_and_names_thaw_store() {
        let d = TempDir::new("maintain-start-fails");
        let s = store(&d);
        struct NoStart(Fake);
        impl Units for NoStart {
            fn run(&mut self, a: &[&str]) -> bool {
                a.first() != Some(&"start") && self.0.run(a)
            }
            fn active_state(&mut self, unit: &str) -> String {
                self.0.active_state(unit)
            }
        }
        let e = maintain(&s, &mut NoStart(Fake::new(&s)), d.path(), &who(), &mut || 0).unwrap_err();
        assert!(e.contains("world thaw-store"), "{e}");
        assert!(maintenance_path(d.path()).exists());
    }

    #[test_id(t9lyet2)]
    #[test]
    fn the_record_round_trips_and_its_owner_is_alive_only_while_the_pid_exists() {
        let d = TempDir::new("maintain-record");
        std::fs::write(maintenance_path(d.path()), serde_json::to_vec(&who()).unwrap()).unwrap();
        let m = read_maintenance(d.path()).unwrap();
        assert_eq!(m, who());
        assert!(owner_alive(&m));
        assert!(!owner_alive(&Maintenance { pid: u32::MAX, ..m }));
    }

    #[test_id(t7sy442)]
    #[test]
    fn a_thaw_removes_a_maintenance_record_left_behind() {
        let d = TempDir::new("thaw-record");
        let s = store(&d);
        let mut u = Fake::new(&s);
        std::fs::write(maintenance_path(d.path()), serde_json::to_vec(&who()).unwrap()).unwrap();
        thaw(&s, &mut u, d.path()).unwrap();
        assert!(read_maintenance(d.path()).is_none());
    }
}
