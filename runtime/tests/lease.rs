//! The conformance suite on the lease executor, and the lease-steal scenarios: in one process on
//! virtual time, and across two real processes with the holder killed mid-stream.

use std::sync::Arc;

use runtime::conformance::Rig;
use runtime::journal::{Journal, MemJournal};
use runtime::lease::LeaseRig;

mod conformance {
    use super::*;
    use runtime::conformance as c;

    fn mem() -> impl Rig {
        LeaseRig::new(|| MemJournal::new() as Arc<dyn Journal>)
    }

    macro_rules! s {
        ($($name:ident),*) => { $( #[test] fn $name() { c::$name(&mem()); } )* };
    }
    s!(
        redelivery_of_any_event,
        crash_during_dispatch_appends_nothing_twice,
        crash_between_effect_request_and_outcome,
        out_of_order_effect_outcomes,
        effect_deadline_times_out,
        determinism_under_replay,
        schedule_rows_survive_restart_and_fire_once,
        observe_shadows_writes_runs_probes_and_never_replays_them,
        halt_holds_writes_runs_probes_and_resume_releases_once_in_order
    );
}
