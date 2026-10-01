//! Path builders for the two files a reader latches onto (`<name>.log`, `<name>.cursor`)
//! and the three watchd owns for itself (`.tail.lock`, `.pending`, `.restarts`,
//! `.unhealthy`). Pure string formatting — DESIGN.md "Files under $SPIRA_RUN/watchd/".

use crate::manifest::Kind;
use std::path::{Path, PathBuf};

pub fn watchd_dir(run: &str) -> PathBuf {
    Path::new(run).join("watchd")
}

/// The log a reader for this row actually reads: the target itself for a `log` row, our own
/// `<name>.log` for everything else, and nothing for `off` (nothing is written for a watcher
/// this installation does not have).
pub fn logfile(run: &str, name: &str, kind: Kind, target: &str) -> Option<PathBuf> {
    match kind {
        Kind::Off => None,
        Kind::Log => Some(PathBuf::from(target)),
        Kind::Daemon | Kind::Extern => Some(watchd_dir(run).join(format!("{name}.log"))),
    }
}

pub fn cursorfile(run: &str, name: &str) -> PathBuf {
    watchd_dir(run).join(format!("{name}.cursor"))
}

pub fn tail_lockfile(run: &str, name: &str) -> PathBuf {
    watchd_dir(run).join(format!("{name}.tail.lock"))
}

pub fn restartfile(run: &str, name: &str) -> PathBuf {
    watchd_dir(run).join(format!("{name}.restarts"))
}

pub fn unhealthyfile(run: &str, name: &str) -> PathBuf {
    watchd_dir(run).join(format!("{name}.unhealthy"))
}

pub fn pendfile(run: &str, name: &str) -> PathBuf {
    watchd_dir(run).join(format!("{name}.pending"))
}

/// `watch_unit_name` — mirrors `inst_watch_name` in systemd/units.sh (one formula, two
/// callers; see conf.sh). `instance` is `SPIRA_INSTANCE`, defaulting to `prod`.
pub fn watch_unit_name(name: &str, instance: &str) -> String {
    format!("spira-watch-{name}-{instance}.service")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_row_points_at_its_own_target() {
        assert_eq!(logfile("/run", "mail-deliver", Kind::Log, "/var/log/x"), Some(PathBuf::from("/var/log/x")));
    }

    #[test]
    fn daemon_and_extern_rows_point_at_our_own_log() {
        assert_eq!(logfile("/run", "pool", Kind::Daemon, "pool.sh"), Some(PathBuf::from("/run/watchd/pool.log")));
        assert_eq!(logfile("/run", "mail-deliver", Kind::Extern, "mail-deliver"), Some(PathBuf::from("/run/watchd/mail-deliver.log")));
    }

    #[test]
    fn off_rows_have_no_log() {
        assert_eq!(logfile("/run", "view", Kind::Off, "@SPIRA_VIEW@"), None);
    }

    #[test]
    fn unit_name_is_instance_qualified() {
        assert_eq!(watch_unit_name("pool", "prod"), "spira-watch-pool-prod.service");
        assert_eq!(watch_unit_name("pool", "dev"), "spira-watch-pool-dev.service");
    }
}
