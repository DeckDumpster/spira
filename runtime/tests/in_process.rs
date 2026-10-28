//! The conformance suite on the in-process executor with the in-memory journal.

use runtime::conformance::*;
use runtime::journal::MemJournal;

fn rig() -> impl Rig {
    InProcessRig(|| MemJournal::new() as std::sync::Arc<dyn runtime::Journal>)
}


mod conformance {
    use super::*;
    use runtime::conformance as super_;
    fn rig() -> impl Rig {
        super::rig()
    }
    macro_rules! s {
        ($($name:ident),*) => { $( #[test] fn $name() { super_::$name(&rig()); } )* };
    }
    s!(
        redelivery_of_any_event,
        crash_between_append_and_dispatch,
        crash_during_dispatch_appends_nothing_twice,
        crash_between_effect_request_and_outcome,
        out_of_order_effect_outcomes,
        effect_deadline_times_out,
        determinism_under_replay,
        handler_panic_quarantines_its_stream_only,
        schedule_rows_survive_restart_and_fire_once
    );
}
