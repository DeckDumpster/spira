//! The store's hot reads, in this process (design S7b). [`start`] builds the projection once and
//! installs it for every reader in the process, then the `store.changed` source keeps it current on the store's
//! `watch` long-poll. A reader whose projection fell behind or lost its watch is refused, never handed an
//! old picture.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use runtime::changes::{AppliedRev, Changes};
use spira_store::lc_call::LcClient;
use store_view::Projection;

/// The revision of the store the in-process copy has applied, set by the `store.changed` source.
#[derive(Default)]
pub struct ChangeTable(AppliedRev);

impl ChangeTable {
    /// Moves the table to `rev`; it never moves back.
    pub fn set(&self, rev: u64) {
        self.0.set(rev);
    }
}

impl ChangeTable {
    /// A confirmation with no event: it gives a table that holds nothing its first revision and
    /// moves no other, so a write outside every watched prefix never reads as a change.
    pub fn settle(&self, rev: u64) {
        if self.0.rev().is_none() {
            self.0.set(rev);
        }
    }
}

impl Changes for ChangeTable {
    fn rev(&self) -> Option<u64> {
        self.0.rev()
    }
}

/// How long the scheduler's watch holds when the store is quiet: "30s is fine" (Ryan, 2026-11-22).
pub const SCHEDULER_WATCH_HOLD_MS: u64 = 30_000;

/// Installs the projection of the store on `lc_socket` and loads it. The first load is tried
/// here so the first tick reads; a store that does not answer yet is the watch's to load once it
/// does, and readers refuse until then. The watch itself is the `store.changed` source's.
pub fn start(lc_socket: &Path) -> Result<(Arc<Projection>, Arc<ChangeTable>), String> {
    install(Projection::configured(LcClient::socket(lc_socket))?.with_prefixes(&crate::tasks::watched_prefixes()).with_watch_wait(SCHEDULER_WATCH_HOLD_MS))
}

fn install(views: Projection) -> Result<(Arc<Projection>, Arc<ChangeTable>), String> {
    let views = Arc::new(views);
    if !Arc::ptr_eq(&store_view::install(views.clone()), &views) {
        return Err("the store views are already installed in this process".into());
    }
    if let Err(e) = views.load() {
        eprintln!("spira-runtime: store views not loaded at boot: {e}");
    }
    Ok((views, Arc::new(ChangeTable::default())))
}

/// The watch thread of [`run`]; dropping it asks the thread to stop.
pub struct Store {
    stop: Arc<AtomicBool>,
}

impl Drop for Store {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// [`install`] on a projection already built, with a bare watch thread and no source: for a test
/// that keeps a projection current without a host.
pub fn run(views: Projection) -> Result<Store, String> {
    let (views, _) = install(views)?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::Builder::new().name("store-watch".into()).spawn(move || spira_store::lc_call::as_task("store-watch", || views.run(&flag, &mut |_| {}))).map_err(|e| format!("store watch thread: {e}"))?;
    Ok(Store { stop })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t1tj0so)]
    #[test]
    fn a_quiet_confirmation_gives_an_empty_table_its_revision_and_moves_no_other() {
        let t = ChangeTable::default();
        t.settle(5);
        assert_eq!(t.rev(), Some(5));
        t.settle(9);
        assert_eq!(t.rev(), Some(5), "a write outside every watched prefix is no change");
        t.set(9);
        assert_eq!(t.rev(), Some(9));
    }
}
