//! The scheduler's real sources (design S9): what submits events to the host from outside it.
//! Each runs as a future under `runtime::sources::Sources::start`, none owns a thread, and each
//! is refused under virtual time, where a `ScriptedSource` stands in. The event shapes and contracts live beside
//! the cockpit tiers in family G (`tasks::g`, which re-exports this module); the real reads
//! live here.
//!
//! | Source | Backend | Module |
//! |---|---|---|
//! | `mail.arrived` | inotify on Linux, a directory listing elsewhere ([`watch`]) | [`mail`] |
//! | `unit.ran` | the unit manager's completions: systemd over D-Bus, or the host's `ProcessUnits` | [`units`] |
//! | `submission` | `spira-submission`'s SMTP listener and sessions as tasks on the loop, delivering through the `mail` crate | [`smtp`] |
//! | `store.changed` | the store's `watch` feed, a socket on the scheduler's loop that keeps the in-process store views current | [`store_changed`] |
//! | `margin.changed` | a long-poll held open on the Margin reader's change feed; dormant while `SPIRA_MARGIN_URL` is empty | [`margin_changed`] |

use std::path::PathBuf;
use std::sync::Arc;

use runtime::ports::Clock;
use runtime::sources::Sources;
use runtime::units::UnitCompletions;

use crate::tasks::g::mail_arrived::MailArrived;
use crate::tasks::g::submission::Submission;

pub mod mail;
pub mod margin_changed;
pub mod smtp;
pub mod store_changed;
pub mod units;
pub mod watch;

/// Every config key [`real`] reads, so `check-config` (through `Settings::keys`) checks them.
pub const KEYS: &[&str] = &[
    "SPIRA_MAIL_READERS",
    "SPIRA_MAIL",
    "SPIRA_SUBMISSION_ADDR",
    "SPIRA_SUBMISSION_PORT",
    "SPIRA_SUBMISSION_CERT",
    "SPIRA_SUBMISSION_KEY",
    "SPIRA_SUBMISSION_PASSWORD_FILE",
    "SPIRA_SUBMISSION_USER",
    "SPIRA_SUBMISSION_FROM",
    "SPIRA_SUBMISSION_RCPT_DOMAIN",
    "SPIRA_RUN",
    "SPIRA_MARGIN_URL",
    "SPIRA_MARGIN_TOKEN_FILE",
];

/// The real sources, from pinned config read once here: the registered mailboxes
/// (`SPIRA_MAIL_READERS` under `SPIRA_MAIL`), the unit manager's completions (`completions`, the
/// same instance the host's `unit.*` effects run on), and the submission listener (dormant when
/// `SPIRA_SUBMISSION_ADDR` or its certificate settings are empty), and, when the store views
/// were installed at boot, the watch that keeps them current (`store.changed`).
pub fn real(cfg: &dyn Fn(&str) -> Result<String, String>, completions: Arc<dyn UnitCompletions>, clock: Arc<dyn Clock>, store: Option<crate::StoreViews>) -> Result<Sources, String> {
    let mut s = Sources::default();
    let mailboxes = mail::mailboxes(&cfg("SPIRA_MAIL_READERS")?, &PathBuf::from(cfg("SPIRA_MAIL")?));
    s.register(Box::new(MailArrived { mailboxes })).map_err(|e| e.to_string())?;
    s.register(Box::new(units::over(completions, clock))).map_err(|e| e.to_string())?;
    let (addr, port) = match spira_submission::Config::from_values(cfg, vec![])? {
        Some(c) => (c.addr.to_string(), c.port),
        None => (String::new(), 0),
    };
    s.register(Box::new(Submission { addr, port })).map_err(|e| e.to_string())?;
    if let Some((views, table, socket)) = store {
        s.register(Box::new(store_changed::StoreChanged { views, table, socket })).map_err(|e| e.to_string())?;
    }
    s.register(Box::new(margin_changed::MarginChanged::configured(cfg)?)).map_err(|e| e.to_string())?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use runtime::units::ScriptedUnits;

    #[test_id(tfqz0sr)]
    #[test]
    fn the_real_set_names_all_five_with_store_views_and_four_without_and_refuses_virtual_time() {
        let clock = Arc::new(VirtualClock::new(0));
        let cfg = |k: &str| -> Result<String, String> {
            Ok(match k {
                "SPIRA_MAIL_READERS" => "concierge=inbox-append.sh".into(),
                "SPIRA_MAIL" => "m".into(),
                _ => String::new(),
            })
        };
        let units = || Arc::new(ScriptedUnits::new(clock.clone()));
        let s = real(&cfg, units(), Arc::new(runtime::ports::WallClock), None).unwrap();
        assert_eq!(s.names(), ["mail.arrived", "unit.ran", "submission", "margin.changed"]);
        let views = Arc::new(store_view::Projection::new(spira_store::lc_call::LcClient::socket("no-such-socket"), std::time::Duration::from_secs(1), Arc::new(std::time::Instant::now)));
        let s = real(&cfg, units(), Arc::new(runtime::ports::WallClock), Some((views, Arc::new(crate::store::ChangeTable::default()), "no-such-socket".into()))).unwrap();
        assert_eq!(s.names(), ["mail.arrived", "unit.ran", "submission", "store.changed", "margin.changed"]);
        let err = s.into_scripted().err().expect("real sources are refused under virtual time");
        assert!(["mail.arrived", "unit.ran", "submission", "store.changed", "margin.changed"].iter().all(|n| err.0.contains(n)), "{err}");
    }

    #[test_id(t9p33pf)]
    #[test]
    fn an_unresolvable_key_refuses_the_set_naming_it() {
        let cfg = |k: &str| -> Result<String, String> { Err(format!("{k} is not set")) };
        let err = real(&cfg, Arc::new(ScriptedUnits::new(Arc::new(VirtualClock::new(0)))), Arc::new(runtime::ports::WallClock), None).err().unwrap();
        assert!(err.contains("SPIRA_MAIL_READERS"), "{err}");
    }
}
