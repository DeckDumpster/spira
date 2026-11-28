// covers: heimdall/src/pass.rs heimdall/src/broker.rs heimdall/src/heartbeat.rs
//! A down broker is a condition and one operator mail per outage, against a real broker.
#[macro_use]
extern crate spira_test_id;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use heimdall::classify::{Observation, Thresholds};
use heimdall::conditions::{Condition, Conditions, R};
use heimdall::pass::{self, PassInput, PassPaths, PassReport};
use heimdall::state::State;
use inbox_rules::Bus;
use serde_json::json;
use testkit::broker::Broker;

const KEY: &str = "broker-down";

/// A store holding what it was last told about each key.
#[derive(Default)]
struct Store {
    rows: Mutex<BTreeMap<String, (String, bool, String)>>,
    down: Mutex<bool>,
}

impl Store {
    fn firing(&self) -> bool {
        self.rows.lock().unwrap().get(KEY).is_some_and(|r| r.1)
    }
    fn message(&self) -> String {
        self.rows.lock().unwrap().get(KEY).map(|r| r.2.clone()).unwrap_or_default()
    }
    fn answers(&self) -> R<()> {
        if *self.down.lock().unwrap() {
            Err(heimdall::conditions::Failure::Other)
        } else {
            Ok(())
        }
    }
}

impl Conditions for Store {
    fn list(&self) -> R<Vec<Condition>> {
        self.answers()?;
        Ok(self.rows.lock().unwrap().iter().map(|(k, (kind, firing, _))| Condition { key: k.clone(), kind: kind.clone(), status: if *firing { "True" } else { "False" }.into(), flaps: 1 }).collect())
    }
    fn set(&self, key: &str, kind: &str, _: &str, message: &str, firing: bool) -> R<()> {
        self.answers()?;
        self.rows.lock().unwrap().insert(key.into(), (kind.into(), firing, message.into()));
        Ok(())
    }
    fn clear(&self, key: &str, _: &str, _: &str) -> R<()> {
        self.answers()?;
        if let Some(r) = self.rows.lock().unwrap().get_mut(key) {
            r.1 = false;
        }
        Ok(())
    }
}

struct Rig {
    dir: testkit::TempDir,
    bus: Bus,
    store: Store,
    state: State,
    now: i64,
    mail_root: PathBuf,
}

impl Rig {
    fn new(broker: &Broker) -> Rig {
        let dir = testkit::TempDir::new("heimdall-broker-down");
        let mail_root = dir.join("mail");
        Rig { bus: Bus::At(broker.socket().to_path_buf()), store: Store::default(), state: State::default(), now: 1_800_000_000, mail_root, dir }
    }

    /// The mail environment as the pass is handed it, with a statute registry that would
    /// refuse the page unless the pager says it has considered it.
    fn mail(&self) -> mail::env::Env {
        let root = self.dir.path();
        std::fs::create_dir_all(root.join("run")).unwrap();
        std::fs::write(root.join("guards"), "law-planted | refuse | broker\n").unwrap();
        heimdall::broker::pager_env(mail::env::Env {
            mail_root: self.mail_root.clone(),
            kinds_dir: testkit::manifest_dir().join("../spira/mail/kinds"),
            index_file: root.join("index"),
            mute: false,
            loom_budget_ms: 1000,
            repeat_window_s: 86_400,
            tidy_fresh_s: 0,
            id_prefix: "sp".into(),
            ask_label: "ask-label".into(),
            db: "a-database-the-page-must-not-need".into(),
            bd_conn_retries: 1,
            operator_actor: "operator".into(),
            run_dir: root.join("run"),
            home: root.to_path_buf(),
            mail_from: None,
            lint_considered: None,
            repeat_considered: None,
            operator_considered: None,
            statute_guards: root.join("guards").display().to_string(),
            statute_considered: None,
            allow_blocking: false,
            bead_id: None,
            lock_timeout_ms: 1000,
            condition: None,
        })
    }

    fn pass(&mut self) -> PassReport {
        self.now += 120;
        let obs = Observation {
            sentinel_log_readable: true,
            sentinel_timer: "active".into(),
            strands: json!({}),
            thresholds: Thresholds { pass_stale: 1_000_000, starve_passes: 5, mirror_stale: 90_000, ghost_stale: 1800, restart_threshold: 3, restart_window: 3600, red_streak: 4, noverdict_streak: 4 },
            ..Observation::default()
        };
        let inp = PassInput { obs, confirm: 2, clear_n: 2, refresh: 3600, tz: "UTC".into(), render_time: |e, _| format!("@{e}"), report_only: false };
        let paths = PassPaths {
            fallback: self.dir.join("fallback"),
            heartbeat: self.dir.join("heimdall.status"),
            inbox: self.dir.join("inbox.log"),
            inbox_cursor: self.dir.join("inbox.cursor"),
            inbox_bus: self.bus.clone(),
            operator_mail: Ok(self.mail()),
        };
        pass::run(&inp, &self.store, &mut self.state, &paths, self.now)
    }

    fn mails(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.mail_root.join("operator/new")) else { return Vec::new() };
        entries.map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap()).collect()
    }

    fn heartbeat(&self) -> String {
        std::fs::read_to_string(self.dir.join("heimdall.status")).unwrap()
    }
}

#[test_id(t0770en)]
#[test]
fn a_down_broker_pages_the_operator_once_per_outage_after_two_passes() {
    let mut broker = Broker::up();
    let mut r = Rig::new(&broker);

    r.pass();
    assert!(!r.store.firing() && r.mails().is_empty(), "broker up");

    broker.stop();
    let first_down = r.now + 120;
    r.pass();
    assert!(!r.store.firing() && r.mails().is_empty(), "one failed pass is not an outage");

    let rep = r.pass();
    assert!(r.store.firing(), "{:?}", rep.err);
    let mails = r.mails();
    assert_eq!(mails.len(), 1, "{:?}", rep.err);
    for needle in [
        "\nSubject: The broker is down: the Concierge cannot see watchers, asks or mail\n",
        "\nX-Spira-Kind: alert\n",
        "\nX-Spira-Urgent: yes\n",
        &format!("Down since: @{first_down}"),
        "The last connect error was: ",
    ] {
        assert!(mails[0].contains(needle), "{needle:?} missing from {}", mails[0]);
    }
    assert!(r.store.message().starts_with("The broker is down"), "{}", r.store.message());

    r.pass();
    assert_eq!(r.mails().len(), 1, "one mail for one outage");
    assert!(r.heartbeat().contains(&format!("SP_HEIMDALL_BROKER_DOWN_SINCE={first_down}\nSP_HEIMDALL_BROKER_PAGED=1\n")), "{}", r.heartbeat());

    broker.restart();
    r.pass();
    assert!(!r.store.firing(), "the first pass that reaches the broker clears it");
    assert!(r.heartbeat().contains("SP_HEIMDALL_BROKER_DOWN_SINCE=\nSP_HEIMDALL_BROKER_PAGED=0\n"), "{}", r.heartbeat());

    broker.stop();
    r.pass();
    r.pass();
    assert_eq!(r.mails().len(), 2, "a later outage mails again");
    assert!(r.store.firing());
}

#[test_id(tal7zjy)]
#[test]
fn with_the_store_also_down_the_operator_is_still_mailed_and_the_condition_follows_the_store() {
    let mut broker = Broker::up();
    let mut r = Rig::new(&broker);
    broker.stop();
    *r.store.down.lock().unwrap() = true;
    r.pass();
    r.pass();
    assert_eq!(r.mails().len(), 1);
    let fallback = std::fs::read_to_string(r.dir.join("fallback")).unwrap();
    assert!(fallback.contains(KEY), "{fallback}");

    *r.store.down.lock().unwrap() = false;
    r.pass();
    assert!(r.store.firing());
    assert_eq!(r.mails().len(), 1);

    *r.store.down.lock().unwrap() = true;
    broker.restart();
    r.pass();
    *r.store.down.lock().unwrap() = false;
    assert!(r.store.firing(), "positive control: the clear did not reach the store");
    r.pass();
    assert!(!r.store.firing(), "a clear the store missed is sent again");
}

#[test_id(t9t76g0)]
#[test]
fn a_mail_that_fails_is_logged_leaves_the_condition_standing_and_is_sent_by_a_later_pass() {
    let mut broker = Broker::up();
    let mut r = Rig::new(&broker);
    broker.stop();
    std::fs::write(&r.mail_root, "a file where the mail root goes").unwrap();
    r.pass();
    let rep = r.pass();
    assert!(r.store.firing());
    assert!(rep.err.iter().any(|l| l.contains("could not mail the operator")), "{:?}", rep.err);

    std::fs::remove_file(&r.mail_root).unwrap();
    r.pass();
    assert_eq!(r.mails().len(), 1);
}
