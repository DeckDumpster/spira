//! `spira-sub run`: the one broker connection, the control socket over it and the set file.
//! Nothing here wakes on a timer while no `--at` or batch is pending and the broker answers.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use ports::Clock;
use rumqttc::{Client, Connection, Event, Packet, QoS};
use serde_json::json;
use spira_otel::metrics::{MetricsSink, Point};
use spira_pub::link::{sleep_until_readable, Dial};
use spira_pub::SocketWatch;

use crate::core::{Core, Out, Saved, LIVENESS};
use crate::protocol::{Reply, Request};

/// The most a command waits for the broker to acknowledge what it asked of it.
const ACK_DEADLINE: Duration = Duration::from_secs(3);
/// Before trying again a broker that is there and did not take the connection.
const RETRY: Duration = Duration::from_secs(5);
/// The longest one sleep toward a timer, so a stepped wall clock is noticed.
const TIMER_SLICE: Duration = Duration::from_secs(60);
/// Requests the client holds ahead of the connection: the whole set, resubscribed at once.
const CHANNEL: usize = 1 << 12;
/// Deliveries a reader may fall behind before it is cut off; it re-attaches.
const BACKLOG: usize = 1 << 16;

pub const DELIVERIES: &str = "spira_sub_deliveries";
pub const SUBSCRIPTIONS: &str = "spira_sub_subscriptions";
pub const COMMANDS: &str = "spira_sub_control_commands";

pub struct Options {
    pub name: String,
    pub broker: PathBuf,
    pub control_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    pub metrics: Option<MetricsSink>,
}

pub fn socket_path(control_dir: &Path, name: &str) -> PathBuf {
    control_dir.join(format!("{name}.sock"))
}

pub fn set_path(control_dir: &Path, name: &str) -> PathBuf {
    control_dir.join(format!("{name}.subs"))
}

enum Sample {
    Delivered(usize, usize),
    Command(&'static str, usize),
}

/// Pushes from its own thread, woken only by a sample, so a slow store delays no delivery.
fn telemetry(sink: Option<MetricsSink>, name: String) -> Option<SyncSender<Sample>> {
    let sink = sink?;
    let (tx, rx) = sync_channel::<Sample>(1024);
    let spawned = std::thread::Builder::new().name("spira-sub-otel".into()).spawn(move || {
        for sample in rx {
            let (point, held, command) = match sample {
                Sample::Delivered(n, held) => (Point::Count { name: DELIVERIES, value: n as u64 }, held, None),
                Sample::Command(command, held) => (Point::Count { name: COMMANDS, value: 1 }, held, Some(command)),
            };
            let mut labels = vec![("subscriber", name.as_str())];
            labels.extend(command.map(|c| ("command", c)));
            sink.push(&[point, Point::Gauge { name: SUBSCRIPTIONS, value: held as f64 }], &labels);
        }
    });
    spawned.ok().map(|_| tx)
}

struct Reader {
    id: u64,
    lines: SyncSender<String>,
}

struct State {
    core: Core,
    client: Option<Client>,
    /// Connections made so far: an acknowledgement counts only on the one that was asked.
    connection: u64,
    /// This connection's `up` payload, until it comes back.
    up: Option<String>,
    /// The broker held this subscriber's state from before this connection.
    kept: bool,
    asked: u64,
    acked: u64,
    readers: Vec<Reader>,
    next_reader: u64,
}

struct Server {
    name: String,
    set: PathBuf,
    state: Mutex<State>,
    changed: Condvar,
    telemetry: Option<SyncSender<Sample>>,
}

fn say(name: &str, line: &str) {
    eprintln!("spira-sub[{name}]: {line}");
}

/// The set, or an empty one when no file is there yet. A file that does not parse is refused:
/// starting empty would overwrite it.
fn load(path: &Path) -> Result<Saved, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} is not a subscription set: {e}", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Saved::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn store(path: &Path, saved: &Saved) -> std::io::Result<()> {
    spira_config::write_atomic(path, &(serde_json::to_string(saved)? + "\n"))
}

/// What carrying out a step left behind.
struct Applied {
    /// The last acknowledgement asked for.
    ticket: Option<(u64, u64)>,
    /// Why the set file does not hold the set as it now stands.
    unsaved: Option<String>,
}

fn unsaved(live: &str, why: &str) -> Reply {
    Reply::Failed(format!("{live} but not saved, so a restart of `spira-sub run` undoes it: {why}"))
}

impl State {
    /// Sends one SUBSCRIBE or UNSUBSCRIBE and returns the acknowledgement it waits for.
    fn ask(&mut self, filter: &str, subscribe: bool) -> Option<(u64, u64)> {
        let client = self.client.as_ref()?;
        let sent = match subscribe {
            true => client.try_subscribe(filter, QoS::AtLeastOnce),
            false => client.try_unsubscribe(filter),
        };
        sent.ok()?;
        self.asked += 1;
        Some((self.connection, self.asked))
    }
}

impl Server {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn sample(&self, sample: Sample) {
        if let Some(tx) = &self.telemetry {
            let _ = tx.try_send(sample);
        }
    }

    /// Carries out what one step of the core owes.
    fn apply(&self, state: &mut State, out: Out) -> Applied {
        let mut unsaved = None;
        if out.changed {
            if let Err(e) = store(&self.set, &state.core.saved()) {
                let why = format!("cannot write {}: {e}", self.set.display());
                say(&self.name, &why);
                unsaved = Some(why);
            }
        }
        // Queued only once saved: a `run` killed after a reader has a value must not resend it.
        if !out.lines.is_empty() {
            for line in &out.lines {
                state.readers.retain(|r| r.lines.try_send(line.clone()).is_ok());
            }
            self.sample(Sample::Delivered(out.lines.len(), state.core.len()));
        }
        let mut ticket = None;
        for filter in &out.unsubscribe {
            ticket = state.ask(filter, false).or(ticket);
        }
        self.changed.notify_all();
        Applied { ticket, unsaved }
    }

    /// Waits for the broker's acknowledgement; false when it did not come.
    fn acknowledged(&self, state: MutexGuard<'_, State>, (connection, ticket): (u64, u64)) -> bool {
        let waiting = |s: &mut State| s.connection == connection && s.client.is_some() && s.acked < ticket;
        let (state, _) = self.changed.wait_timeout_while(state, ACK_DEADLINE, waiting).unwrap_or_else(|e| e.into_inner());
        state.connection == connection && state.acked >= ticket
    }

    fn command(&self, request: Request) -> Reply {
        let mut state = self.lock();
        self.sample(Sample::Command(request.name(), state.core.len()));
        let (printed, ticket) = match request {
            Request::Add { filter, note, once, batch_s } => {
                let listening = !state.readers.is_empty();
                match state.core.add_classed(&filter, &note, once, batch_s, listening) {
                    Ok((id, out)) => {
                        // Asked even when the filter is already held: that is what resends the
                        // retained values, which the other holders recognise and skip.
                        let ticket = state.ask(&filter, true);
                        if let Some(why) = self.apply(&mut state, out).unsaved {
                            return unsaved(&format!("{id} is live"), &why);
                        }
                        (id, ticket)
                    }
                    Err(why) => return Reply::Refused(why),
                }
            }
            Request::At { at_ms, note } => {
                let (id, out) = state.core.add_at(at_ms, &note);
                return match self.apply(&mut state, out).unsaved {
                    Some(why) => unsaved(&format!("{id} is set"), &why),
                    None => Reply::Done { out: id, warning: None },
                };
            }
            Request::Drop { id } => match state.core.remove(&id) {
                Ok(out) => {
                    let owes = !out.unsubscribe.is_empty();
                    let applied = self.apply(&mut state, out);
                    if let Some(why) = applied.unsaved {
                        return unsaved(&format!("{id} is dropped"), &why);
                    }
                    if !owes {
                        return Reply::Done { out: String::new(), warning: None };
                    }
                    (String::new(), applied.ticket)
                }
                Err(why) => return Reply::Refused(why),
            },
            Request::Set { id, batch_s } => {
                let listening = !state.readers.is_empty();
                return match state.core.set_class(&id, batch_s, listening) {
                    Ok(out) => match self.apply(&mut state, out).unsaved {
                        Some(why) => unsaved(&format!("{id} is changed"), &why),
                        None => Reply::Done { out: String::new(), warning: None },
                    },
                    Err(why) => Reply::Refused(why),
                };
            }
            Request::List => {
                let screen = state.core.list(&self.name, state.client.is_some(), state.readers.len());
                return Reply::Done { out: screen, warning: None };
            }
            Request::Follow => return Reply::Refused("follow is not a one-reply command".to_string()),
        };
        let confirmed = match ticket {
            Some(ticket) => self.acknowledged(state, ticket),
            None => false,
        };
        let warning = (!confirmed).then(|| "the broker has not confirmed this; it is applied on the next connect".to_string());
        Reply::Done { out: printed, warning }
    }

    /// Registers a reader with its replay already queued ahead of anything newer.
    fn attach(&self) -> (u64, Receiver<String>) {
        let (tx, rx) = sync_channel(BACKLOG);
        let mut state = self.lock();
        self.sample(Sample::Command(Request::Follow.name(), state.core.len()));
        state.next_reader += 1;
        let id = state.next_reader;
        let replay = state.core.attach();
        // What the replay holds has been given to nobody, so every reader gets it.
        state.readers.push(Reader { id, lines: tx });
        self.apply(&mut state, replay);
        (id, rx)
    }

    fn detach(&self, id: u64) {
        self.lock().readers.retain(|r| r.id != id);
    }

    fn serve(self: &Arc<Self>, mut stream: UnixStream) -> std::io::Result<()> {
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        let reply = |stream: &mut UnixStream, reply: &Reply| writeln!(stream, "{}", serde_json::to_string(reply)?);
        let request: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => return reply(&mut stream, &Reply::Refused(format!("unreadable request: {e}"))),
        };
        if request != Request::Follow {
            let answer = self.command(request);
            return reply(&mut stream, &answer);
        }
        let (id, lines) = self.attach();
        reply(&mut stream, &Reply::Done { out: String::new(), warning: None })?;
        let mut writing = stream.try_clone()?;
        let server = self.clone();
        std::thread::Builder::new().name("spira-sub-reader".into()).spawn(move || {
            for line in lines {
                if writeln!(writing, "{line}").is_err() {
                    break;
                }
            }
            // Cut off or gone: the closed socket ends the read below.
            let _ = writing.shutdown(std::net::Shutdown::Both);
            server.detach(id);
        })?;
        // A reader says nothing more, so the end of this read is its going.
        let mut sink = [0u8; 64];
        while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
        self.detach(id);
        Ok(())
    }

    /// Fires `--at` timers and due batches: asleep for good while there is none pending, or
    /// nobody to tell.
    fn timers(&self) {
        let mut state = self.lock();
        loop {
            let due = state.core.next_due_ms().filter(|_| !state.readers.is_empty());
            let now = state.core.now_ms();
            state = match due {
                Some(at) if at <= now => {
                    for out in [state.core.flush_due(true), state.core.fire_due(true)] {
                        self.apply(&mut state, out);
                    }
                    state
                }
                Some(at) => {
                    let sleep = Duration::from_millis(at - now).min(TIMER_SLICE);
                    self.changed.wait_timeout(state, sleep).unwrap_or_else(|e| e.into_inner()).0
                }
                None => self.changed.wait(state).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }

    fn client_id(&self) -> String {
        format!("spira-sub-{}", self.name)
    }

    fn own_topic(&self) -> String {
        format!("subscriber/{}", self.name)
    }

    /// Takes the whole set to the new connection and then says it is up. The broker works
    /// through these in order, so `up` coming back ends the retained values they bring.
    fn connected(&self, client: Client, now_ms: u64) {
        let mut state = self.lock();
        state.client = Some(client.clone());
        state.connection += 1;
        state.core.reconnected();
        (state.asked, state.acked) = (0, 0);
        for filter in [self.own_topic(), LIVENESS.to_string()].into_iter().chain(state.core.filters()) {
            if state.ask(&filter, true).is_none() {
                say(&self.name, &format!("cannot subscribe {filter}"));
            }
        }
        let up = json!({"state": "up", "since_ms": now_ms}).to_string();
        state.up = Some(up.clone());
        state.kept = false;
        if let Err(e) = client.try_publish(self.own_topic(), QoS::AtLeastOnce, true, up) {
            say(&self.name, &format!("cannot say it is up: {e}"));
        }
        self.changed.notify_all();
    }

    /// Watches this subscriber's own topic for the end of the retained values. Only a broker
    /// that held a copy before `up` came back is taken to have kept its state: a restarted one
    /// whose publishers have not republished must prune nothing.
    fn end_of_retained(&self, state: &mut State, payload: &[u8], retain: bool) {
        if retain {
            state.kept = true;
        } else if state.up.as_ref().is_some_and(|up| up.as_bytes() == payload) {
            state.up = None;
            if state.kept {
                let out = state.core.synced();
                self.apply(state, out);
            }
        }
    }

    /// Drives one connection until it ends, and says why.
    fn drive(&self, connection: &mut Connection) -> String {
        loop {
            match connection.recv() {
                Ok(Ok(Event::Incoming(Packet::Publish(p)))) => {
                    let mut state = self.lock();
                    if p.topic == self.own_topic() {
                        self.end_of_retained(&mut state, &p.payload, p.retain);
                    }
                    let listening = !state.readers.is_empty();
                    let out = state.core.message(&p.topic, &p.payload, listening);
                    self.apply(&mut state, out);
                }
                Ok(Ok(Event::Incoming(Packet::SubAck(_) | Packet::UnsubAck(_)))) => {
                    self.lock().acked += 1;
                    self.changed.notify_all();
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return e.to_string(),
                Err(_) => return "the client closed".to_string(),
            }
        }
    }

    /// Holds a connection for as long as there is a broker. Between brokers it sleeps until the
    /// next one makes its socket.
    fn connection(&self, broker: &Path, clock: &dyn Clock) -> String {
        let client_id = self.client_id();
        let dial = Dial { client_id: &client_id, socket: broker, will_topic: self.own_topic(), capacity: CHANNEL };
        let mut watch = SocketWatch::new(broker);
        // Whether the broker's socket has just appeared, so a refusal is a not-yet.
        let mut announced = false;
        loop {
            if let Err(why) = watch.arm() {
                return why;
            }
            watch.drain();
            match dial.connect(announced) {
                Ok((client, mut connection)) => {
                    self.connected(client, clock.now_ms());
                    let why = self.drive(&mut connection);
                    self.lock().client = None;
                    self.changed.notify_all();
                    say(&self.name, &format!("lost the broker: {why}"));
                    // Once at once, in case only this connection went.
                    announced = false;
                }
                Err(lost) => {
                    if announced || !lost.refused {
                        say(&self.name, &format!("cannot connect: {}", lost.why));
                    }
                    // A broker that is not there announces itself; one that is there may not.
                    let retry = (!lost.refused).then_some(RETRY);
                    loop {
                        sleep_until_readable([watch.fd(), None], retry);
                        announced = watch.drain();
                        if announced || retry.is_some() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// One `run` per name: two would share a client id and take the broker connection from each
/// other for ever. The lock lives as long as the returned file.
fn only_instance(options: &Options) -> Result<std::fs::File, String> {
    let path = options.control_dir.join(format!("{}.lock", options.name));
    match strand::state::try_lock(&path) {
        Ok(Some(file)) => Ok(file),
        Ok(None) => Err(format!("another `spira-sub run --as {}` holds {}", options.name, path.display())),
        Err(e) => Err(format!("cannot lock {}: {e}", path.display())),
    }
}

/// Runs until killed; returns only a failure to do so.
pub fn run(options: Options) -> Result<(), String> {
    let dir = &options.control_dir;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let _only = only_instance(&options)?;
    let set = set_path(dir, &options.name);
    let core = Core::new(options.clock.clone(), load(&set)?);
    let socket = socket_path(dir, &options.name);
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).map_err(|e| format!("cannot listen on {}: {e}", socket.display()))?;
    let server = Arc::new(Server {
        telemetry: telemetry(options.metrics, options.name.clone()),
        name: options.name,
        set,
        state: Mutex::new(State { core, client: None, connection: 0, up: None, kept: false, asked: 0, acked: 0, readers: Vec::new(), next_reader: 0 }),
        changed: Condvar::new(),
    });
    let spawn = |name: &str, body: Box<dyn FnOnce() + Send>| {
        std::thread::Builder::new().name(name.into()).spawn(body).map(drop).map_err(|e| format!("cannot start {name}: {e}"))
    };
    let timing = server.clone();
    spawn("spira-sub-timers", Box::new(move || timing.timers()))?;
    let accepting = server.clone();
    spawn(
        "spira-sub-control",
        Box::new(move || {
            for stream in listener.incoming().flatten() {
                let server = accepting.clone();
                let serving = std::thread::Builder::new().name("spira-sub-request".into()).spawn(move || {
                    if let Err(e) = server.serve(stream) {
                        say(&server.name, &format!("control request failed: {e}"));
                    }
                });
                if let Err(e) = serving {
                    eprintln!("spira-sub: cannot serve a control request: {e}");
                }
            }
        }),
    )?;
    Err(server.connection(&options.broker, &*options.clock))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::VirtualClock;
    use testkit::TempDir;

    fn server(dir: &Path, core: Core) -> Server {
        Server {
            name: "t".into(),
            set: dir.join("t.subs"),
            state: Mutex::new(State { core, client: None, connection: 0, up: None, kept: false, asked: 0, acked: 0, readers: Vec::new(), next_reader: 0 }),
            changed: Condvar::new(),
            telemetry: None,
        }
    }

    // Only an attach that beats the timer thread to a due batch replays to a standing reader,
    // so no timer thread runs here.
    #[test_id(tzdad0i)]
    #[test]
    fn what_an_attach_replays_reaches_the_readers_already_attached() {
        let dir = TempDir::new("sub-attach");
        let clock = VirtualClock::new(1_000_000);
        let mut core = Core::new(Arc::new(clock.clone()), Saved::default());
        core.add_classed("release/active", "", false, Some(60), true).unwrap();
        let server = server(&dir, core);
        let (_, standing) = server.attach();
        server.lock().core.message("release/active", br#"{"release":"r1"}"#, true);
        clock.advance(60_000);

        let (_, newcomer) = server.attach();

        for reader in [standing, newcomer] {
            let delivery = reader.try_recv().unwrap();
            assert!(delivery.ends_with("[sub:s1] release/active release=r1"), "{delivery}");
        }
    }

    #[test_id(traucfi)]
    #[test]
    fn only_a_broker_that_held_this_subscriber_before_up_came_back_prunes() {
        let dir = TempDir::new("sub-kept");
        let mut core = Core::new(Arc::new(VirtualClock::new(1_000_000)), Saved::default());
        core.add_classed("release/active", "", false, None, true).unwrap();
        core.message("release/active", br#"{"release":"r1"}"#, true);
        let server = server(&dir, core);
        let mut state = server.lock();
        let digests = |state: &State| state.core.saved().subs[0].delivered.len();
        for (held_before, left) in [(false, 1), (true, 0)] {
            state.core.reconnected();
            (state.up, state.kept) = (Some("up".to_string()), false);
            if held_before {
                server.end_of_retained(&mut state, b"gone", true);
            }
            server.end_of_retained(&mut state, b"another up", false);
            assert_eq!(digests(&state), 1, "only this connection's up ends the burst");
            server.end_of_retained(&mut state, b"up", false);
            assert_eq!(digests(&state), left, "held before: {held_before}");
        }
    }

    // The set file's temp path is a FIFO and the set outgrows its buffer, so `apply` is held
    // inside the write until it is drained.
    #[test_id(tfvgq5y)]
    #[test]
    fn a_value_reaches_no_reader_before_the_set_file_is_written_with_it_as_given() {
        let dir = TempDir::new("sub-order");
        let mut core = Core::new(Arc::new(VirtualClock::new(1_000_000)), Saved::default());
        core.add_classed("release/active", &"n".repeat(1 << 20), false, None, true).unwrap();
        let server = server(&dir, core);
        let (_, reader) = server.attach();
        let temp = std::ffi::CString::new(format!("{}/.t.subs.tmp.{}", dir.display(), std::process::id())).unwrap();
        // SAFETY: a plain mkfifo(3) on a path this test just built.
        assert_eq!(unsafe { libc::mkfifo(temp.as_ptr(), 0o600) }, 0);

        let (early, written) = std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut state = server.lock();
                let out = state.core.message("release/active", br#"{"release":"r1"}"#, true);
                server.apply(&mut state, out);
            });
            // Opening returns once the writer has the other end.
            let (opened, opening) = std::sync::mpsc::channel();
            let path = temp.to_str().unwrap().to_string();
            std::thread::spawn(move || opened.send(std::fs::File::open(path)));
            let failsafe = Duration::from_secs(30);
            let mut pipe = opening.recv_timeout(failsafe).expect("apply never wrote the set file's temp path").unwrap();
            let early = reader.try_recv().ok();
            let mut written = String::new();
            pipe.read_to_string(&mut written).unwrap();
            (early.is_some(), written)
        });

        assert!(!early, "the line was queued while the set file did not yet hold it");
        assert!(written.contains(r#""delivered":{"release/active":"#), "the held write records the delivery");
        assert!(reader.try_recv().unwrap().starts_with("[sub:s1] release/active release=r1"));
    }
}

