//! One publisher, one connection, one thread that owns it.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use ports::{Clock, Rng, SeededRng, VirtualClock, WallClock};
use rumqttc::{Client, Connection, Event, Outgoing, Packet, QoS};
use serde_json::{json, Value};
use spira_otel::metrics::MetricsSink;

use crate::link::{gone, sleep_until_readable, Dial};
use crate::socket_watch::SocketWatch;
use crate::telemetry::{Sample, Telemetry};
use crate::topics::{self, Class};
use crate::PubError;

/// The most a publish waits for a connection.
pub const CONNECT_DEADLINE: Duration = Duration::from_secs(1);
/// The most dropping a publisher waits to deliver what it has queued.
const FLUSH_DEADLINE: Duration = Duration::from_secs(1);
/// The most messages a publisher holds that the broker has not taken.
const QUEUE: usize = 1024;
/// The client's channel: the queue, plus room for everything an `on_connect` hook republishes.
const CHANNEL: usize = 1 << 16;
pub const MAX_PAYLOAD: usize = 256 << 10;

pub type Log = Arc<dyn Fn(&str) + Send + Sync>;
type Hook = Arc<dyn Fn(&mut Republish) + Send + Sync>;

/// How a publisher is built when the caller supplies what [`Publisher::new`] reads from the
/// configuration.
pub struct Options {
    pub socket: PathBuf,
    pub clock: Arc<dyn Clock>,
    pub rng: Box<dyn Rng>,
    pub log: Log,
    pub queue: usize,
    pub metrics: Option<MetricsSink>,
}

impl Options {
    pub fn new(socket: impl Into<PathBuf>, clock: Arc<dyn Clock>, rng: Box<dyn Rng>) -> Options {
        Options { socket: socket.into(), clock, rng, log: Arc::new(|line| eprintln!("{line}")), queue: QUEUE, metrics: None }
    }
}

struct Message {
    topic: String,
    class: Class,
    payload: Vec<u8>,
}

fn message(topic: &str, class: Option<Class>, payload: &Value) -> Result<Message, PubError> {
    let family = topics::check(topic, class)?;
    sized(topic, family.class, payload.to_string().into_bytes())
}

fn sized(topic: &str, class: Class, payload: Vec<u8>) -> Result<Message, PubError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(PubError::TooLarge { topic: topic.to_string(), bytes: payload.len() });
    }
    Ok(Message { topic: topic.to_string(), class, payload })
}

/// The zero-byte message that makes the broker forget a state topic's retained value.
fn clearing(topic: &str) -> Result<Message, PubError> {
    let family = topics::check(topic, Some(Class::State))?;
    sized(topic, family.class, Vec::new())
}

/// What an `on_connect` hook publishes: sent right after `publisher/<name>` = up, ahead of
/// anything queued later.
#[derive(Default)]
pub struct Republish(Vec<Message>);

impl Republish {
    pub fn publish(&mut self, topic: &str, payload: &Value) -> Result<(), PubError> {
        self.0.push(message(topic, None, payload)?);
        Ok(())
    }
}

enum Phase {
    Down,
    Connecting,
    Up(Client),
}

struct Shared {
    phase: Phase,
    /// A connection has been asked for, so a broker that comes back is connected to.
    wanted: bool,
    requested: bool,
    closing: bool,
    stopped: bool,
    /// Connect attempts that have ended, either way.
    attempts: u64,
    failure: String,
    /// Messages handed to the client that the broker has not taken yet.
    pending: usize,
    /// Messages dropped with a lost connection, until a flush reports them.
    lost: usize,
}

impl Shared {
    fn up(&self) -> bool {
        matches!(self.phase, Phase::Up(_))
    }
}

struct Inner {
    name: String,
    client_id: String,
    socket: PathBuf,
    queue: usize,
    clock: Arc<dyn Clock>,
    log: Log,
    shared: Mutex<Shared>,
    changed: Condvar,
    hook: Mutex<Option<Hook>>,
    telemetry: Telemetry,
}

pub struct Publisher {
    inner: Arc<Inner>,
    wake: UnixStream,
    worker: Option<JoinHandle<()>>,
}

/// `SPIRA_BROKER_SOCKET` as `cfg` declares it; refused, naming the key, when it does not.
fn configured_socket(cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<String, PubError> {
    use spira_config::broker::{socket_path, SOCKET_KEY};
    let refuse = |why: String| PubError::Config(format!("cannot find the broker ({SOCKET_KEY}): {why}"));
    cfg(SOCKET_KEY).map_err(refuse)?;
    let run = cfg("SPIRA_RUN").map_err(refuse)?;
    socket_path(&run, cfg).map_err(refuse)
}

/// `SPIRA_SEED` as `cfg` declares it; declared empty draws from the clock.
fn configured_seed(cfg: &dyn Fn(&str) -> Result<String, String>, now_ms: u64) -> Result<u64, PubError> {
    let value = cfg("SPIRA_SEED").map_err(|why| PubError::Config(format!("cannot read SPIRA_SEED: {why}")))?;
    match value.trim() {
        "" => Ok(now_ms),
        seed => seed.parse().map_err(|e| PubError::Config(format!("SPIRA_SEED {seed:?} is not a number: {e}"))),
    }
}

impl Publisher {
    /// The publisher called `name`, on the broker the configuration names. Nothing is connected
    /// until the first [`publish`](Publisher::publish) or [`connect`](Publisher::connect).
    pub fn new(name: &str) -> Result<Publisher, PubError> {
        Publisher::from_config(name, &spira_config::process::cfg)
    }

    /// [`new`](Publisher::new) for a caller that already holds its resolved configuration.
    pub fn from_config(name: &str, cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<Publisher, PubError> {
        let socket = configured_socket(cfg)?;
        let clock: Arc<dyn Clock> = match spira_config::vtime::override_epoch() {
            Some(secs) => Arc::new(VirtualClock::new(secs.saturating_mul(1000))),
            None => Arc::new(WallClock),
        };
        let seed = configured_seed(cfg, clock.now_ms())?;
        let mut options = Options::new(socket, clock, Box::new(SeededRng::new(seed)));
        // Metrics are not worth refusing a publish over: an unreadable endpoint is logged.
        let endpoint = cfg("SPIRA_METRICS_ENDPOINT")
            .inspect_err(|why| (options.log)(&format!("spira-pub[{name}]: no metrics: {why}")))
            .ok();
        options.metrics = Some(MetricsSink::for_component(endpoint.as_deref(), "spira-pub"));
        Publisher::with(name, options)
    }

    pub fn with(name: &str, mut options: Options) -> Result<Publisher, PubError> {
        if name.is_empty() || name.contains(['/', '+', '#', '\0']) {
            return Err(PubError::BadName(name.to_string()));
        }
        // The broker drops the older of two clients sharing an id, publishing its will: two
        // processes publishing under one name must never share one.
        let client_id = format!("spira-pub-{name}-{}-{:016x}", std::process::id(), options.rng.next_u64());
        let telemetry = Telemetry::start(options.metrics, name);
        let inner = Arc::new(Inner {
            name: name.to_string(),
            client_id,
            socket: options.socket,
            queue: options.queue,
            clock: options.clock,
            log: options.log,
            shared: Mutex::new(Shared {
                phase: Phase::Down,
                wanted: false,
                requested: false,
                closing: false,
                stopped: false,
                attempts: 0,
                failure: String::new(),
                pending: 0,
                lost: 0,
            }),
            changed: Condvar::new(),
            hook: Mutex::new(None),
            telemetry,
        });
        let start = |e: std::io::Error| PubError::Unreachable { why: format!("cannot start the client thread: {e}") };
        let (wake, woken) = UnixStream::pair().map_err(start)?;
        wake.set_nonblocking(true).map_err(start)?;
        woken.set_nonblocking(true).map_err(start)?;
        let on_thread = inner.clone();
        let worker = std::thread::Builder::new().name("spira-pub".into()).spawn(move || on_thread.run(woken)).map_err(start)?;
        Ok(Publisher { inner, wake, worker: Some(worker) })
    }

    /// Runs `hook` on every connect, so this publisher's current state survives a broker that
    /// keeps nothing. Set it before the first publish. It runs on the client thread while
    /// publishers wait: it must not take a lock a caller holds across a publish.
    pub fn on_connect(&self, hook: impl Fn(&mut Republish) + Send + Sync + 'static) {
        *lock(&self.inner.hook) = Some(Arc::new(hook));
    }

    /// Connects now rather than at the first publish: a long-lived publisher's way to be seen
    /// as up before it has anything to say.
    pub fn connect(&self) -> Result<(), PubError> {
        let connected = self.connected().map(drop);
        connected.inspect_err(|e| self.inner.failed(e, "connect"))
    }

    /// Queues `payload` for `topic`, retained when its family is state. `Err` means it will
    /// not arrive; it is already logged.
    pub fn publish(&self, topic: &str, payload: &Value) -> Result<(), PubError> {
        self.send(topic, message(topic, None, payload))
    }

    /// Queues the removal of state `topic`'s retained value: a later subscriber is handed
    /// nothing for it. Refused for chatter, which retains nothing. Fails and is logged as
    /// [`publish`](Publisher::publish) does.
    pub fn clear(&self, topic: &str) -> Result<(), PubError> {
        self.send(topic, clearing(topic))
    }

    /// [`publish`](Publisher::publish) for a caller that states the class it means: refused
    /// unless the topic's family declares it.
    pub fn publish_as_class(&self, class: Class, topic: &str, payload: &Value) -> Result<(), PubError> {
        self.send(topic, message(topic, Some(class), payload))
    }

    fn send(&self, topic: &str, built: Result<Message, PubError>) -> Result<(), PubError> {
        let sent = built.and_then(|m| {
            let mut shared = self.connected()?;
            if shared.pending >= self.inner.queue {
                return Err(PubError::QueueFull { topic: m.topic });
            }
            enqueue(&mut shared, m)?;
            Ok(shared.pending)
        });
        match sent {
            Ok(depth) => {
                self.inner.telemetry.record(Sample::Published { depth });
                Ok(())
            }
            Err(e) => {
                self.inner.failed(&e, topic);
                Err(e)
            }
        }
    }

    /// The shared state with the connection up, after at most one attempt to bring it up.
    fn connected(&self) -> Result<MutexGuard<'_, Shared>, PubError> {
        let mut shared = lock(&self.inner.shared);
        if shared.up() {
            return Ok(shared);
        }
        let before = shared.attempts;
        shared.wanted = true;
        if matches!(shared.phase, Phase::Down) {
            shared.requested = true;
            self.wake_worker();
        }
        let shared = self.inner.wait(shared, CONNECT_DEADLINE, |s| !s.up() && s.attempts == before);
        if shared.up() {
            Ok(shared)
        } else if shared.attempts != before {
            Err(PubError::Unreachable { why: shared.failure.clone() })
        } else {
            Err(PubError::Timeout)
        }
    }

    /// Waits until the broker has taken everything queued. `Err` when something never will, or
    /// has not within `deadline`. Dropping a publisher flushes it.
    pub fn flush(&self, deadline: Duration) -> Result<(), PubError> {
        let shared = lock(&self.inner.shared);
        let mut shared = self.inner.wait(shared, deadline, |s| s.up() && s.pending > 0);
        match std::mem::take(&mut shared.lost) {
            0 if shared.pending == 0 => Ok(()),
            0 => Err(PubError::Timeout),
            lost => Err(PubError::Undelivered(lost)),
        }
    }

    /// Waits for the client to see its connection gone; false if it is still up at `deadline`.
    pub fn wait_disconnected(&self, deadline: Duration) -> bool {
        let shared = lock(&self.inner.shared);
        !self.inner.wait(shared, deadline, Shared::up).up()
    }

    fn wake_worker(&self) {
        // A full pipe already holds a wake-up.
        let _ = (&self.wake).write(&[1]);
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        let topic = format!("publisher/{}", self.inner.name);
        let goodbye = {
            let mut shared = lock(&self.inner.shared);
            // A clean DISCONNECT discards the will, so this says what it would have.
            shared.up().then(|| message(&topic, None, &gone()).and_then(|m| enqueue(&mut shared, m)))
        };
        if goodbye.is_some() {
            if let Err(e) = self.flush(FLUSH_DEADLINE) {
                self.inner.failed(&e, "flush");
            }
        }
        {
            let mut shared = lock(&self.inner.shared);
            shared.closing = true;
            if let (Phase::Up(client), 0) = (&shared.phase, shared.pending) {
                let _ = client.try_disconnect();
            }
        }
        self.wake_worker();
        let shared = lock(&self.inner.shared);
        // A thread still on a connection that took nothing is left behind: its socket closes
        // with the process, and the broker's will says gone.
        let stopped = self.inner.wait(shared, FLUSH_DEADLINE, |s| !s.stopped).stopped;
        if let (true, Some(worker)) = (stopped, self.worker.take()) {
            let _ = worker.join();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Hands `m` to the client without blocking. The lock is held, so the order of these calls is
/// the order the broker sees.
fn enqueue(shared: &mut Shared, m: Message) -> Result<(), PubError> {
    let Phase::Up(client) = &shared.phase else {
        return Err(PubError::Unreachable { why: shared.failure.clone() });
    };
    let (qos, retain) = delivery(m.class);
    let topic = m.topic.clone();
    client.try_publish(m.topic, qos, retain, m.payload).map_err(|_| PubError::QueueFull { topic })?;
    shared.pending += 1;
    Ok(())
}

/// A state message is acknowledged by the broker; chatter is sent once and forgotten.
fn delivery(class: Class) -> (QoS, bool) {
    match class {
        Class::State => (QoS::AtLeastOnce, true),
        Class::Chatter => (QoS::AtMostOnce, false),
    }
}

impl Inner {
    fn wait<'a>(&self, shared: MutexGuard<'a, Shared>, deadline: Duration, mut still: impl FnMut(&Shared) -> bool) -> MutexGuard<'a, Shared> {
        self.changed.wait_timeout_while(shared, deadline, |s| still(s)).unwrap_or_else(|e| e.into_inner()).0
    }

    fn say(&self, line: &str) {
        (self.log)(&format!("spira-pub[{}]: {line}", self.name));
    }

    fn failed(&self, e: &PubError, what: &str) {
        self.say(&format!("publish failed ({what}): {e}"));
        self.telemetry.record(Sample::Failed { reason: e.reason(), count: 1 });
    }

    /// The client thread: asleep until a caller asks for a connection or the broker makes its
    /// socket, and inside the connection while there is one.
    fn run(&self, woken: UnixStream) {
        let mut watch = SocketWatch::new(&self.socket);
        loop {
            let (closing, requested, wanted) = {
                let mut s = lock(&self.shared);
                (s.closing, std::mem::take(&mut s.requested), s.wanted)
            };
            if closing {
                break;
            }
            if requested {
                self.session(&mut watch, false);
            } else if watch.drain() && wanted {
                self.session(&mut watch, true);
            } else {
                sleep_until_readable([Some(woken.as_raw_fd()), watch.fd()], None);
                let mut buf = [0u8; 64];
                while matches!((&woken).read(&mut buf), Ok(n) if n > 0) {}
            }
        }
        lock(&self.shared).stopped = true;
        self.changed.notify_all();
    }

    /// One connection, from the attempt to its end. `announced` says the broker's socket has
    /// just appeared, so a refusal is waited out until the deadline.
    fn session(&self, watch: &mut SocketWatch, announced: bool) {
        lock(&self.shared).phase = Phase::Connecting;
        if let Err(why) = watch.arm() {
            self.say(&format!("{why}; a restarted broker is reconnected to at the next publish"));
        }
        watch.drain();
        let dial = Dial { client_id: &self.client_id, socket: &self.socket, will_topic: format!("publisher/{}", self.name), capacity: CHANNEL };
        let (client, mut connection) = match dial.connect(announced) {
            Ok(open) => open,
            Err(lost) => {
                if announced {
                    self.say(&format!("reconnect failed: {}", lost.why));
                }
                let mut s = lock(&self.shared);
                s.phase = Phase::Down;
                s.failure = lost.why;
                s.attempts += 1;
                self.changed.notify_all();
                return;
            }
        };
        let greeting = self.greeting();
        {
            let mut s = lock(&self.shared);
            s.attempts += 1;
            if s.closing {
                s.phase = Phase::Down;
                return;
            }
            s.phase = Phase::Up(client);
            for m in greeting {
                if let Err(e) = enqueue(&mut s, m) {
                    self.failed(&e, "on connect");
                }
            }
        }
        self.changed.notify_all();
        let why = self.serve(&mut connection);
        let mut s = lock(&self.shared);
        s.phase = Phase::Down;
        let dropped = std::mem::take(&mut s.pending);
        s.lost += dropped;
        let closing = s.closing;
        s.failure = why.clone();
        drop(s);
        if !closing {
            self.say(&format!("lost the broker with {dropped} message(s) undelivered: {why}"));
        }
        if dropped > 0 {
            self.telemetry.record(Sample::Failed { reason: PubError::Undelivered(dropped).reason(), count: dropped });
        }
        self.changed.notify_all();
    }

    /// `publisher/<name>` = up, then whatever the hook republishes.
    fn greeting(&self) -> Vec<Message> {
        let mut out = Republish::default();
        let up = json!({"state": "up", "since_ms": self.clock.now_ms()});
        if let Err(e) = out.publish(&format!("publisher/{}", self.name), &up) {
            self.failed(&e, "on connect");
        }
        let hook = lock(&self.hook).clone();
        if let Some(hook) = hook {
            let mut again = Republish::default();
            match catch_unwind(AssertUnwindSafe(|| hook(&mut again))) {
                Ok(()) => out.0.append(&mut again.0),
                Err(_) => self.say("the on_connect hook panicked; nothing of it was republished"),
            }
        }
        out.0
    }

    /// Drives the connection until it ends, and says why.
    fn serve(&self, connection: &mut Connection) -> String {
        loop {
            match connection.recv() {
                Ok(Ok(Event::Incoming(Packet::PubAck(_)) | Event::Outgoing(Outgoing::Publish(0)))) => {
                    let mut s = lock(&self.shared);
                    s.pending = s.pending.saturating_sub(1);
                    drop(s);
                    self.changed.notify_all();
                }
                Ok(Ok(Event::Outgoing(Outgoing::Disconnect))) => return "disconnected".to_string(),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return e.to_string(),
                Err(_) => return "the client closed".to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tbbt35e)]
    #[test]
    fn a_config_that_does_not_name_the_broker_is_refused_naming_the_key() {
        let why = configured_socket(&|k| Err(format!("{k} is not declared"))).unwrap_err();
        assert!(matches!(why, PubError::Config(_)) && why.to_string().contains("SPIRA_BROKER_SOCKET"), "{why}");
        let empty = configured_socket(&|k| Ok(if k == "SPIRA_RUN" { "run".into() } else { String::new() })).unwrap_err();
        assert!(empty.to_string().contains("SPIRA_BROKER_SOCKET"), "{empty}");
        let found = configured_socket(&|k| Ok(if k == "SPIRA_RUN" { "run".into() } else { "b.sock".into() }));
        assert_eq!(found.unwrap(), "run/b.sock");
    }

    #[test_id(tmq7ost)]
    #[test]
    fn only_a_declared_empty_seed_is_the_clocks_and_a_bad_one_is_refused_by_name() {
        let seed = |v: &'static str| configured_seed(&|_| Ok(v.to_string()), 99);
        assert_eq!(seed("").unwrap(), 99);
        assert_eq!(seed("7").unwrap(), 7);
        let bad = seed("seven").unwrap_err().to_string();
        assert!(bad.contains("SPIRA_SEED") && bad.contains("seven"), "{bad}");
        let refused = configured_seed(&|k| Err(format!("{k} is not declared")), 99).unwrap_err().to_string();
        assert!(refused.contains("SPIRA_SEED is not declared"), "{refused}");
    }

    #[test_id(tqbh325)]
    #[test]
    fn the_class_sets_retain() {
        assert_eq!(delivery(Class::State), (QoS::AtLeastOnce, true));
        assert_eq!(delivery(Class::Chatter), (QoS::AtMostOnce, false));
    }
}
