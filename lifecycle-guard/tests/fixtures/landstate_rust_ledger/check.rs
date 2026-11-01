use landing_pass::landstate::landed;

fn already_in(run: &Path, id: &str) -> bool {
    if landed(run, id) {
        return true;
    }
    landing_pass::landstate::landed_sha(run, id).is_some()
}
