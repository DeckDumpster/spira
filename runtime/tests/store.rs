//! The conformance suite on the lifecycle-store journal: container tier, against a throwaway
//! Dolt database holding `lifecycle/schema.sql`. `RUNTIME_STORE_DIR` names that database's
//! directory; the tests are ignored until the tier supplies it.

use runtime::conformance::*;
use runtime::store::{DoltCli, StoreJournal};
use std::sync::Arc;

fn rig() -> impl Rig {
    let dir = std::env::var("RUNTIME_STORE_DIR").expect("RUNTIME_STORE_DIR names the throwaway database directory");
    InProcessRig(move || {
        let j = StoreJournal::new(DoltCli { dir: dir.clone().into() });
        j.reset().expect("empty the runtime tables");
        Arc::new(j) as Arc<dyn runtime::Journal>
    })
}

#[test]
#[ignore = "container tier: needs RUNTIME_STORE_DIR (a throwaway Dolt database with lifecycle/schema.sql)"]
fn the_whole_suite_on_the_lifecycle_store() {
    run_all(&rig());
}
