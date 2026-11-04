//! What every family D and E acceptance test (`tests/task_d_*.rs`, `tests/task_e_*.rs`) asks of
//! the service, in one place: the task's row, settings pinned from the complete fixture, the
//! stream's health on the rig, and the static no-spawn scan of one module.

use std::path::{Path, PathBuf};

use runtime::taskrig::TaskRig;
use runtime::{Journal, StreamKey};

use crate::tasks::{self, Task};
use crate::Settings;

/// The row named `name` in `tasks::all()`; panics naming it when the row is missing, which is T1's
/// failure when a registration is removed.
pub fn row(name: &str) -> &'static Task {
    tasks::all().find(|t| t.name == name).unwrap_or_else(|| panic!("no task row named {name} in tasks::all()"))
}

/// Settings loaded through the one config door from the complete fixture (`spira/testlib`), with
/// `declare` on top. Sets `SPIRA_TOML` for this process; call it before anything reads config.
pub fn pinned_settings(declare: &[(&str, &str)]) -> Result<Settings, String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("task-acceptance-{nanos}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Every row needs a mode, and the complete fixture predates the family rows.
    let modes = tasks::all().map(|t| format!("{}=act", t.name)).collect::<Vec<_>>().join(" ");
    let service: &[(&str, &str)] = &[("SPIRA_LC_SOCKET", "/fixture/run/lc.sock"), ("SPIRA_TASK_MODES", &modes)];
    let toml = spira_config::process::fixture_toml(&dir, &[service, declare].concat());
    std::env::set_var(spira_config::process::TOML_ENV, &toml);
    std::env::set_var("SPIRA_HOME", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira"));
    Settings::load()
}

/// Panics with the quarantine message when the rig's handler poisoned `stream`. A handler that
/// panicked leaves its stream silent, so a silence or determinism check passes vacuously without
/// this; with it, a stub's `unimplemented!` text is the failure.
pub fn assert_healthy(rig: &TaskRig, stream: &str) {
    if let Some(p) = rig.journal.poisoned(&StreamKey(stream.into())).expect("read poison") {
        panic!("{}", p.message);
    }
}

/// The source of the module at `rel` under `src/tasks/`.
pub fn module_source(rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tasks").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The process launchers `tests/no_spawn.rs` scans for, the first `text` names outside its
/// `#[cfg(test)]` tail.
pub fn launcher(text: &str) -> Option<&'static str> {
    // Spelled in pieces so this file does not name them itself.
    const LAUNCHERS: &[&str] = &[concat!("Command", "::new"), concat!("std::", "process"), concat!("process::", "Command"), concat!("libc::", "fork"), concat!("libc::", "exec"), concat!("posix", "_spawn"), concat!("nix::", "unistd")];
    let code = text.split("#[cfg(test)]").next().unwrap_or_default();
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}
