use super::*;

fn landed(bead: &str, repo: &str, at: u64) -> LoopEvent {
    LoopEvent::Landed { bead: bead.into(), repo: repo.into(), at }
}

fn spread(n: usize, repo: &str, end: u64) -> Vec<LoopEvent> {
    (0..n).map(|i| landed(&format!("{repo}-{i}"), repo, end - i as u64)).collect()
}

#[test]
fn a_landing_names_its_bead() {
    let mut events = spread(3, "harness", 100);
    events.push(landed("w2", "harness", 4));
    assert_eq!(land_state(&events[3]), "w2");
}
