//! `Command::EmitTo` on both rigs: one task wakes another stream's handler exactly once,
//! across a crash and a full replay, shadowed under observe and running under a halt.

use std::sync::Arc;

use runtime::conformance::{self as suite, InProcessRig};
use runtime::journal::{Journal, MemJournal};
use runtime::lease::LeaseRig;

fn mem() -> Arc<dyn Journal> {
    MemJournal::new()
}

#[test]
fn in_process_emit_to_wakes_another_stream_once_across_a_crash() {
    suite::emit_to_wakes_another_stream_once_across_a_crash(&InProcessRig(mem));
}

#[test]
fn in_process_emit_to_is_shadowed_under_observe_and_runs_under_halt() {
    suite::emit_to_is_shadowed_under_observe_and_runs_under_halt(&InProcessRig(mem));
}

#[test]
fn lease_emit_to_wakes_another_stream_once_across_a_crash() {
    suite::emit_to_wakes_another_stream_once_across_a_crash(&LeaseRig::new(mem));
}

#[test]
fn lease_emit_to_is_shadowed_under_observe_and_runs_under_halt() {
    suite::emit_to_is_shadowed_under_observe_and_runs_under_halt(&LeaseRig::new(mem));
}
