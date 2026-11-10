//! The control socket's `scheduler` verbs, over a real Unix socket.

use spira_runtime::status;

use std::sync::{Arc, Mutex};

use runtime::engine::StatusBoard;
use runtime::journal::MemJournal;
use runtime::ports::VirtualClock;
use runtime::socket::{answer, lc_call};
use runtime::*;

struct Ticker;
impl Handler for Ticker {
    fn interests(&self) -> &[EventKindTag] {
        &["tick"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        vec![
            Command::Schedule { at: ev.at + 1000, kind: EventKind::new("tick", "") },
            Command::Emit(EventKind::new("ticked", "ok")),
            Command::Effect(EffectSpec::keyed("write", "write", String::new(), 5000)),
        ]
    }
}

type Queue = Arc<Mutex<Vec<(String, String, EventKind)>>>;

fn serve(control: status::Control, dir: &testkit::TempDir) -> std::path::PathBuf {
    let path = dir.path().join("control.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || answer(&listener, &|argv| control.respond(argv)));
    path
}

/// Hears nothing: a task on the signal plane whose row is all `status` reads.
struct Silent;
impl runtime::Handler for Silent {
    fn interests(&self) -> &[runtime::EventKindTag] {
        &[]
    }
    fn handle(&self, _: &runtime::Ctx, _: &runtime::Projection, _: &runtime::Event) -> Vec<runtime::Command> {
        vec![]
    }
}

#[test]
fn status_halt_and_resume_round_trip_over_the_socket() {
    let (j, clock) = (MemJournal::new(), VirtualClock::new(0));
    let board = Arc::new(StatusBoard::default());
    let mut e = Engine::open(j, Arc::new(clock.clone()), vec![Registration::new("ticker", Ticker).with_effects(&["work", "page"]).with_signal_effects(&["page"]), Registration::new("pager", Silent).with_plane(runtime::Plane::Signal)], 1).unwrap().with_status_board(board.clone());
    let queue: Queue = Arc::default();
    let q = queue.clone();
    let submit: status::SubmitFn = Arc::new(move |id: &str, stream: &str, kind: EventKind| {
        q.lock().unwrap().push((id.into(), stream.into(), kind));
        Ok(())
    });
    let dir = testkit::TempDir::new("status-sock");
    let path = serve(status::Control::new(board.clone(), submit, Arc::new(clock.clone())), &dir);

    let early = lc_call(&path, &["scheduler", "status"]).unwrap_err();
    assert!(early.0.contains("no turn"), "before the first turn there is no picture: {early}");

    e.submit("t1", "t", EventKind::new("tick", "")).unwrap();
    clock.advance_to(40);
    e.run_until_idle().unwrap();
    let doc: serde_json::Value = serde_json::from_str(&lc_call(&path, &["scheduler", "status"]).unwrap()).unwrap();
    assert_eq!(doc["schema"], status::SCHEMA);
    assert_eq!(doc["at"], 40);
    assert_eq!(doc["halted"], false);
    let t = &doc["tasks"][0];
    assert_eq!(t["name"], "ticker");
    assert_eq!(t["mode"], "act");
    assert_eq!(t["plane"], "work");
    assert_eq!(t["effects"], serde_json::json!([{"name": "work", "plane": "work"}, {"name": "page", "plane": "signal"}]), "status names each effect's plane under its task");
    assert_eq!((doc["tasks"][1]["name"].as_str(), doc["tasks"][1]["plane"].as_str()), (Some("pager"), Some("signal")), "status names each task's plane");
    assert_eq!((t["last_due_at"].as_u64(), t["last_fired_at"].as_u64(), t["lag_ms"].as_u64()), (Some(0), Some(40), Some(40)));
    assert_eq!(t["next_due_at"], 1000);
    assert_eq!(t["state"]["tag"], "ticked");
    assert_eq!(t["state"]["body_len"], 2);
    assert_eq!(t["in_flight"].as_array().unwrap().len(), 1);
    assert!(t["last_outcome"].is_null());
    assert!(t["last_commands"]["returned"].is_u64() && t["last_commands"]["admitted"].is_u64(), "status carries what the last fire returned and the engine admitted: {t}");
    assert_eq!(doc["classes"][0], serde_json::json!({"class": "host", "depth": 1, "refusals": 0}));

    assert!(lc_call(&path, &["scheduler", "halt", "maintenance"]).is_ok());
    for (id, stream, kind) in queue.lock().unwrap().drain(..) {
        e.submit(&id, &stream, kind).unwrap();
    }
    e.run_until_idle().unwrap();
    let doc: serde_json::Value = serde_json::from_str(&lc_call(&path, &["scheduler", "status"]).unwrap()).unwrap();
    assert_eq!(doc["halted"], true, "the halt verb journals a halt: {doc}");

    assert!(lc_call(&path, &["scheduler", "resume"]).is_ok());
    let ids: Vec<String> = queue.lock().unwrap().iter().map(|q| q.0.clone()).collect();
    for (id, stream, kind) in queue.lock().unwrap().drain(..) {
        assert_eq!((stream.as_str(), kind.tag.as_str()), (HALT_STREAM, SCHEDULER_RESUMED));
        e.submit(&id, &stream, kind).unwrap();
    }
    e.run_until_idle().unwrap();
    let doc: serde_json::Value = serde_json::from_str(&lc_call(&path, &["scheduler", "status"]).unwrap()).unwrap();
    assert_eq!(doc["halted"], false);
    assert_eq!(ids.len(), 1);

    let unknown = lc_call(&path, &["scheduler", "explode"]).unwrap_err();
    assert!(unknown.is_refused(), "an unknown verb is refused: {unknown}");
}
