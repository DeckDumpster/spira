// covers: spira-runtime/src/tasks/b/mail_reconcile.rs spira-runtime/src/watch_topics.rs
//! Unread reader mail on `mail/<mailbox>/<id>` on a real broker: `mail.reconcile` on the task
//! rig over a real maildir, its observe and publish run by the service's own effect runner.
#[macro_use]
extern crate spira_test_id;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::event::EffectSpec;
use runtime::host::EffectRunner;
use runtime::ports::{SeededRng, VirtualClock};
use runtime::taskrig::TaskRig;
use serde_json::{json, Value};
use spira_pub::{Options, Publisher};
use spira_runtime::tasks::b::kit::testing as t;
use spira_runtime::tasks::b::mail_reconcile::{self as task, MailReconcile};
use spira_runtime::tasks::g::mail_arrived::{stream, Arrived};
use spira_runtime::watch_topics::WatchTopics;
use spira_runtime::Effects;
use testkit::broker::{Broker, Subscriber};
use testkit::TempDir;

/// Bounds each wait on the broker so a broken one fails the test instead of hanging.
const FAILSAFE: Duration = Duration::from_secs(30);
const CADENCE_MS: u64 = 60_000;
const PUBLISH: &str = "mail.publish";
const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "60"), ("SPIRA_MAIL_READERS", "concierge=inbox-append.sh")];

/// The service's effect runner on the maildir under `dir`, publishing to `broker`.
fn service(dir: &Path, broker: &Broker) -> Arc<Effects> {
    let world = spira_runtime::world::World { mail: dir.join("mail"), run_dir: dir.join("run"), ..Default::default() };
    let fx = Effects { world, ..Default::default() };
    let options = Options::new(broker.socket(), Arc::new(VirtualClock::new(0)), Box::new(SeededRng::new(1)));
    assert!(fx.watch_topics.set(Some(WatchTopics::new(Publisher::with("spira-runtime", options).unwrap()))).is_ok());
    Arc::new(fx)
}

struct Fixture {
    rig: TaskRig,
    fx: Arc<Mutex<Arc<Effects>>>,
    dir: TempDir,
}

impl Fixture {
    fn up(broker: &Broker) -> Fixture {
        let dir = TempDir::new("mail-topics");
        let fx = Arc::new(Mutex::new(service(dir.path(), broker)));
        let runner = |name: &'static str| {
            let fx = fx.clone();
            move |input: &str| fx.lock().unwrap().run("k", &EffectSpec::keyed(name, "k", input, 10_000))
        };
        let mut rig = TaskRig::new(1, t::handler::<MailReconcile>(SET).registration()).unwrap().double(task::OBSERVE, runner(task::OBSERVE)).double(PUBLISH, runner(PUBLISH));
        let (_, boot_stream, kind) = MailReconcile::boot();
        rig.tick(boot_stream, kind).unwrap();
        Fixture { rig, fx, dir }
    }

    fn mailbox(&self, name: &str) -> PathBuf {
        self.dir.path().join("mail").join(name)
    }

    fn deliver(&self, mailbox: &str, id: &str, subject: &str, mute: bool) {
        let dir = self.mailbox(mailbox);
        mail::maildir::mail_ensure(&dir).unwrap();
        std::fs::write(dir.join("tmp").join(id), format!("From: Warden <warden@spira>\nSubject: {subject}\nX-Spira-Kind: note\n\nbody\n")).unwrap();
        mail::maildir::mail_deliver(&dir, id, mute).unwrap();
    }

    /// The arrival the real source submits for `mailbox`; no time passes.
    fn arrived(&mut self, mailbox: &str) {
        let arrived = Arrived { mailbox: mailbox.into(), kind_event: false };
        self.rig.deliver(&stream(mailbox), arrived.event_kind()).unwrap();
        self.flush();
    }

    fn tick(&mut self) {
        self.rig.advance(CADENCE_MS).unwrap();
        self.flush();
    }

    fn flush(&self) {
        if let Some(Some(topics)) = self.fx.lock().unwrap().watch_topics.get() {
            topics.flush(FAILSAFE).unwrap();
        }
    }

    fn read(&self, mailbox: &str, id: &str) {
        let root = self.dir.path().join("mail").display().to_string();
        let source = |key: &str| {
            Ok(match key {
                "SPIRA_MAIL" => root.clone(),
                "SPIRA_LOOM_BUDGET_MS" | "SPIRA_MAIL_REPEAT_WINDOW" | "SPIRA_MAIL_TIDY_FRESH" => "1".into(),
                _ => String::new(),
            })
        };
        let env = mail::env::Env::from_config(self.dir.path().to_path_buf(), &source).unwrap();
        mail::cmds::read_cmd(&env, mailbox, Some(id)).unwrap();
    }
}

fn held(broker: &Broker, filter: &str) -> BTreeMap<String, Value> {
    broker.retained(filter).into_iter().map(|(topic, payload)| (topic, serde_json::from_str(&payload).unwrap_or_else(|e| panic!("{payload:?}: {e}")))).collect()
}

fn topics(values: &BTreeMap<String, Value>) -> Vec<&str> {
    values.keys().map(String::as_str).collect()
}

#[test_id(tvwzt5o)]
#[test]
fn a_delivered_message_is_retained_on_its_arrival_and_cleared_once_read() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let mut f = Fixture::up(&broker);
    f.deliver("concierge", "100.1.1", "gate is red", false);
    f.arrived("concierge");
    let got = held(&broker, "mail/#");
    assert_eq!(topics(&got), ["mail/concierge/100.1.1"]);
    let value = &got["mail/concierge/100.1.1"];
    let at = mail::maildir::mtime_secs(&f.mailbox("concierge").join("new/100.1.1"));
    assert_eq!(value, &json!({"from": "Warden <warden@spira>", "subject": "gate is red", "kind": "note", "at": at}));

    f.read("concierge", "100.1.1");
    f.tick();
    assert_eq!(held(&broker, "mail/#"), BTreeMap::new());
}

#[test_id(typtrj4)]
#[test]
fn a_removed_message_is_cleared() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let mut f = Fixture::up(&broker);
    f.deliver("concierge", "100.1.1", "one", false);
    f.arrived("concierge");
    assert_eq!(held(&broker, "mail/#").len(), 1);
    std::fs::remove_file(f.mailbox("concierge").join("new/100.1.1")).unwrap();
    f.tick();
    assert_eq!(held(&broker, "mail/#"), BTreeMap::new());
}

/// Restarts the broker, which keeps nothing, and requires `before` to come back.
fn comes_back(broker: &mut Broker, before: &BTreeMap<String, Value>) {
    broker.restart();
    // Waits for the reconnect's republish: retained or live, each value is delivered.
    let mut watching = Subscriber::to(broker.socket(), &["mail/#"]);
    let mut back = BTreeMap::new();
    while back.len() < before.len() {
        let d = watching.recv();
        back.insert(d.topic, serde_json::from_slice::<Value>(&d.payload).unwrap());
    }
    assert_eq!(&back, before);
}

#[test_id(tk5k7ny)]
#[test]
fn a_restarted_broker_is_given_every_unread_message_again() {
    let _env = testkit::env_read();
    let mut broker = Broker::up();
    let mut f = Fixture::up(&broker);
    for id in ["100.1.1", "100.2.1", "100.3.1", "100.4.1"] {
        f.deliver("concierge", id, id, false);
    }
    f.arrived("concierge");
    f.read("concierge", "100.4.1");
    f.tick();
    let before = held(&broker, "mail/#");
    assert_eq!(topics(&before), ["mail/concierge/100.1.1", "mail/concierge/100.2.1", "mail/concierge/100.3.1"]);
    comes_back(&mut broker, &before);
    f.flush();
    assert_eq!(held(&broker, "mail/#"), before);
}

#[test_id(t58xn4r)]
#[test]
fn a_restarted_runtime_says_its_carried_mail_again_to_a_broker_that_kept_nothing() {
    let _env = testkit::env_read();
    let mut broker = Broker::up();
    let mut f = Fixture::up(&broker);
    f.deliver("concierge", "100.1.1", "one", false);
    f.arrived("concierge");
    let before = held(&broker, "mail/#");
    assert_eq!(before.len(), 1);

    *f.fx.lock().unwrap() = service(f.dir.path(), &broker);
    broker.restart();
    let mut watching = Subscriber::to(broker.socket(), &["mail/#"]);
    f.rig.reopen(t::handler::<MailReconcile>(SET).registration()).unwrap();
    let (_, boot_stream, kind) = MailReconcile::boot();
    f.rig.tick(boot_stream, kind).unwrap();
    f.flush();
    let d = watching.recv();
    assert_eq!(BTreeMap::from([(d.topic, serde_json::from_slice::<Value>(&d.payload).unwrap())]), before);
}

#[test_id(t1iy7hm)]
#[test]
fn operator_mail_and_muted_mail_are_never_published() {
    let _env = testkit::env_read();
    let broker = Broker::up();
    let mut f = Fixture::up(&broker);
    f.deliver("operator", "100.1.1", "for the operator", false);
    f.deliver("concierge", "100.2.1", "muted", true);
    // The positive control: a message that is published, on the same passes.
    f.deliver("concierge", "100.3.1", "loud", false);
    f.arrived("operator");
    f.arrived("concierge");
    f.tick();
    assert_eq!(topics(&held(&broker, "mail/#")), ["mail/concierge/100.3.1"]);
}
