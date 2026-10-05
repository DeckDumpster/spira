pub fn landed(run: &std::path::Path, id: &str) -> bool {
    std::fs::read_to_string(run.join("landstate").join(id)).is_ok()
}
