//! A handler's `Emit` against a bead is a lifecycle event: the journal carries the library's
//! own `BeadEvent`, and a stream folds back to a row with the library's own fold.

use lifecycle::bead::{BeadEvent, BeadRow};

use crate::event::{Event, EventKind};
use crate::{Result, RuntimeError};

pub const BEAD_EVENT: &str = "bead.event";

pub fn bead_event_kind(ev: &BeadEvent) -> Result<EventKind> {
    let body = serde_json::to_string(ev).map_err(|e| RuntimeError::new(e.to_string()))?;
    Ok(EventKind::new(BEAD_EVENT, &body))
}

pub fn bead_event_from(kind: &EventKind) -> Result<BeadEvent> {
    if kind.tag != BEAD_EVENT {
        return Err(RuntimeError::new(format!("{} is not a {BEAD_EVENT}", kind.tag)));
    }
    serde_json::from_str(&kind.body).map_err(|e| RuntimeError::new(format!("bad bead event: {e}")))
}

/// The row a bead stream's events fold to; events of other kinds are skipped.
pub fn fold_bead_stream(bead_id: &str, events: &[Event]) -> Result<BeadRow> {
    let evs: Vec<BeadEvent> = events.iter().filter(|e| e.kind.tag == BEAD_EVENT).map(|e| bead_event_from(&e.kind)).collect::<Result<_>>()?;
    Ok(lifecycle::replay::fold_bead(bead_id, &evs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{CorrelationId, EventId, StreamKey};
    use lifecycle::bead::{BeadEventKind, BeadState};

    fn ev(i: usize, kind: EventKind) -> Event {
        Event { id: EventId(format!("e{i}")), stream: StreamKey("b".into()), kind, at: 0, cause: None, correlation: CorrelationId("c".into()) }
    }

    fn first_machine_event() -> BeadEvent {
        let filed = BeadRow::filed("b");
        let kind = BeadEventKind::Claim { holder: "h".into(), lease_until: 100, stack: Default::default(), stack_depth: 0, stack_max_depth: 3, persona: None };
        BeadEvent { expect: filed.state, version: filed.version, kind, actor: "t".into(), at: None }
    }

    #[test]
    fn a_bead_event_round_trips_through_the_journal_kind() {
        let be = first_machine_event();
        assert_eq!(bead_event_from(&bead_event_kind(&be).unwrap()).unwrap(), be);
        assert!(bead_event_from(&EventKind::new("other", "{}")).is_err());
    }

    #[test]
    fn a_stream_folds_with_the_library_fold() {
        let be = first_machine_event();
        let evs = vec![ev(0, bead_event_kind(&be).unwrap()), ev(1, EventKind::new("noise", ""))];
        let row = fold_bead_stream("b", &evs).unwrap();
        assert_eq!(row, lifecycle::replay::fold_bead("b", &[be]));
        assert_eq!(row.state, BeadState::Working);
    }
}
