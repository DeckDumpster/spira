//! The pool of one (DESIGN.md §2.3): acquire, release, the background provision, reaping
//! and the outage alarm. Every read-modify-write of the pool state happens under one flock
//! and is written back atomically.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::alarm::Alarm;
use crate::procs::FileLock;
use crate::provider::{destroy_verified, provision, DestroyError, Provider, ProvisionSpec, Timing};
use crate::machine::VmState;
use crate::schema::{now, AcquireMode, Lease, Outage, PoolState, ProcId, Provisioning, Vm};

/// Everything one attempt needs, built fresh from config and pve.env each time (G7).
pub struct Attempt {
    pub provider: Box<dyn Provider>,
    pub iface: String,
    pub ssh_user: String,
    pub pubkey: String,
    pub timing: Timing,
}

impl Attempt {
    fn spec(&self) -> ProvisionSpec<'_> {
        ProvisionSpec { iface: &self.iface, ssh_user: &self.ssh_user, pubkey: &self.pubkey, timing: self.timing }
    }
}

/// Starts the one background provision of the next VM; returns its process identity.
pub trait Spawner {
    fn spawn_next(&self) -> Result<ProcId, String>;
}

pub struct Deps<'a> {
    pub factory: &'a dyn Fn() -> Result<Attempt, String>,
    pub alarm: &'a dyn Alarm,
    pub spawner: &'a dyn Spawner,
}

pub struct Pool {
    pub state_dir: PathBuf,
    pub retry_interval: Duration,
    pub max_retries: u32,
    pub wait_poll: Duration,
    /// Zero waits forever.
    pub acquire_deadline: Duration,
}

enum Next {
    Got(Vm),
    Wait,
    Provision,
}

pub fn read_state(path: &Path) -> Result<PoolState, String> {
    match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => Ok(PoolState::default()),
        Ok(t) => serde_json::from_str(&t).map_err(|e| format!("round-vm: {} unreadable: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PoolState::default()),
        Err(e) => Err(format!("round-vm: {}: {e}", path.display())),
    }
}

fn write_state(path: &Path, s: &PoolState) -> Result<(), String> {
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let body = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

impl Pool {
    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("pool.json")
    }

    /// Runs `f` on the pool state under the lock and writes the result back.
    pub fn with_state<R>(&self, f: impl FnOnce(&mut PoolState) -> R) -> Result<R, String> {
        std::fs::create_dir_all(&self.state_dir).map_err(|e| format!("{}: {e}", self.state_dir.display()))?;
        let _lock = FileLock::exclusive(&self.state_dir.join("lock"))?;
        let path = self.state_file();
        let mut s = read_state(&path)?;
        let before = s.clone();
        s.adopt_untracked();
        let r = f(&mut s);
        if s != before {
            write_state(&path, &s)?;
        }
        Ok(r)
    }

    /// Destroys what nobody owns any more (G2): every `doomed` VM, a provision whose owner
    /// died, and every lease held by a process that is gone.
    fn reap(s: &mut PoolState, p: &dyn Provider, t: Timing) {
        if let Some(prov) = s.provisioning.clone() {
            if !prov.owner.alive() {
                if let Some(id) = &prov.vmid {
                    s.note(id, VmState::Doomed, "its provisioner died");
                    s.doomed.push(id.clone());
                }
                s.provisioning = None;
            }
        }
        let (dead, live): (Vec<Lease>, Vec<Lease>) =
            s.leases.drain(..).partition(|l| l.owner.map(|o| !o.alive()).unwrap_or(false));
        s.leases = live;
        for l in &dead {
            s.note(&l.vm.handle, VmState::Doomed, "its lease holder is gone");
        }
        s.doomed.extend(dead.into_iter().map(|l| l.vm.handle));
        let doomed = std::mem::take(&mut s.doomed);
        for id in doomed {
            match destroy_verified(p, &id, t) {
                Ok(()) => s.settle(&id, "destroy verified"),
                Err(DestroyError::NotOurs(e)) => {
                    eprintln!("{e}");
                    s.settle(&id, "not ours; nothing to destroy");
                }
                Err(DestroyError::Failed(e)) => {
                    eprintln!("{e}");
                    if !s.doomed.contains(&id) {
                        s.doomed.push(id);
                    }
                }
            }
        }
    }

    /// Records the VMID `me` was just issued as its provision in flight.
    fn began(s: &mut PoolState, me: ProcId, id: &str) {
        if let Some(pr) = s.provisioning.as_mut().filter(|pr| pr.owner == me) {
            pr.vmid = Some(id.to_string());
            s.note(id, VmState::Provisioning, "provision started");
        }
    }

    /// A provision of `me` that failed: its VM is doomed if it could not be destroyed, else gone.
    fn ended_failed(s: &mut PoolState, me: ProcId, doomed: Option<&str>, reason: &str) {
        let Some(id) = s.provisioning.as_ref().filter(|pr| pr.owner == me).and_then(|pr| pr.vmid.clone()) else { return };
        if s.state_of(&id) != Some(VmState::Provisioning) {
            return;
        }
        if doomed == Some(id.as_str()) {
            s.note(&id, VmState::Doomed, reason);
        } else {
            s.note(&id, VmState::Destroyed, reason);
        }
    }

    /// Leases `vm`, ends any outage, and starts the one background provision of the next VM
    /// if nothing is ready or in flight (G1).
    fn hand_out(s: &mut PoolState, vm: &Vm, owner: Option<ProcId>, spawner: &dyn Spawner, why: &str) {
        s.note(&vm.handle, VmState::Leased, why);
        s.leases.push(Lease { vm: vm.clone(), owner, since: now() });
        s.outage = None;
        if s.ready.is_none() && s.provisioning.is_none() {
            match spawner.spawn_next() {
                Ok(pid) => s.provisioning = Some(Provisioning { owner: pid, vmid: None, since: now() }),
                Err(e) => eprintln!("round-vm: could not start the background provision: {e}"),
            }
        }
    }

    /// Records a failed attempt: alarms if this is the first failure of an outage (G6),
    /// then either gives up (bounded retries) or waits the retry interval.
    fn failed(&self, reason: &str, alarm: &dyn Alarm, attempts: &mut u32) -> Result<(), String> {
        let first = self.with_state(|s| {
            if s.outage.is_none() {
                s.outage = Some(Outage { reason: reason.to_string(), since: now() });
                true
            } else {
                false
            }
        })?;
        if first {
            alarm.raise(reason);
        } else {
            eprintln!("round-vm: still failing: {reason}");
        }
        *attempts += 1;
        if self.max_retries > 0 && *attempts >= self.max_retries {
            return Err(format!("round-vm: acquire: giving up after {attempts} attempt(s): {reason}"));
        }
        std::thread::sleep(self.retry_interval);
        Ok(())
    }

    /// Hands out a VM, leased to `owner` (None: an external caller who will `release` it).
    /// Waits and retries for as long as it takes (or `max_retries`); never runs anything
    /// anywhere else (G5).
    pub fn acquire(&self, deps: &Deps, owner: Option<ProcId>) -> Result<(Vm, AcquireMode), String> {
        let me = ProcId::current();
        let mut attempts = 0u32;
        let mut waited = false;
        let started = std::time::Instant::now();
        let mut waiting_on = String::from("the first attempt");
        loop {
            if !self.acquire_deadline.is_zero() && started.elapsed() >= self.acquire_deadline {
                return Err(format!(
                    "round-vm: acquire: no VM within the {}s deadline (SPIRA_ROUND_VM_ACQUIRE_DEADLINE); last waiting on {waiting_on}",
                    self.acquire_deadline.as_secs()
                ));
            }
            let attempt = match (deps.factory)() {
                Ok(a) => a,
                Err(reason) => {
                    waiting_on = format!("the provider: {reason}");
                    self.failed(&reason, deps.alarm, &mut attempts)?;
                    continue;
                }
            };
            let p = attempt.provider.as_ref();
            let next = self.with_state(|s| {
                Self::reap(s, p, attempt.timing);
                if s.refreshing.map(|r| !r.alive()).unwrap_or(false) {
                    s.refreshing = None;
                }
                if let Some(vm) = s.ready.take() {
                    if p.alive(&vm.handle).unwrap_or(false) {
                        Self::hand_out(s, &vm, owner, deps.spawner, "warm VM handed out");
                        return Next::Got(vm);
                    }
                    s.note(&vm.handle, VmState::Doomed, "died while ready");
                    s.doomed.push(vm.handle);
                    Self::reap(s, p, attempt.timing);
                }
                if s.provisioning.is_some() || s.refreshing.map(|r| r.alive()).unwrap_or(false) {
                    return Next::Wait;
                }
                s.provisioning = Some(Provisioning { owner: me, vmid: None, since: now() });
                Next::Provision
            })?;
            match next {
                Next::Got(vm) => return Ok((vm, if waited { AcquireMode::Cold } else { AcquireMode::Warm })),
                Next::Wait => {
                    waited = true;
                    waiting_on = "the provision in flight to finish".into();
                    std::thread::sleep(self.wait_poll);
                }
                Next::Provision => {
                    let result = provision(p, &attempt.spec(), &mut |id| {
                        let _ = self.with_state(|s| Self::began(s, me, id));
                    });
                    let clear_mine = |s: &mut PoolState| {
                        if s.provisioning.as_ref().map(|pr| pr.owner == me).unwrap_or(false) {
                            s.provisioning = None;
                        }
                    };
                    match result {
                        Ok(vm) => {
                            self.with_state(|s| {
                                clear_mine(s);
                                Self::hand_out(s, &vm, owner, deps.spawner, "provisioned for this acquire");
                            })?;
                            return Ok((vm, AcquireMode::Cold));
                        }
                        Err(f) => {
                            self.with_state(|s| {
                                Self::ended_failed(s, me, f.doomed.as_deref(), &f.reason);
                                clear_mine(s);
                                s.doomed.extend(f.doomed.clone());
                            })?;
                            waiting_on = format!("a provision that failed: {}", f.reason);
                            self.failed(&f.reason, deps.alarm, &mut attempts)?;
                        }
                    }
                }
            }
        }
    }

    /// The background provision: provisions one VM and makes it the ready one. Runs only if
    /// this process is the recorded provisioner; a VM finished while another is somehow
    /// already ready is destroyed rather than kept (G1).
    pub fn provision_background(&self, factory: &dyn Fn() -> Result<Attempt, String>) -> Result<(), String> {
        let me = ProcId::current();
        let mine = |s: &PoolState| s.provisioning.as_ref().map(|pr| pr.owner == me).unwrap_or(false);
        if !self.with_state(|s| mine(s))? {
            return Err("round-vm: _provision-bg: not the recorded provisioner; nothing to do".into());
        }
        let attempt = match factory() {
            Ok(a) => a,
            Err(e) => {
                self.with_state(|s| {
                    if mine(s) {
                        s.provisioning = None;
                    }
                })?;
                return Err(e);
            }
        };
        let p = attempt.provider.as_ref();
        let result = provision(p, &attempt.spec(), &mut |id| {
            let _ = self.with_state(|s| Self::began(s, me, id));
        });
        self.with_state(|s| {
            if let Err(f) = &result {
                Self::ended_failed(s, me, f.doomed.as_deref(), &f.reason);
            }
            if mine(s) {
                s.provisioning = None;
            }
            match result {
                Ok(vm) if s.ready.is_none() => {
                    s.note(&vm.handle, VmState::Ready, "background provision finished");
                    s.ready = Some(vm);
                    Ok(())
                }
                Ok(vm) => {
                    s.note(&vm.handle, VmState::Doomed, "a VM was already ready");
                    s.doomed.push(vm.handle);
                    Self::reap(s, p, attempt.timing);
                    Err("round-vm: _provision-bg: a VM was already ready; destroyed the extra one".to_string())
                }
                Err(f) => {
                    s.doomed.extend(f.doomed);
                    Err(format!("round-vm: _provision-bg: {}", f.reason))
                }
            }
        })?
    }

    /// Destroys `handle` and verifies it is gone. It is recorded as doomed first, so a
    /// crash mid-destroy is finished by the next acquire.
    /// Refused, naming the holder, while a live process other than `caller` holds the lease.
    pub fn release(&self, handle: &str, caller: ProcId, factory: &dyn Fn() -> Result<Attempt, String>) -> Result<(), String> {
        self.release_inner(handle, caller, false, factory)
    }

    fn release_inner(&self, handle: &str, caller: ProcId, any_lease_holds: bool, factory: &dyn Fn() -> Result<Attempt, String>) -> Result<(), String> {
        let attempt = factory()?;
        self.with_state(|s| {
            if let Some(l) = s.leases.iter().find(|l| l.vm.handle == handle) {
                if any_lease_holds {
                    return Err(format!("round-vm: refusing to destroy VM {handle}: leased since {}", l.since));
                }
                if let Some(h) = l.owner.filter(|o| *o != caller && o.alive()) {
                    return Err(format!(
                        "round-vm: refusing to release VM {handle}: leased since {} to live run {h} (caller run {caller})",
                        l.since
                    ));
                }
            }
            s.leases.retain(|l| l.vm.handle != handle);
            if s.ready.as_ref().map(|v| v.handle == handle).unwrap_or(false) {
                s.ready = None;
            }
            match s.state_of(handle) {
                Some(VmState::Ready | VmState::Leased) => s.note(handle, VmState::Released, if any_lease_holds { "recycled" } else { "released" }),
                Some(VmState::Provisioning) => s.note(handle, VmState::Doomed, "released while provisioning"),
                _ => {}
            }
            if !s.doomed.iter().any(|d| d == handle) {
                s.doomed.push(handle.to_string());
            }
            Ok(())
        })??;
        let r = destroy_verified(attempt.provider.as_ref(), handle, attempt.timing);
        match r {
            Ok(()) | Err(DestroyError::NotOurs(_)) => {
                self.with_state(|s| {
                    s.doomed.retain(|d| d != handle);
                    s.settle(handle, "destroy verified");
                })?;
            }
            Err(DestroyError::Failed(ref e)) => {
                self.with_state(|s| {
                    if s.state_of(handle) == Some(VmState::Released) {
                        s.note(handle, VmState::Doomed, &format!("destroy failed: {e}"));
                    }
                })?;
            }
        }
        r.map_err(|e| e.to_string())
    }

    /// Destroys the warm VM and any provision in flight: both were cloned from a template that
    /// is no longer current. Leases are left to finish. Returns how many VMs it released.
    pub fn recycle(&self, factory: &dyn Fn() -> Result<Attempt, String>) -> Result<usize, String> {
        let handles = self.with_state(|s| {
            let mut h: Vec<String> = s.ready.take().map(|v| v.handle).into_iter().collect();
            if let Some(pr) = s.provisioning.take() {
                h.extend(pr.vmid);
            }
            h.retain(|id| !s.leases.iter().any(|l| &l.vm.handle == id));
            h
        })?;
        let mut released = 0;
        for h in &handles {
            match self.release_inner(h, ProcId::current(), true, factory) {
                Ok(()) => released += 1,
                Err(e) => eprintln!("{e}"),
            }
        }
        Ok(released)
    }

    /// Claims the pool for a template refresh: refused (with the reason) while a round holds
    /// a lease, a provision is in flight, or another refresh runs. While claimed, `acquire`
    /// waits, so no round starts on a VM the refresh is about to recycle.
    pub fn begin_refresh(&self, me: ProcId) -> Result<Result<(), String>, String> {
        self.with_state(|s| {
            if let Some(r) = s.refreshing.filter(|r| r.alive()) {
                return Err(format!("another refresh (pid {}) holds the pool", r.pid));
            }
            let (live, dead): (Vec<Lease>, Vec<Lease>) = s.leases.drain(..).partition(|l| l.owner.map(|o| o.alive()).unwrap_or(true));
            s.leases = live;
            for l in dead {
                s.note(&l.vm.handle, VmState::Doomed, "its lease holder is gone");
                s.doomed.push(l.vm.handle);
            }
            if let Some(l) = s.leases.first() {
                return Err(format!("a round holds VM {} (leased since {})", l.vm.handle, l.since));
            }
            if s.provisioning.as_ref().map(|p| p.owner.alive()).unwrap_or(false) {
                return Err("a VM is being provisioned for a round".into());
            }
            s.refreshing = Some(me);
            Ok(())
        })
    }

    pub fn end_refresh(&self, me: ProcId) {
        let _ = self.with_state(|s| {
            if s.refreshing == Some(me) {
                s.refreshing = None;
            }
        });
    }

    /// The VMs `owner` holds leases on, without releasing them.
    pub fn leased_to(&self, owner: ProcId) -> Vec<Vm> {
        self.with_state(|s| s.leases.iter().filter(|l| l.owner == Some(owner)).map(|l| l.vm.clone()).collect())
            .unwrap_or_default()
    }

    /// Releases every VM `owner` holds: its leases and a provision it had in flight (a `run`
    /// being interrupted).
    pub fn release_owned_by(&self, owner: ProcId, factory: &dyn Fn() -> Result<Attempt, String>) {
        let handles = self
            .with_state(|s| {
                let mut h: Vec<String> = s.leases.iter().filter(|l| l.owner == Some(owner)).map(|l| l.vm.handle.clone()).collect();
                if let Some(pr) = s.provisioning.as_ref().filter(|pr| pr.owner == owner) {
                    h.extend(pr.vmid.clone());
                    s.provisioning = None;
                }
                h
            })
            .unwrap_or_default();
        for h in handles {
            if let Err(e) = self.release(&h, owner, factory) {
                eprintln!("{e}");
            }
        }
    }

    /// Starts the one background provision when nothing is ready, in flight or being refreshed:
    /// the warm spare a due pass finds waiting. Returns whether it started one.
    pub fn ensure_spare(&self, spawner: &dyn Spawner) -> Result<bool, String> {
        self.with_state(|s| {
            let busy = s.ready.is_some()
                || s.provisioning.as_ref().map(|p| p.owner.alive()).unwrap_or(false)
                || s.refreshing.map(|r| r.alive()).unwrap_or(false);
            if busy {
                return Ok(false);
            }
            let owner = spawner.spawn_next()?;
            s.provisioning = Some(Provisioning { owner, vmid: None, since: now() });
            Ok(true)
        })?
    }

    /// `status --json`: the pool as the event log records it.
    pub fn status_json(&self) -> Result<String, String> {
        let mut s = read_state(&self.state_file())?;
        s.adopt_untracked();
        let t = now();
        let age = |since: u64| t.saturating_sub(since);
        let vms: Vec<_> = s
            .vms()
            .into_iter()
            .map(|(vm, state, since, reason)| {
                let addr = s.ready.iter().map(|v| (&v.handle, &v.addr)).chain(s.leases.iter().map(|l| (&l.vm.handle, &l.vm.addr))).find(|(h, _)| **h == vm).map(|(_, a)| a.clone());
                serde_json::json!({"vm": vm, "state": state.as_str(), "since": since, "age_secs": age(since), "reason": reason, "addr": addr})
            })
            .collect();
        let leases: Vec<_> = s
            .leases
            .iter()
            .map(|l| serde_json::json!({"vm": l.vm.handle, "addr": l.vm.addr, "owner": l.owner.map(|o| o.to_string()), "since": l.since, "age_secs": age(l.since)}))
            .collect();
        let doomed: Vec<_> = s
            .doomed
            .iter()
            .map(|h| {
                let since = s.events.iter().rev().find(|e| &e.vm == h).map_or(0, |e| e.at);
                serde_json::json!({"vm": h, "since": since, "age_secs": age(since)})
            })
            .collect();
        let tail = s.events.len().saturating_sub(50);
        let v = serde_json::json!({
            "ready": s.ready.as_ref().map(|v| serde_json::json!({"vm": v.handle, "addr": v.addr})),
            "provisioning": s.provisioning.as_ref().filter(|p| p.owner.alive()).map(|p| serde_json::json!({"owner": p.owner.to_string(), "vm": p.vmid, "since": p.since, "age_secs": age(p.since)})),
            "refreshing": s.refreshing.filter(|r| r.alive()).map(|r| r.to_string()),
            "outage": s.outage.as_ref().map(|o| serde_json::json!({"reason": o.reason, "since": o.since, "age_secs": age(o.since)})),
            "template": crate::template::Record::read(&self.state_dir).map(|r| serde_json::json!({"vmid": r.vmid, "image": r.image})),
            "leases": leases,
            "doomed": doomed,
            "vms": vms,
            "events": &s.events[tail..],
        });
        serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
    }

    /// The three `status` lines.
    pub fn status(&self) -> Result<String, String> {
        let s = read_state(&self.state_file())?;
        let ready = s.ready.map(|v| format!("{} {}", v.handle, v.addr)).unwrap_or_else(|| "none".into());
        let prov = s
            .provisioning
            .filter(|p| p.owner.alive())
            .map(|p| format!("pid {}", p.owner.pid))
            .unwrap_or_else(|| "none".into());
        let outage = s.outage.map(|o| o.reason).unwrap_or_else(|| "none".into());
        Ok(format!("ready: {ready}\nprovisioning: {prov}\noutage: {outage}\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeAlarm, FakeProvider, Step, TempDir};
    use std::cell::Cell;
    use std::sync::{Arc, Mutex};

    const T: Timing = Timing { boot_tries: 2, poll: Duration::ZERO, gone_tries: 2 };

    fn pool(d: &TempDir, max_retries: u32) -> Pool {
        Pool {
            state_dir: d.path().join("state"),
            retry_interval: Duration::ZERO,
            max_retries,
            wait_poll: Duration::from_millis(10),
            acquire_deadline: Duration::ZERO,
        }
    }

    fn attempt(p: &FakeProvider) -> Attempt {
        Attempt { provider: Box::new(p.clone()), iface: "ens18".into(), ssh_user: "root".into(), pubkey: "k".into(), timing: T }
    }

    /// Records spawns; hands back the test process's own identity as the "background"
    /// provisioner, so a later `provision_background` in the test is the recorded owner.
    #[derive(Default)]
    struct FakeSpawner(Mutex<u32>);
    impl Spawner for FakeSpawner {
        fn spawn_next(&self) -> Result<ProcId, String> {
            *self.0.lock().unwrap() += 1;
            Ok(ProcId::current())
        }
    }
    impl FakeSpawner {
        fn count(&self) -> u32 {
            *self.0.lock().unwrap()
        }
    }

    fn dead_proc() -> ProcId {
        let me = ProcId::current();
        ProcId { pid: me.pid, start: me.start + 12345 }
    }

    #[test]
    fn cold_acquire_provisions_leases_and_starts_exactly_one_background_provision() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let (vm, mode) = pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap();
        assert_eq!(mode, AcquireMode::Cold);
        assert_eq!(sp.count(), 1);
        let s = read_state(&pl.state_file()).unwrap();
        assert_eq!(s.leases.len(), 1);
        assert_eq!(s.leases[0].vm, vm);
        assert!(s.provisioning.is_some(), "the background provision is recorded");
        assert!(alarm.sent().is_empty());
    }

    #[test]
    fn warm_acquire_returns_the_ready_vm_and_starts_exactly_one_more_provision() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &sp };
        let (first, _) = pl.acquire(&deps, None).unwrap();
        pl.provision_background(&f).unwrap();
        let ready = read_state(&pl.state_file()).unwrap().ready.expect("background provision left a ready VM");
        assert_ne!(ready.handle, first.handle);

        let (second, mode) = pl.acquire(&deps, None).unwrap();
        assert_eq!(mode, AcquireMode::Warm);
        assert_eq!(second, ready);
        assert_eq!(sp.count(), 2, "one background provision per acquire");
        assert_eq!(fp.count("clone"), 2, "the warm acquire cloned nothing itself");
        assert!(read_state(&pl.state_file()).unwrap().ready.is_none());
    }

    #[test]
    fn a_ready_vm_that_died_is_destroyed_and_replaced_cold() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &sp };
        pl.acquire(&deps, None).unwrap();
        pl.provision_background(&f).unwrap();
        let ready = read_state(&pl.state_file()).unwrap().ready.unwrap();
        fp.kill(&ready.handle);
        let (vm, mode) = pl.acquire(&deps, None).unwrap();
        assert_eq!(mode, AcquireMode::Cold);
        assert_ne!(vm.handle, ready.handle);
        assert!(fp.name(&ready.handle).is_none(), "the dead ready VM was destroyed");
    }

    #[test]
    fn background_provision_never_makes_a_second_ready_vm() {
        let d = TempDir::new();
        let fp = FakeProvider::new();
        let pl = pool(&d, 1);
        pl.with_state(|s| {
            s.ready = Some(Vm { handle: "7".into(), addr: "a".into() });
            s.provisioning = Some(Provisioning { owner: ProcId::current(), vmid: None, since: 0 });
        })
        .unwrap();
        let f = || Ok(attempt(&fp));
        assert!(pl.provision_background(&f).is_err());
        let s = read_state(&pl.state_file()).unwrap();
        assert_eq!(s.ready.unwrap().handle, "7");
        assert!(s.provisioning.is_none());
        assert!(fp.live_vms().is_empty(), "the extra VM was destroyed: {:?}", fp.live_vms());
    }

    #[test]
    fn background_provision_runs_only_as_the_recorded_provisioner() {
        let d = TempDir::new();
        let fp = FakeProvider::new();
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        assert!(pl.provision_background(&f).is_err());
        assert_eq!(fp.count("clone"), 0);
    }

    #[test]
    fn acquire_waits_on_the_provision_in_flight_instead_of_starting_a_second() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = Arc::new(pool(&d, 1));
        pl.with_state(|s| s.provisioning = Some(Provisioning { owner: ProcId::current(), vmid: None, since: 0 })).unwrap();
        fp.plant("555", "round-555");
        let pl2 = Arc::clone(&pl);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            pl2.with_state(|s| {
                s.provisioning = None;
                s.ready = Some(Vm { handle: "555".into(), addr: "10.0.0.555".into() });
            })
            .unwrap();
        });
        let f = || Ok(attempt(&fp));
        let (vm, mode) = pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap();
        t.join().unwrap();
        assert_eq!(vm.handle, "555");
        assert_eq!(mode, AcquireMode::Cold, "it had to wait, so it was not warm");
        assert_eq!(fp.count("clone"), 0, "no second provision was started");
    }

    #[test]
    fn acquire_against_a_pool_that_never_fills_fails_within_its_deadline() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let mut pl = pool(&d, 0);
        pl.acquire_deadline = Duration::from_millis(200);
        pl.with_state(|s| s.provisioning = Some(Provisioning { owner: ProcId::current(), vmid: None, since: 0 })).unwrap();
        let f = || Ok(attempt(&fp));
        let t = std::time::Instant::now();
        let e = pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap_err();
        assert!(t.elapsed() < Duration::from_secs(5), "returned late: {:?}", t.elapsed());
        assert!(e.contains("deadline") && e.contains("provision in flight"), "{e}");
    }

    #[test]
    fn a_dead_provisioner_s_vm_is_destroyed_by_the_next_acquire() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        fp.plant("444", "round-444");
        pl.with_state(|s| s.provisioning = Some(Provisioning { owner: dead_proc(), vmid: Some("444".into()), since: 0 })).unwrap();
        let f = || Ok(attempt(&fp));
        pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap();
        assert!(fp.name("444").is_none());
    }

    #[test]
    fn a_dead_run_s_lease_is_destroyed_but_an_external_lease_is_kept() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        fp.plant("301", "round-301");
        fp.plant("302", "round-302");
        pl.with_state(|s| {
            s.leases.push(Lease { vm: Vm { handle: "301".into(), addr: "x".into() }, owner: Some(dead_proc()), since: 0 });
            s.leases.push(Lease { vm: Vm { handle: "302".into(), addr: "y".into() }, owner: None, since: 0 });
        })
        .unwrap();
        let f = || Ok(attempt(&fp));
        pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap();
        assert!(fp.name("301").is_none());
        assert!(fp.name("302").is_some());
    }

    #[test]
    fn outage_alarms_once_and_never_fabricates_a_vm() {
        let d = TempDir::new();
        let (alarm, sp) = (FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 3);
        let calls = Cell::new(0);
        let f = || {
            calls.set(calls.get() + 1);
            Err::<Attempt, _>("round-vm: PVE_TEMPLATE_VMID not set".to_string())
        };
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &sp };
        let e = pl.acquire(&deps, None).unwrap_err();
        assert!(e.contains("giving up after 3"), "{e}");
        assert_eq!(calls.get(), 3, "config re-read on every attempt");
        assert_eq!(alarm.sent(), vec!["round-vm: PVE_TEMPLATE_VMID not set".to_string()]);
        assert!(read_state(&pl.state_file()).unwrap().leases.is_empty());

        // A later acquire in the same outage stays silent, even for a different reason.
        let fp = FakeProvider::new();
        fp.fail(Step::NextId);
        let g = || Ok(attempt(&fp));
        pl.acquire(&Deps { factory: &g, alarm: &alarm, spawner: &sp }, None).unwrap_err();
        assert_eq!(alarm.sent().len(), 1);
        assert!(pl.status().unwrap().contains("outage: round-vm: PVE_TEMPLATE_VMID not set"));

        // Success ends the outage; the next one alarms anew.
        fp.heal();
        pl.acquire(&Deps { factory: &g, alarm: &alarm, spawner: &sp }, None).unwrap();
        assert!(pl.status().unwrap().contains("outage: none"));
        fp.fail(Step::Clone);
        pl.with_state(|s| s.provisioning = None).unwrap();
        pl.acquire(&Deps { factory: &g, alarm: &alarm, spawner: &sp }, None).unwrap_err();
        assert_eq!(alarm.sent().len(), 2);
        assert!(alarm.sent()[1].contains("clone refused"), "{:?}", alarm.sent());
    }

    #[test]
    fn a_failure_is_never_cached_the_next_retry_rereads_and_succeeds() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 0);
        let calls = Cell::new(0);
        let f = || {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err("pve.env missing".to_string())
            } else {
                Ok(attempt(&fp))
            }
        };
        let (_, mode) = pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap();
        assert_eq!(mode, AcquireMode::Cold);
        assert_eq!(calls.get(), 3);
        assert_eq!(alarm.sent().len(), 1);
    }

    #[test]
    fn failed_provisions_leak_nothing_across_retries() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        fp.fail(Step::FileWrite);
        let pl = pool(&d, 4);
        let f = || Ok(attempt(&fp));
        pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap_err();
        assert_eq!(fp.count("clone"), 4);
        assert!(fp.live_vms().is_empty(), "{:?}", fp.live_vms());
        let s = read_state(&pl.state_file()).unwrap();
        assert!(s.provisioning.is_none() && s.doomed.is_empty() && s.leases.is_empty());
    }

    #[test]
    fn an_undestroyable_vm_stays_doomed_and_is_retried() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        fp.fail(Step::Start);
        fp.fail(Step::Destroy);
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap_err();
        assert_eq!(read_state(&pl.state_file()).unwrap().doomed, vec!["100".to_string()]);
        fp.heal();
        pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap();
        assert!(fp.name("100").is_none());
        assert!(read_state(&pl.state_file()).unwrap().doomed.is_empty());
    }

    #[test]
    fn release_destroys_verifies_and_forgets_the_lease() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let (vm, _) = pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap();
        pl.release(&vm.handle, ProcId::current(), &f).unwrap();
        assert!(fp.name(&vm.handle).is_none());
        let s = read_state(&pl.state_file()).unwrap();
        assert!(s.leases.is_empty() && s.doomed.is_empty());
    }

    #[test]
    fn release_refuses_a_vm_leased_to_another_live_run_and_names_it() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let holder = ProcId::current();
        let (vm, _) = pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, Some(holder)).unwrap();
        let stranger = ProcId { pid: holder.pid, start: holder.start + 1 };
        let e = pl.release(&vm.handle, stranger, &f).unwrap_err();
        assert!(e.contains("refusing") && e.contains(&holder.to_string()), "{e}");
        assert!(fp.name(&vm.handle).is_some());
        assert_eq!(fp.count("stop"), 0);
        assert_eq!(fp.count("destroy"), 0);
        assert_eq!(read_state(&pl.state_file()).unwrap().leases.len(), 1);
        pl.release(&vm.handle, holder, &f).unwrap();
        assert!(fp.name(&vm.handle).is_none());
    }

    #[test]
    fn a_stale_lease_is_released_by_anyone() {
        let d = TempDir::new();
        let fp = FakeProvider::new();
        fp.plant("311", "round-311");
        let pl = pool(&d, 1);
        pl.with_state(|s| {
            s.leases.push(Lease { vm: Vm { handle: "311".into(), addr: "x".into() }, owner: Some(dead_proc()), since: 0 })
        })
        .unwrap();
        let f = || Ok(attempt(&fp));
        pl.release("311", ProcId::current(), &f).unwrap();
        assert!(fp.name("311").is_none());
    }

    #[test]
    fn a_second_acquire_never_stops_the_first_runs_vm() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &sp };
        let (first, _) = pl.acquire(&deps, Some(ProcId::current())).unwrap();
        pl.with_state(|s| s.provisioning = None).unwrap();
        let (second, _) = pl.acquire(&deps, Some(ProcId::current())).unwrap();
        assert_ne!(first.handle, second.handle);
        assert!(fp.name(&first.handle).is_some());
        assert_eq!(fp.count("stop"), 0);
    }

    #[test]
    fn release_refuses_a_vm_that_is_not_ours() {
        let d = TempDir::new();
        let fp = FakeProvider::new();
        fp.plant("9000", "ci-template");
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let e = pl.release("9000", ProcId::current(), &f).unwrap_err();
        assert!(e.contains("refusing"), "{e}");
        assert!(fp.name("9000").is_some());
        assert!(read_state(&pl.state_file()).unwrap().doomed.is_empty());
    }

    #[test]
    fn release_owned_by_frees_only_that_owner_s_vms() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &sp };
        let me = ProcId::current();
        let (mine, _) = pl.acquire(&deps, Some(me)).unwrap();
        pl.with_state(|s| s.provisioning = None).unwrap();
        let (theirs, _) = pl.acquire(&deps, None).unwrap();
        fp.plant("777", "round-777");
        pl.with_state(|s| s.provisioning = Some(Provisioning { owner: me, vmid: Some("777".into()), since: 0 })).unwrap();
        pl.release_owned_by(me, &f);
        assert!(fp.name(&mine.handle).is_none());
        assert!(fp.name("777").is_none(), "its provision in flight is destroyed too");
        assert!(fp.name(&theirs.handle).is_some());
        assert!(read_state(&pl.state_file()).unwrap().provisioning.is_none());
    }

    #[test]
    fn recycle_destroys_the_warm_vm_and_the_provision_but_not_a_lease() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &sp };
        let (leased, _) = pl.acquire(&deps, None).unwrap();
        fp.plant("201", "round-201");
        fp.plant("202", "round-202");
        pl.with_state(|s| {
            s.ready = Some(Vm { handle: "201".into(), addr: "10.0.0.1".into() });
            s.provisioning = Some(Provisioning { owner: ProcId::current(), vmid: Some("202".into()), since: 0 });
        })
        .unwrap();
        assert_eq!(pl.recycle(&f).unwrap(), 2);
        assert!(fp.name("201").is_none() && fp.name("202").is_none());
        assert!(fp.name(&leased.handle).is_some(), "a run in progress keeps its VM");
        let s = read_state(&pl.state_file()).unwrap();
        assert!(s.ready.is_none() && s.provisioning.is_none() && s.doomed.is_empty());
    }

    #[test]
    fn a_refresh_is_refused_while_a_round_holds_a_vm_and_blocks_acquire_while_it_runs() {
        let d = TempDir::new();
        let pl = pool(&d, 3);
        let me = ProcId::current();
        let fp = FakeProvider::new();
        let f = || Ok(attempt(&fp));
        let sp = FakeSpawner::default();
        let al = FakeAlarm::default();
        let deps = Deps { factory: &f, alarm: &al, spawner: &sp };
        pl.acquire(&deps, Some(me)).unwrap();
        let why = pl.begin_refresh(me).unwrap().unwrap_err();
        assert!(why.contains("a round holds VM"), "{why}");
        pl.with_state(|s| s.leases.clear()).unwrap();
        pl.with_state(|s| s.provisioning = None).unwrap();
        pl.begin_refresh(me).unwrap().unwrap();
        assert!(pl.begin_refresh(me).unwrap().unwrap_err().contains("another refresh"));
        pl.end_refresh(me);
        assert!(pl.with_state(|s| s.refreshing).unwrap().is_none());
    }

    #[test]
    fn a_recycle_never_destroys_a_vm_another_caller_leases() {
        let d = TempDir::new();
        let pl = pool(&d, 3);
        let fp = FakeProvider::new();
        let f = || Ok(attempt(&fp));
        let sp = FakeSpawner::default();
        let al = FakeAlarm::default();
        let deps = Deps { factory: &f, alarm: &al, spawner: &sp };
        let (leased, _) = pl.acquire(&deps, None).unwrap();
        pl.with_state(|s| {
            s.ready = Some(leased.clone());
            s.provisioning = Some(Provisioning { owner: ProcId::current(), vmid: Some(leased.handle.clone()), since: 0 });
        })
        .unwrap();
        assert_eq!(pl.recycle(&f).unwrap(), 0);
        assert!(fp.name(&leased.handle).is_some(), "the leased VM is untouched");
        assert!(pl.with_state(|s| s.leases.len()).unwrap() == 1);
    }

    #[test]
    fn status_reports_ready_provisioning_and_outage() {
        let d = TempDir::new();
        let pl = pool(&d, 1);
        assert_eq!(pl.status().unwrap(), "ready: none\nprovisioning: none\noutage: none\n");
        pl.with_state(|s| {
            s.ready = Some(Vm { handle: "123".into(), addr: "192.168.1.149".into() });
            s.provisioning = Some(Provisioning { owner: ProcId::current(), vmid: None, since: 0 });
            s.outage = Some(Outage { reason: "clone refused".into(), since: 0 });
        })
        .unwrap();
        assert_eq!(
            pl.status().unwrap(),
            format!("ready: 123 192.168.1.149\nprovisioning: pid {}\noutage: clone refused\n", std::process::id())
        );
        pl.with_state(|s| s.provisioning = Some(Provisioning { owner: dead_proc(), vmid: None, since: 0 })).unwrap();
        assert!(pl.status().unwrap().contains("provisioning: none"));
    }

    fn trail(pl: &Pool, vm: &str) -> Vec<(VmState, String)> {
        read_state(&pl.state_file()).unwrap().events.iter().filter(|e| e.vm == vm).map(|e| (e.to, e.reason.clone())).collect()
    }

    #[test]
    fn a_cold_acquire_and_release_record_provisioning_leased_released_destroyed() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let (vm, _) = pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap();
        let states = |pl: &Pool| trail(pl, &vm.handle).into_iter().map(|(s, _)| s).collect::<Vec<_>>();
        assert_eq!(states(&pl), vec![VmState::Provisioning, VmState::Leased]);
        pl.release(&vm.handle, ProcId::current(), &f).unwrap();
        assert_eq!(states(&pl), vec![VmState::Provisioning, VmState::Leased, VmState::Released, VmState::Destroyed]);
        assert!(trail(&pl, &vm.handle).iter().all(|(_, why)| !why.is_empty()));
    }

    #[test]
    fn a_background_provision_records_ready_and_a_warm_acquire_leases_it() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &sp };
        pl.acquire(&deps, None).unwrap();
        pl.provision_background(&f).unwrap();
        let ready = pl.with_state(|s| s.ready.clone()).unwrap().unwrap();
        assert_eq!(trail(&pl, &ready.handle).last().map(|t| t.0), Some(VmState::Ready));
        let (vm, mode) = pl.acquire(&deps, None).unwrap();
        assert_eq!((vm.handle == ready.handle, mode), (true, AcquireMode::Warm));
        assert_eq!(trail(&pl, &vm.handle).last().map(|t| t.0), Some(VmState::Leased));
    }

    #[test]
    fn status_json_returns_exactly_the_recorded_state() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        let f = || Ok(attempt(&fp));
        let (vm, _) = pl.acquire(&Deps { factory: &f, alarm: &alarm, spawner: &sp }, None).unwrap();
        let j: serde_json::Value = serde_json::from_str(&pl.status_json().unwrap()).unwrap();
        assert_eq!(j["leases"][0]["vm"], vm.handle.as_str());
        assert_eq!(j["vms"][0]["state"], "leased");
        assert_eq!(j["vms"][0]["vm"], vm.handle.as_str());
        assert!(j["ready"].is_null() && j["template"].is_null() && j["doomed"].as_array().unwrap().is_empty());
        assert_eq!(j["provisioning"]["owner"], ProcId::current().to_string());
        pl.release(&vm.handle, ProcId::current(), &f).unwrap();
        let j: serde_json::Value = serde_json::from_str(&pl.status_json().unwrap()).unwrap();
        assert!(j["leases"].as_array().unwrap().is_empty() && j["vms"].as_array().unwrap().is_empty());
        assert_eq!(j["events"].as_array().unwrap().last().unwrap()["to"], "destroyed");
    }

    #[test]
    fn status_json_shows_a_doomed_vm_and_the_template() {
        let d = TempDir::new();
        let pl = pool(&d, 1);
        crate::template::Record { vmid: "900".into(), image: "reg:tag".into() }.write(&pl.state_dir).unwrap_or_else(|_| {
            std::fs::create_dir_all(&pl.state_dir).unwrap();
            crate::template::Record { vmid: "900".into(), image: "reg:tag".into() }.write(&pl.state_dir).unwrap();
        });
        pl.with_state(|s| s.doomed.push("77".into())).unwrap();
        let j: serde_json::Value = serde_json::from_str(&pl.status_json().unwrap()).unwrap();
        assert_eq!((j["doomed"][0]["vm"].as_str(), j["template"]["vmid"].as_str()), (Some("77"), Some("900")));
        assert_eq!(j["vms"][0]["state"], "doomed");
    }

    #[test]
    fn a_warm_spare_is_started_only_when_nothing_is_ready_or_in_flight() {
        let d = TempDir::new();
        let (fp, alarm, sp) = (FakeProvider::new(), FakeAlarm::default(), FakeSpawner::default());
        let pl = pool(&d, 1);
        assert!(pl.ensure_spare(&sp).unwrap());
        assert!(!pl.ensure_spare(&sp).unwrap(), "one provision in flight is the spare");
        assert_eq!(sp.count(), 1);
        let f = || Ok(attempt(&fp));
        let _ = &alarm;
        pl.provision_background(&f).unwrap();
        assert!(!pl.ensure_spare(&sp).unwrap(), "a ready VM is the spare");
        assert_eq!(sp.count(), 1);
    }
}
