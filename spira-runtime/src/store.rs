//! The store's hot reads, in this process (design S7b). [`start`] builds the projection once and
//! installs it for every reader in the process, then keeps it current on the store's `watch`
//! long-poll. A reader whose projection fell behind or lost its watch is refused, never handed an
//! old picture.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use spira_config::lc_call::LcClient;
use store_view::Projection;

/// The watch thread; dropping it asks the thread to stop.
pub struct Store {
    stop: Arc<AtomicBool>,
}

impl Drop for Store {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Installs the projection of the store on `lc_socket` and starts its watch. The first load is
/// tried here so the first tick reads; a store that does not answer yet is the watch thread's to
/// load once it does, and readers refuse until then.
pub fn start(lc_socket: &Path) -> Result<Store, String> {
    run(Projection::configured(LcClient::socket(lc_socket))?)
}

/// [`start`] on a projection already built.
pub fn run(views: Projection) -> Result<Store, String> {
    let views = Arc::new(views);
    if !Arc::ptr_eq(&store_view::install(views.clone()), &views) {
        return Err("the store views are already installed in this process".into());
    }
    if let Err(e) = views.load() {
        eprintln!("spira-runtime: store views not loaded at boot: {e}");
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::Builder::new().name("store-watch".into()).spawn(move || spira_config::lc_call::as_task("store-watch", || views.run(&flag))).map_err(|e| format!("store watch thread: {e}"))?;
    Ok(Store { stop })
}
