//! Source `mail.arrived` (`wiki/projects/spira/designs/tasks/source-mail-arrived.md`) on the
//! VirtualDriver: a `ScriptedSource` standing in submits the declared event shape and a stub
//! consumer task hears it on the mailbox's stream at its virtual time; the real source is refused
//! under virtual time. Red until the port lands: the real source's `run` is `unimplemented!`.

use std::sync::Arc;

use runtime::conformance::trace;
use runtime::host::{EffectRunner, Submit, VirtualDriver};
use runtime::journal::Journal;
use runtime::ports::{Clock, VirtualClock};
use runtime::sources::{ScriptedSource, Source, Sources};
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, Projection, Registration};
use spira_runtime::tasks::g::mail_arrived::*;
use tokio::sync::mpsc;

/// The `mail.wake` handler's stand-in: records each arrival it parses, with its stream and time.
struct Consumer;
impl Handler for Consumer {
    fn interests(&self) -> &[EventKindTag] {
        &[ARRIVED]
    }
    fn handle(&self, ctx: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        match Arrived::from_event_kind(&ev.kind) {
            Some(a) => vec![Command::Emit(EventKind::new("consumed", &format!("{}|{}|{}@{}", ev.stream.0, a.mailbox, a.kind_event, ctx.now())))],
            None => vec![Command::Emit(EventKind::new("unparsed", &ev.kind.body))],
        }
    }
}

struct NoEffects;
impl EffectRunner for NoEffects {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        Err(format!("no runner for effect {}", spec.name))
    }
}

fn driver(clock: &VirtualClock, j: Arc<dyn Journal>, sources: Sources) -> runtime::Result<VirtualDriver> {
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let engine = Engine::open(j, c, vec![Registration::new("mail.wake.stub", Consumer)], 1).unwrap();
    VirtualDriver::new(engine, clock.clone(), Arc::new(NoEffects)).with_sources(sources)
}

fn scripted(clock: &VirtualClock, script: Vec<(u64, Submit)>) -> Sources {
    let mut s = Sources::default();
    s.register(Box::new(ScriptedSource::new("mail.arrived.script", Arc::new(clock.clone()), script))).unwrap();
    s
}

#[test]
fn the_declared_event_reaches_a_consumer_on_the_mailboxs_stream_at_its_virtual_time() {
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(0));
    let reply = Arrived { mailbox: "concierge".into(), kind_event: false };
    let event = Arrived { mailbox: "concierge".into(), kind_event: true };
    let mut v = driver(&clock, j.clone(), scripted(&clock, vec![(100, reply.submit(1)), (200, event.submit(2))])).unwrap();
    v.run_until(1_000).unwrap();
    let consumed: Vec<String> = trace(&*j).into_iter().filter(|t| t.2 == "consumed" || t.2 == "unparsed").map(|t| t.3).collect();
    assert_eq!(consumed, ["mail/concierge|concierge|false@100", "mail/concierge|concierge|true@200"]);
    assert_eq!(stream("concierge"), "mail/concierge");
}

#[test]
fn the_real_source_is_refused_under_virtual_time_naming_it() {
    let clock = VirtualClock::new(0);
    let mut s = scripted(&clock, vec![]);
    s.register(Box::new(MailArrived { mailboxes: vec![] })).unwrap();
    let err = driver(&clock, MemJournal::new(), s).err().expect("a source that reads the world is refused");
    assert!(err.0.contains(NAME) && !err.0.contains("mail.arrived.script"), "{err}");
    assert_eq!(BACKEND, "inotify");
}

#[test]
fn the_real_source_submits_one_catch_up_per_mailbox_with_mail_already_unread() {
    let dir = testkit::TempDir::new("mail-arrived");
    let new = dir.path().join("concierge/new");
    std::fs::create_dir_all(&new).unwrap();
    std::fs::write(new.join("m1"), "X-Spira-Kind: note\n\nhello\n").unwrap();
    let (tx, mut rx) = mpsc::channel(4);
    let source = MailArrived { mailboxes: vec![("concierge".into(), dir.path().join("concierge"))] };
    let h = std::thread::spawn(move || Box::new(source).run(tx));
    let first = rx.blocking_recv();
    drop(rx);
    if let Err(p) = h.join() {
        std::panic::resume_unwind(p);
    }
    let first = first.expect("the catch-up submit");
    assert_eq!((first.stream.as_str(), Arrived::from_event_kind(&first.kind).map(|a| a.mailbox)), ("mail/concierge", Some("concierge".to_string())));
}

#[test]
fn the_real_source_returns_once_its_channel_is_closed() {
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    Box::new(MailArrived { mailboxes: vec![] }).run(tx);
}
