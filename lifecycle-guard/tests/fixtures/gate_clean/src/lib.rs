// landed() used to answer this from commit subjects; the machine does now.
pub fn report(id: &str) -> String {
    format!("{id} never landed (gate-red) — see spira-lc state")
}
