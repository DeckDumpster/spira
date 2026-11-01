mod tests;

pub enum LoopEvent {
    Landed { bead: String, repo: String, at: u64 },
}

pub fn land_state(e: &LoopEvent) -> &str {
    match e {
        LoopEvent::Landed { bead, .. } => bead,
    }
}
