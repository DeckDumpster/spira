mod read;

pub fn land_state(run: &Path, id: &str) -> Option<String> {
    std::fs::read_to_string(run.join(id)).ok()
}
