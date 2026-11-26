//! Where `spira-sub` keeps each subscriber's control socket and subscription set.

use std::path::Path;

pub const CONTROL_DIR_KEY: &str = "SPIRA_SUB_CONTROL_DIR";

/// [`CONTROL_DIR_KEY`] as read through `cfg`; a relative value is a path under `run`. Joined
/// here for the reason [`crate::broker::socket_path`] is.
pub fn control_dir(run: &str, cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<String, String> {
    let value = cfg(CONTROL_DIR_KEY)?;
    if value.is_empty() {
        return Err(format!("{CONTROL_DIR_KEY} is empty: spira-sub has nowhere to keep its socket"));
    }
    Ok(Path::new(run).join(value).to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(telwkc9)]
    #[test]
    fn a_relative_control_dir_is_under_the_run_dir_and_none_is_defaulted() {
        let of = |v: &'static str| control_dir("h/run", &move |_| Ok(v.to_string()));
        assert_eq!(of("elsewhere/sub").unwrap(), "h/run/elsewhere/sub");
        assert!(of("").unwrap_err().contains(CONTROL_DIR_KEY));
        let undeclared = control_dir("h/run", &|k| Err(format!("{k} is not declared")));
        assert_eq!(undeclared.unwrap_err(), "SPIRA_SUB_CONTROL_DIR is not declared");
    }
}
