//! The operational control plane, as watchtower consults it: a suspended subject is declined,
//! not a fault. Unreadable config or control file reads as "not suspended" — a broken control
//! plane must not silence the watcher.

use spira_ctrl::CtrlData;

fn load() -> Option<CtrlData> {
    let path = spira_config::process::cfg("SPIRA_CTRL").ok()?;
    spira_ctrl::read(std::path::Path::new(&path)).ok()
}

/// True when `cause` (a condition name) is suspended in the control plane.
pub fn condition_suspended(cause: &str) -> bool {
    load().is_some_and(|d| spira_ctrl::is_suspended(&d, cause))
}

/// `units` without the ones the control plane has suspended.
pub fn without_suspended(units: Vec<String>) -> Vec<String> {
    let Some(data) = load() else { return units };
    let Ok(inst) = spira_config::process::cfg("SPIRA_INSTANCE") else { return units };
    units.into_iter().filter(|u| !spira_ctrl::unit_suspended(&data, u, &inst)).collect()
}
