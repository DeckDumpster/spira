use super::*;

pub fn is_red(run: &Path, id: &str) -> bool {
    land_state(run, id).as_deref() == Some("RED")
}
