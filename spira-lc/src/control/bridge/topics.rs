//! The four topic families of `spira/topics.toml` the bridge publishes, and nothing else:
//! `two-plane` exempts this file's topic names, so no record key belongs here.

pub(super) fn state(id: &str) -> String {
    format!("bead/{id}/state")
}

pub(super) fn sessions(id: &str) -> String {
    format!("bead/{id}/sessions")
}

pub(super) const INCIDENTS: &str = "incident/";

pub(super) fn incident(seg: &str) -> String {
    format!("{INCIDENTS}{seg}")
}

pub(super) const ASKS: &str = "ask/";

pub(super) fn ask(id: &str) -> String {
    format!("{ASKS}{id}")
}
