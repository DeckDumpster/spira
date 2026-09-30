//! The heartbeat — `$SPIRA_RUN/auron.status`, written on EVERY run including a failing
//! one. A watchdog that stops without saying so is indistinguishable from a healthy
//! system; the ops pane reads this file's age and marks it stale rather than omitting it.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbStatus {
    Ok,
    Down,
    Saturated,
    Unknown,
}

impl DbStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DbStatus::Ok => "ok",
            DbStatus::Down => "down",
            DbStatus::Saturated => "saturated",
            DbStatus::Unknown => "?",
        }
    }
}

/// `db_read`: `ok` if reachable, else `saturated` if the read itself timed out, else `down`.
pub fn db_read_status(db_reachable: bool, saturated: bool) -> DbStatus {
    if db_reachable {
        DbStatus::Ok
    } else if saturated {
        DbStatus::Saturated
    } else {
        DbStatus::Down
    }
}

/// `db_write`: the probe result (`Some(true)`=ok, `Some(false)`=down, `None`=unprobed),
/// with a down reading promoted to `saturated` when the pass also saw a timeout — a
/// failed write and a timed-out write are the same probe outcome, but the cockpit renders
/// a different diagnosis for contention than for an actual broken write path.
pub fn db_write_status(write_ok: Option<bool>, saturated: bool) -> DbStatus {
    match write_ok {
        Some(true) => DbStatus::Ok,
        Some(false) if saturated => DbStatus::Saturated,
        Some(false) => DbStatus::Down,
        None => DbStatus::Unknown,
    }
}

pub struct Heartbeat<'a> {
    pub at: i64,
    pub firing_keys: &'a [String],
    pub db_read: DbStatus,
    pub db_write: DbStatus,
    pub db_saturated: bool,
    pub fallback_written: bool,
    pub acted: i64,
}

impl Heartbeat<'_> {
    pub fn render(&self) -> String {
        format!(
            "SP_AURON_AT={}\nSP_AURON_FIRING={}\nSP_AURON_KEYS='{}'\nSP_AURON_DB_READ={}\nSP_AURON_DB_WRITE={}\nSP_AURON_DB_SATURATED={}\nSP_AURON_FALLBACK={}\nSP_AURON_ACTED={}\n",
            self.at,
            self.firing_keys.len(),
            self.firing_keys.join(","),
            self.db_read.as_str(),
            self.db_write.as_str(),
            if self.db_saturated { 1 } else { 0 },
            if self.fallback_written { 1 } else { 0 },
            self.acted,
        )
    }
}

/// Write-then-rename, matching every other file this program writes.
pub fn write(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_read_ok_when_reachable() {
        assert_eq!(db_read_status(true, false), DbStatus::Ok);
    }

    #[test]
    fn db_read_saturated_outranks_down_when_the_read_itself_timed_out() {
        assert_eq!(db_read_status(false, true), DbStatus::Saturated);
    }

    #[test]
    fn db_read_down_otherwise() {
        assert_eq!(db_read_status(false, false), DbStatus::Down);
    }

    #[test]
    fn db_write_unknown_never_renders_as_ok() {
        assert_eq!(db_write_status(None, false), DbStatus::Unknown);
    }

    #[test]
    fn db_write_down_promoted_to_saturated_on_a_timeout() {
        assert_eq!(db_write_status(Some(false), true), DbStatus::Saturated);
    }

    #[test]
    fn db_write_plain_down_without_a_timeout() {
        assert_eq!(db_write_status(Some(false), false), DbStatus::Down);
    }

    #[test]
    fn render_shape() {
        let keys = vec!["a".to_string(), "b".to_string()];
        let h = Heartbeat { at: 100, firing_keys: &keys, db_read: DbStatus::Ok, db_write: DbStatus::Ok, db_saturated: false, fallback_written: false, acted: 2 };
        let text = h.render();
        assert!(text.contains("SP_AURON_AT=100"));
        assert!(text.contains("SP_AURON_FIRING=2"));
        assert!(text.contains("SP_AURON_KEYS='a,b'"));
        assert!(text.contains("SP_AURON_DB_READ=ok"));
        assert!(text.contains("SP_AURON_ACTED=2"));
    }

    #[test]
    fn render_with_nothing_firing_is_still_written() {
        let h = Heartbeat { at: 1, firing_keys: &[], db_read: DbStatus::Down, db_write: DbStatus::Unknown, db_saturated: false, fallback_written: true, acted: 0 };
        let text = h.render();
        assert!(text.contains("SP_AURON_FIRING=0"));
        assert!(text.contains("SP_AURON_KEYS=''"));
        assert!(text.contains("SP_AURON_DB_WRITE=?"));
        assert!(text.contains("SP_AURON_FALLBACK=1"));
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = testkit::TempDir::new("auron-heartbeat");
        let p = dir.join("auron.status");
        write(&p, "SP_AURON_AT=1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "SP_AURON_AT=1\n");
    }
}
