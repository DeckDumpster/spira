//! The scheduler's real sources (design S9): what submits events to the host from outside it.
//! Each runs on its own thread under `runtime::sources::Sources::start`, and each is refused under
//! virtual time, where a `ScriptedSource` stands in. Declared in `tasks::g` (the module's event
//! shapes and contracts live beside the cockpit tiers in family G); the real reads live here.
//!
//! | Source | Backend | Module |
//! |---|---|---|
//! | `mail.arrived` | inotify on Linux, a directory listing elsewhere ([`watch`]) | [`mail`] |
//! | `unit.ran` | the unit manager's completions: systemd over D-Bus, or the host's `ProcessUnits` | [`units`] |
//! | `submission` | `spira-submission`'s SMTP session, delivering through the `mail` crate | [`smtp`] |

use std::path::PathBuf;
use std::sync::Arc;

use runtime::ports::Clock;
use runtime::sources::Sources;
use runtime::units::UnitCompletions;

use crate::tasks::g::mail_arrived::MailArrived;
use crate::tasks::g::submission::Submission;

pub mod mail;
pub mod smtp;
pub mod units;
pub mod watch;

/// The three real sources, from pinned config read once here: the registered mailboxes
/// (`SPIRA_MAIL_READERS` under `SPIRA_MAIL`), the unit manager's completions (`completions`, the
/// same instance the host's `unit.*` effects run on), and the submission listener (dormant when
/// `SPIRA_SUBMISSION_ADDR` or its certificate settings are empty).
pub fn real(cfg: &dyn Fn(&str) -> Result<String, String>, completions: Arc<dyn UnitCompletions>, clock: Arc<dyn Clock>) -> Result<Sources, String> {
    let mut s = Sources::default();
    let mailboxes = mail::mailboxes(&cfg("SPIRA_MAIL_READERS")?, &PathBuf::from(cfg("SPIRA_MAIL")?));
    s.register(Box::new(MailArrived { mailboxes })).map_err(|e| e.to_string())?;
    s.register(Box::new(units::over(completions, clock))).map_err(|e| e.to_string())?;
    let (addr, port) = match spira_submission::Config::from_values(cfg, vec![])? {
        Some(c) => (c.addr.to_string(), c.port),
        None => (String::new(), 0),
    };
    s.register(Box::new(Submission { addr, port })).map_err(|e| e.to_string())?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use runtime::units::ScriptedUnits;

    #[test]
    fn the_real_set_names_all_three_and_refuses_virtual_time() {
        let clock = Arc::new(VirtualClock::new(0));
        let cfg = |k: &str| -> Result<String, String> {
            Ok(match k {
                "SPIRA_MAIL_READERS" => "concierge=inbox-append.sh".into(),
                "SPIRA_MAIL" => "/m".into(),
                _ => String::new(),
            })
        };
        let s = real(&cfg, Arc::new(ScriptedUnits::new(clock.clone())), Arc::new(runtime::ports::WallClock)).unwrap();
        assert_eq!(s.names(), ["mail.arrived", "unit.ran", "submission"]);
        let err = s.into_scripted().err().expect("real sources are refused under virtual time");
        assert!(err.0.contains("mail.arrived") && err.0.contains("unit.ran") && err.0.contains("submission"), "{err}");
    }

    #[test]
    fn an_unresolvable_key_refuses_the_set_naming_it() {
        let cfg = |k: &str| -> Result<String, String> { Err(format!("{k} is not set")) };
        let err = real(&cfg, Arc::new(ScriptedUnits::new(Arc::new(VirtualClock::new(0)))), Arc::new(runtime::ports::WallClock)).err().unwrap();
        assert!(err.contains("SPIRA_MAIL_READERS"), "{err}");
    }
}
