//! `Command::EmitTo`: one task wakes another stream's handler exactly once, shadowed under
//! observe and running under a halt.

use std::sync::Arc;

use runtime::conformance::{self as suite, InProcessRig};
use runtime::journal::{Journal, MemJournal};

fn mem() -> Arc<dyn Journal> {
    MemJournal::new()
}

#[test]
fn in_process_emit_to_wakes_another_stream_once() {
    suite::emit_to_wakes_another_stream_once(&InProcessRig(mem));
}

#[test]
fn in_process_emit_to_is_shadowed_under_observe_and_runs_under_halt() {
    suite::emit_to_is_shadowed_under_observe_and_runs_under_halt(&InProcessRig(mem));
}
