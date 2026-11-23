use super::*;

mod cert_log;
mod closes;
mod content_writes;
mod deadline;
mod drain;
mod filing;
mod gc;
mod holds;
mod lifecycle_rows;
mod request_id;
mod served;
mod spans;
mod startup;
mod sweep;
mod wire;
mod workers;

fn lc(store: &crate::control::store::Store<state_store::MemBackend>, argv: &[&str], now: i64) -> String {
    let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let request = crate::control::served::request_id();
    let (code, out) = crate::control::state::run(store, &args, &crate::control::state::Call { now, request: &request, inputs: &crate::control::claimable::FIXED });
    assert_eq!(code, 0, "{argv:?}: {out}");
    out
}

fn ask(sink: Option<&spira_otel::Sink>, request: &str) -> (String, i32) {
    use std::io::{Read, Write};
    let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    (&client).write_all(request.as_bytes()).unwrap();
    let door = super::Door { ledger: super::Ledger::new(std::time::Duration::from_secs(60)), work: super::live_work(), deadlines: test_deadlines() };
    super::serve_one(server, sink, std::time::Instant::now(), &door);
    let mut got = String::new();
    (&client).read_to_string(&mut got).unwrap();
    let head: serde_json::Value = serde_json::from_str(got.lines().next().unwrap()).unwrap();
    (got, head["exit_code"].as_i64().unwrap() as i32)
}

use crate::callers::world::World;

use crate::callers::Machine;

/// bd's content half as a double: answers every read with its argv, records every write.
#[derive(Default)]
struct Content(std::cell::RefCell<Vec<String>>);

impl crate::composed::Content for Content {
    fn read(&self, args: &[String]) -> Result<String, crate::composed::ReadError> {
        Ok(format!("read {}", args.join(" ")))
    }
    fn write(&self, args: &[&str]) -> Result<String, String> {
        self.0.borrow_mut().push(args.join(" "));
        Ok(String::new())
    }
    fn write_stdin(&self, args: &[String], stdin: Option<&str>) -> Result<String, String> {
        let body = stdin.map_or(String::new(), |b| format!(" <<{b}"));
        self.0.borrow_mut().push(format!("{}{body}", args.join(" ")));
        Ok(format!("wrote {}", args.join(" ")))
    }
    fn mutate(&self, m: &crate::content_write::Mutation) -> Result<String, String> {
        self.write_stdin(&m.argv, m.body.as_deref())
    }
    fn delete(&self, _: &[String]) -> Result<(), String> {
        Ok(())
    }
    fn ask_label(&self) -> Result<String, String> {
        // literal-ok: test fixture label
        Ok("needs-ryan".into())
    }
    fn registry(&self) -> spira_config::repos::Registry {
        spira_config::repos::Registry::new(Some("spira|r/spira|main|gate.sh\n"), &Default::default(), std::path::Path::new("r/spira"))
    }
}

/// One request through the server's own reader, answer and writer, with `w` as its store and
/// bd; the client half is spira-lc's own socket client.
fn over(w: &World, content: &Content, argv: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let body = stdin.map(str::to_string);
    let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    let asked = std::thread::spawn(move || crate::client::exchange(client, &argv, body.as_deref()));
    let mut reader = std::io::BufReader::new(server.try_clone().unwrap());
    let (req, _) = super::read_request(&mut reader).unwrap().unwrap();
    let (mut m, mut b) = (w, w);
    let ans = super::answer(req, &mut m, &mut b, content, &|| Err("no run dir here".to_string()));
    super::reply_then_settle(&server, &ans, false, || {}, &mut |id, reason| crate::reconcile_content::settle_owed(&mut b, id, reason));
    drop(server);
    asked.join().unwrap()
}

fn filed(w: &World, id: &str) {
    w.file(id, "task", None, &[]);
    w.ok(&["create-bead", id, "--ready"]);
}

use std::sync::{Arc, Condvar, Mutex};

use std::time::Duration;

/// bd's content half as a double that is safe to share with the worker threads: records
/// each write, and parks a write until released while `held`.
struct Notes {
    wrote: Mutex<Vec<String>>,
    mutated: Mutex<Vec<String>>,
    held: Mutex<bool>,
    released: Condvar,
}

impl Notes {
    fn new(held: bool) -> Arc<Notes> {
        Arc::new(Notes { wrote: Mutex::new(Vec::new()), mutated: Mutex::new(Vec::new()), held: Mutex::new(held), released: Condvar::new() })
    }
    fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.released.notify_all();
    }
    fn writes(&self) -> Vec<String> {
        self.wrote.lock().unwrap().clone()
    }
    fn mutations(&self) -> Vec<String> {
        self.mutated.lock().unwrap().clone()
    }
}

impl crate::composed::Content for Notes {
    fn read(&self, args: &[String]) -> Result<String, crate::composed::ReadError> {
        Ok(format!("read {}", args.join(" ")))
    }
    fn write(&self, args: &[&str]) -> Result<String, String> {
        self.wrote.lock().unwrap().push(args.join(" "));
        Ok(String::new())
    }
    fn write_stdin(&self, args: &[String], stdin: Option<&str>) -> Result<String, String> {
        let mut held = self.held.lock().unwrap();
        while *held {
            held = self.released.wait(held).unwrap();
        }
        drop(held);
        let body = stdin.map_or(String::new(), |b| format!(" <<{b}"));
        self.wrote.lock().unwrap().push(format!("{}{body}", args.join(" ")));
        Ok(format!("wrote {}", args.join(" ")))
    }
    fn mutate(&self, m: &crate::content_write::Mutation) -> Result<String, String> {
        let mut held = self.held.lock().unwrap();
        while *held {
            held = self.released.wait(held).unwrap();
        }
        drop(held);
        let body = m.body.as_deref().map_or(String::new(), |b| format!(" <<{b}"));
        self.mutated.lock().unwrap().push(format!("{}{body}", m.argv.join(" ")));
        Ok(String::new())
    }
    fn delete(&self, _: &[String]) -> Result<(), String> {
        Ok(())
    }
    fn ask_label(&self) -> Result<String, String> {
        // literal-ok: test fixture label
        Ok("needs-ryan".into())
    }
    fn registry(&self) -> spira_config::repos::Registry {
        spira_config::repos::Registry::new(Some("spira|r/spira|main|gate.sh\n"), &Default::default(), std::path::Path::new("r/spira"))
    }
}

struct Rig {
    world: Arc<Mutex<World>>,
    notes: Arc<Notes>,
    door: Arc<super::Door>,
}

fn rig(held: bool, deadlines: super::Deadlines) -> Rig {
    let world = Arc::new(Mutex::new(World::new()));
    let notes = Notes::new(held);
    let (w, n) = (Arc::clone(&world), Arc::clone(&notes));
    let work: super::Work = Arc::new(move |req| {
        let world = w.lock().unwrap_or_else(|e| e.into_inner());
        let (mut m, mut b) = (&*world, &*world);
        super::answer(req, &mut m, &mut b, &*n, &|| Err("no run dir here".to_string()))
    });
    let door = Arc::new(super::Door { ledger: super::Ledger::new(Duration::from_secs(60)), work, deadlines });
    Rig { world, notes, door }
}

fn three_seconds() -> super::Deadlines {
    super::Deadlines { default: Duration::from_secs(3) }
}

fn test_deadlines() -> super::Deadlines {
    super::Deadlines { default: Duration::from_millis(1000) }
}

fn dl(argv: &[String]) -> Duration {
    super::request_deadline(argv, &test_deadlines())
}

fn config(deadline: Option<&str>) -> impl Fn(&str) -> Result<String, String> {
    let deadline = deadline.map(str::to_string);
    move |k| match k {
        "SPIRA_LC_REQUEST_DEADLINE_MS" => deadline.clone().ok_or_else(|| "missing".to_string()),
        other => Err(format!("{other} is not read")),
    }
}

/// One request, `request_id` in its head, through `serve_one` over a socket pair.
fn send(door: &Arc<super::Door>, argv: &[&str], request_id: Option<&str>) -> (i32, String) {
    send_spanned(door, None, argv, None, request_id)
}

fn send_body(door: &Arc<super::Door>, argv: &[&str], body: Option<&str>, request_id: Option<&str>) -> (i32, String) {
    send_spanned(door, None, argv, body, request_id)
}

fn send_spanned(door: &Arc<super::Door>, sink: Option<&spira_otel::Sink>, argv: &[&str], body: Option<&str>, request_id: Option<&str>) -> (i32, String) {
    use std::io::{Read, Write};
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let head = spira_store::lc_call::request_head(&argv, body.map(str::len), &Default::default(), None, request_id).unwrap();
    let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    (&client).write_all(format!("{head}\n{}", body.unwrap_or("")).as_bytes()).unwrap();
    let mut got = String::new();
    std::thread::scope(|scope| {
        scope.spawn(|| super::serve_one(server, sink, std::time::Instant::now(), door));
        (&client).read_to_string(&mut got).unwrap();
    });
    let (line, rest) = got.split_once('\n').unwrap();
    let head: serde_json::Value = serde_json::from_str(line).unwrap();
    let len = head["len"].as_u64().unwrap() as usize;
    (head["exit_code"].as_i64().unwrap() as i32, rest[..len].to_string())
}

fn history_rows(r: &Rig, id: &str) -> usize {
    let shown: serde_json::Value = serde_json::from_str(&r.world.lock().unwrap().ok(&["history", id, "--machine", "bead"])).unwrap();
    shown.as_array().map(Vec::len).or_else(|| shown["history"].as_array().map(Vec::len)).unwrap_or_else(|| panic!("{shown}"))
}

fn drop_argv(r: &Rig, id: &str) -> Vec<String> {
    let w = r.world.lock().unwrap();
    let shown: serde_json::Value = serde_json::from_str(&w.ok(&["show", id])).unwrap();
    let (state, version) = (shown["bead"]["state"].as_str().unwrap().to_string(), shown["bead"]["version"].as_str().unwrap().to_string());
    let kind = serde_json::to_string(&lifecycle::bead::BeadEventKind::Drop { reason: lifecycle::reason::DropReason::Unwanted }).unwrap();
    ["event", "bead", id, "--expect", &state, "--version", &version, "--actor", "test", "--kind", &kind].map(String::from).to_vec()
}

fn fifty_ms() -> super::Deadlines {
    super::Deadlines { default: Duration::from_millis(50) }
}

/// bd as the content door writes it, over `World`'s planning plane: a create files the
/// bead and prints its id, a close closes it.
struct Filing<'w>(&'w World, &'w str);

impl crate::composed::Content for Filing<'_> {
    fn read(&self, _: &[String]) -> Result<String, crate::composed::ReadError> {
        Ok(String::new())
    }
    fn write(&self, _: &[&str]) -> Result<String, String> {
        Ok(String::new())
    }
    fn write_stdin(&self, args: &[String], _: Option<&str>) -> Result<String, String> {
        match args.first().map(String::as_str) {
            Some("create") => {
                self.0.file(self.1, "task", None, &[]);
                Ok(format!("{}\n", self.1))
            }
            Some("close") => {
                let mut b = self.0;
                crate::callers::Bd::close(&mut b, &args[1], "")?;
                Ok(String::new())
            }
            _ => Ok(String::new()),
        }
    }
    fn mutate(&self, m: &crate::content_write::Mutation) -> Result<String, String> {
        self.write_stdin(&m.argv, m.body.as_deref())
    }
    fn delete(&self, _: &[String]) -> Result<(), String> {
        Ok(())
    }
    fn ask_label(&self) -> Result<String, String> {
        // literal-ok: test fixture label
        Ok("needs-ryan".into())
    }
    fn registry(&self) -> spira_config::repos::Registry {
        spira_config::repos::Registry::new(Some("spira|r/spira|main|gate.sh\n"), &Default::default(), std::path::Path::new("r/spira"))
    }
    fn policy(&self) -> Result<bead::filing::Policy, String> {
        let builder = bead::filing::Persona { name: "builder".into(), labels: vec!["builder-lane".into()], requires_build_spec: true, unresolved: None };
        Ok(bead::filing::Policy { personas: vec![builder], ..bead::filing::Policy::none() })
    }
}

fn served_with(w: &World, c: &dyn crate::composed::Content, argv: &[&str], stdin: &str) -> super::Answer {
    let req = super::Request { argv: argv.iter().map(|a| a.to_string()).collect(), stdin: Some(stdin.to_string()), caller: Default::default(), task: None, request_id: None };
    let (mut m, mut b) = (w, w);
    super::answer(req, &mut m, &mut b, c, &|| Err("no run dir here".to_string()))
}

fn served(w: &World, c: &dyn crate::composed::Content, argv: &[&str], caller: &[(&str, &str)]) -> super::Answer {
    let req = super::Request {
        argv: std::iter::once("content").chain(argv.iter().copied()).map(str::to_string).collect(),
        stdin: None,
        caller: caller.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        task: None,
        request_id: None,
    };
    let (mut m, mut b) = (w, w);
    super::answer(req, &mut m, &mut b, c, &|| Err("no run dir here".to_string()))
}

fn state(w: &World, id: &str) -> Option<spira_store::lc_state::Row> {
    match w.lc(&["show", id]) {
        (0, out) => spira_store::lc_state::parse_show(&out).unwrap(),
        _ => None,
    }
}
