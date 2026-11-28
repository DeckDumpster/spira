//! The broker check. A broker that fails two passes running is the `broker-down` condition and
//! one operator mail for that outage. Neither goes through the broker.

use std::sync::mpsc;
use std::time::Duration;

pub use inbox_rules::Bus;
use serde::{Deserialize, Serialize};
use spira_pub::PubError;

use crate::alerts;
use crate::conditions::Conditions;

pub const KEY: &str = "broker-down";
pub const TITLE: &str = "The broker is down: the Concierge cannot see watchers, asks or mail";
/// Not `heimdall`: this publisher comes and goes each pass, and its last will must not mark the
/// resident one gone.
const PUBLISHER: &str = "heimdall-check";
/// Well inside the unit's `TimeoutStartSec`.
pub const MAIL_DEADLINE: Duration = Duration::from_secs(10);

/// What one connect found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reading {
    /// There is no broker to judge, so the outage state stands.
    #[default]
    Unchecked,
    Up,
    Down(String),
}

/// One connect, bounded by [`spira_pub::CONNECT_DEADLINE`].
pub fn check(bus: &Bus) -> Reading {
    if matches!(bus, Bus::Off) {
        return Reading::Unchecked;
    }
    match bus.publisher(PUBLISHER).and_then(|p| p.connect()) {
        Ok(()) => Reading::Up,
        Err(e @ (PubError::Unreachable { .. } | PubError::Timeout)) => Reading::Down(e.to_string()),
        Err(e) => {
            eprintln!("heimdall: the broker was not checked: {e}");
            Reading::Unchecked
        }
    }
}

/// What a pass carries to the next.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Outage {
    pub down_since: Option<i64>,
    pub paged: bool,
    /// The condition was written and no clear has reached the store since.
    pub raised: bool,
}

/// What a pass owes the store and the operator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Todo {
    Raise { since: i64, why: String },
    Page { since: i64, why: String },
    Clear,
}

/// The outage after a pass that read `reading`. `row_firing` is the store's own word that the
/// condition stands, which clears it even when the carry was lost.
pub fn step(before: &Outage, reading: &Reading, row_firing: bool, now: i64) -> (Outage, Vec<Todo>) {
    match reading {
        Reading::Unchecked => (before.clone(), Vec::new()),
        Reading::Up => {
            let todos = if before.raised || row_firing { vec![Todo::Clear] } else { Vec::new() };
            (Outage { down_since: None, paged: false, raised: before.raised }, todos)
        }
        Reading::Down(why) => {
            let Some(since) = before.down_since else {
                return (Outage { down_since: Some(now), paged: false, raised: false }, Vec::new());
            };
            let mut todos = Vec::new();
            if !before.raised {
                todos.push(Todo::Raise { since, why: why.clone() });
            }
            if !before.paged {
                todos.push(Todo::Page { since, why: why.clone() });
            }
            (before.clone(), todos)
        }
    }
}

/// Which of the todos got through; the rest are owed again next pass.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Done {
    /// The outage a raise or page was for.
    pub since: Option<i64>,
    pub raised: bool,
    pub paged: bool,
    pub cleared: bool,
    pub errors: Vec<String>,
}

impl Outage {
    pub fn apply(&mut self, done: &Done) {
        if done.cleared && self.down_since.is_none() {
            self.raised = false;
        }
        if self.down_since.is_some() && self.down_since == done.since {
            self.raised |= done.raised;
            self.paged |= done.paged;
        }
    }
}

/// Sends the page: `(subject, body)`.
pub type Pager<'a> = &'a dyn Fn(&str, String) -> Result<(), String>;

pub fn perform(todos: &[Todo], ops: &dyn Conditions, pager: Pager, render_time: &dyn Fn(i64) -> String) -> Done {
    let mut done = Done::default();
    for todo in todos {
        match todo {
            Todo::Raise { since, why } => {
                done.since = Some(*since);
                match alerts::alert_write(ops, KEY, TITLE, &evidence(&render_time(*since), why)) {
                    Ok(_) => done.raised = true,
                    Err(_) => done.errors.push(format!("could not raise the alert condition for {KEY}")),
                }
            }
            Todo::Page { since, why } => {
                done.since = Some(*since);
                match pager(TITLE, mail_body(&render_time(*since), why)) {
                    Ok(()) => done.paged = true,
                    Err(e) => done.errors.push(format!("could not mail the operator about the broker: {e}")),
                }
            }
            Todo::Clear => match ops.clear(KEY, "Cleared", "The broker answered heimdall's connect.") {
                Ok(()) => done.cleared = true,
                Err(_) => done.errors.push(format!("could not clear the alert condition for {KEY}")),
            },
        }
    }
    done
}

/// The condition's evidence, and the fallback file's.
pub fn evidence(down_since: &str, why: &str) -> String {
    format!("DOWN SINCE {down_since}\nLAST ERROR {why}")
}

fn mail_body(down_since: &str, why: &str) -> String {
    format!(
        "## Alert\nThe broker has not answered heimdall on two passes running.\n\nDown since: {down_since}\nThe last connect error was: {why}\n\n## Why it is urgent\nThe Concierge reads watchers, asks and mail from the broker, so it sees none of them until the broker answers.\n"
    )
}

/// `base` as the page is sent: no bd, since the page must not wait on a store. It is an alert
/// because the operator's door sends a `note` on to the Concierge, who cannot see it.
pub fn pager_env(mut env: mail::env::Env) -> mail::env::Env {
    const WHY: &str = "heimdall's broker-down page: fixed text, sent once per outage";
    env.db.clear();
    env.operator_considered = Some(WHY.into());
    // The outage carry dedupes; a second outage has the same subject and must still send.
    env.repeat_considered = Some(WHY.into());
    env.statute_considered = Some(WHY.into());
    env
}

/// Sends the page under `env` (see [`pager_env`]), waiting no longer than [`MAIL_DEADLINE`].
pub fn send_page(env: Result<mail::env::Env, String>, subject: &str, body: String) -> Result<(), String> {
    let env = env?;
    let subject = subject.to_string();
    within(MAIL_DEADLINE, move || {
        let args = mail::cmds::SendArgs { mailbox: "operator", from: Some("Heimdall <heimdall@spira>"), subject: &subject, kind: "alert", default: "", class: "", bead: "", urgent: true, digest: false, dry_run: false, requested_by: "" };
        mail::cmds::send(&mail::bead::BdCli { conn_retries: 1 }, &env, &args, body).map(drop)
    })
}

fn within(deadline: Duration, send: impl FnOnce() -> Result<(), String> + Send + 'static) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("heimdall-page".into())
        .spawn(move || drop(tx.send(send())))
        .map_err(|e| format!("cannot start the send: {e}"))?;
    rx.recv_timeout(deadline).unwrap_or_else(|_| Err(format!("the send did not finish within {deadline:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tyaa5um)]
    #[test]
    fn a_send_that_outlives_its_deadline_is_a_failure() {
        let (release, held) = mpsc::channel::<()>();
        let late = within(Duration::ZERO, move || {
            let _ = held.recv();
            Ok(())
        });
        assert!(late.unwrap_err().contains("did not finish"));
        drop(release);
        assert_eq!(within(Duration::from_secs(30), || Ok(())), Ok(()));
    }

    #[test_id(taaaazg)]
    #[test]
    fn a_late_outcome_marks_only_the_outage_it_was_for() {
        let done = Done { since: Some(10), raised: true, paged: true, ..Done::default() };
        let mut later = Outage { down_since: Some(500), ..Outage::default() };
        later.apply(&done);
        assert_eq!(later, Outage { down_since: Some(500), ..Outage::default() });
        let mut same = Outage { down_since: Some(10), ..Outage::default() };
        same.apply(&done);
        assert!(same.paged && same.raised);
    }

    #[test_id(tsgdedp)]
    #[test]
    fn an_unchecked_pass_leaves_an_outage_standing_and_a_lost_carry_still_clears_a_firing_row() {
        let down = Outage { down_since: Some(10), paged: true, raised: true };
        assert_eq!(step(&down, &Reading::Unchecked, true, 99), (down.clone(), vec![]));
        assert_eq!(step(&Outage::default(), &Reading::Up, true, 99).1, vec![Todo::Clear]);
        assert_eq!(step(&Outage::default(), &Reading::Up, false, 99).1, vec![]);
    }
}
