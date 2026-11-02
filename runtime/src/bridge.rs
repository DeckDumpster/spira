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
