//! Effector classes. Every effect runs under the cap of its class, so a slow class (a container
//! run, a forge call) cannot take the permits another class needs. A class at its cap refuses
//! the effect at once; the host records the refusal and offers the effect again later.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectClass {
    Store,
    Unit,
    Git,
    Forge,
    #[default]
    Host,
    Container,
}

impl EffectClass {
    pub const ALL: [EffectClass; 6] = [EffectClass::Store, EffectClass::Unit, EffectClass::Git, EffectClass::Forge, EffectClass::Host, EffectClass::Container];

    pub fn name(self) -> &'static str {
        match self {
            EffectClass::Store => "store",
            EffectClass::Unit => "unit",
            EffectClass::Git => "git",
            EffectClass::Forge => "forge",
            EffectClass::Host => "host",
            EffectClass::Container => "container",
        }
    }

    pub fn parse(s: &str) -> Option<EffectClass> {
        EffectClass::ALL.into_iter().find(|c| c.name() == s)
    }
}

/// The host run ceiling: container runs (a test VM, a podman suite) in flight at once on this
/// host. More than this has filled swap and stalled the user manager; the cap is clamped to it.
pub const HOST_RUN_CEILING: usize = 2;

/// Each cap counts the effects of that class running at once in this process, measured as
/// permits held from runner start to runner return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolCaps {
    /// Calls to the lifecycle store.
    pub store: usize,
    /// systemd unit starts, stops and reads.
    pub unit: usize,
    /// git processes on a local checkout.
    pub git: usize,
    /// Calls to the forge's API or CLI.
    pub forge: usize,
    /// Everything else on this host: files, mail, local sockets.
    pub host: usize,
    /// Container and VM runs; never above [`HOST_RUN_CEILING`].
    pub container: usize,
}

impl Default for PoolCaps {
    fn default() -> Self {
        PoolCaps { store: 4, unit: 4, git: 2, forge: 2, host: 4, container: HOST_RUN_CEILING }
    }
}

impl PoolCaps {
    /// `n` for every class, the container class held to the ceiling.
    pub fn uniform(n: usize) -> Self {
        PoolCaps { store: n, unit: n, git: n, forge: n, host: n, container: n.min(HOST_RUN_CEILING) }
    }

    /// The cap in force: at least one, so a class never wedges; container at most the ceiling.
    pub fn cap(&self, class: EffectClass) -> usize {
        let raw = match class {
            EffectClass::Store => self.store,
            EffectClass::Unit => self.unit,
            EffectClass::Git => self.git,
            EffectClass::Forge => self.forge,
            EffectClass::Host => self.host,
            EffectClass::Container => self.container.min(HOST_RUN_CEILING),
        };
        raw.max(1)
    }
}

impl From<usize> for PoolCaps {
    fn from(n: usize) -> Self {
        PoolCaps::uniform(n)
    }
}

/// A class at its cap refused an effect. Nothing ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deferred {
    pub class: EffectClass,
    pub in_use: usize,
    pub cap: usize,
}

struct Pool {
    sem: Arc<Semaphore>,
    cap: usize,
    deferred: AtomicU64,
}

pub struct ClassPools {
    pools: BTreeMap<EffectClass, Pool>,
}

impl ClassPools {
    pub fn new(caps: PoolCaps) -> Self {
        let pools = EffectClass::ALL.into_iter().map(|c| (c, Pool { sem: Arc::new(Semaphore::new(caps.cap(c))), cap: caps.cap(c), deferred: AtomicU64::new(0) })).collect();
        ClassPools { pools }
    }

    fn pool(&self, class: EffectClass) -> &Pool {
        self.pools.get(&class).expect("every class has a pool")
    }

    /// A permit held until the runner returns, or a refusal when the class is at its cap.
    pub fn try_take(&self, class: EffectClass) -> Result<OwnedSemaphorePermit, Deferred> {
        let p = self.pool(class);
        p.sem.clone().try_acquire_owned().map_err(|_| {
            p.deferred.fetch_add(1, Ordering::SeqCst);
            Deferred { class, in_use: self.in_use(class), cap: p.cap }
        })
    }

    pub fn in_use(&self, class: EffectClass) -> usize {
        let p = self.pool(class);
        p.cap - p.sem.available_permits().min(p.cap)
    }

    pub fn cap(&self, class: EffectClass) -> usize {
        self.pool(class).cap
    }

    /// Effects this class has refused since the pools were built.
    pub fn deferred_total(&self, class: EffectClass) -> u64 {
        self.pool(class).deferred.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_container_cap_is_held_to_the_host_run_ceiling_and_no_cap_is_zero() {
        let caps = PoolCaps { container: 9, git: 0, ..PoolCaps::uniform(5) };
        assert_eq!(caps.cap(EffectClass::Container), HOST_RUN_CEILING);
        assert_eq!(caps.cap(EffectClass::Git), 1);
        assert_eq!(caps.cap(EffectClass::Forge), 5);
        assert_eq!(PoolCaps::from(4).cap(EffectClass::Container), HOST_RUN_CEILING);
    }

    #[test]
    fn a_full_class_refuses_without_touching_another() {
        let pools = ClassPools::new(PoolCaps { container: 1, ..PoolCaps::default() });
        let held = pools.try_take(EffectClass::Container).unwrap();
        let refused = pools.try_take(EffectClass::Container).unwrap_err();
        assert_eq!(refused, Deferred { class: EffectClass::Container, in_use: 1, cap: 1 });
        assert_eq!(pools.deferred_total(EffectClass::Container), 1);
        assert!(pools.try_take(EffectClass::Host).is_ok(), "another class is unaffected");
        drop(held);
        assert!(pools.try_take(EffectClass::Container).is_ok(), "a freed permit is taken again");
    }

    #[test]
    fn class_names_round_trip() {
        for c in EffectClass::ALL {
            assert_eq!(EffectClass::parse(c.name()), Some(c));
        }
        assert_eq!(EffectClass::parse("nope"), None);
    }
}
