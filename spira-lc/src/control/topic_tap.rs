//! The topic tap: a subscription to `#` on the broker that keeps one record per topic family ever
//! published, since the broker lists none and forgets chatter. It speaks MQTT 5 because a 3.1.1
//! subscription is handed every live publish with its retain flag cleared. It publishes nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rumqttc::v5::mqttbytes::v5::{Filter, Packet, Publish, RetainForwardRule, SubscribeReasonCode};
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{Client, ConnectionError, Event, MqttOptions};
use rumqttc::Transport;
use serde::{Deserialize, Serialize};
use spira_pub::link::{sleep_until_readable, LISTEN_PAUSE};
use spira_pub::{topics, SocketWatch, CONNECT_DEADLINE};
use state_store::{limits, Backend, Contract, Object, Request, Selector, Txn};

use super::store::Store;
use crate::control::keys;

/// The least time between two writes of the record, and between two tries of a broker or a store
/// that failed.
const FLUSH: Duration = Duration::from_secs(5);
/// Publisher names kept per family, of each kind.
const NAMES: usize = 16;
/// Rows one txn writes, leaving room for the request and events keys.
const CHUNK: usize = limits::MAX_TXN_OPS - 2;
/// A bypassing producer's payload has no cap of its own; the broker withholds a larger one.
const MAX_PACKET: u32 = 16 << 20;
const USER_PUBLISHER: &str = "spira-publisher";
const USER_RESIDENT: &str = "spira-resident";

/// What the store knows of one topic family.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct TopicRecord {
    family: String,
    /// The last publish's retain flag.
    retained: bool,
    /// Publishes both with and without the flag have been seen.
    mixed: bool,
    /// Milliseconds on the store's clock, as is `last_seen`.
    first_seen: u64,
    last_seen: u64,
    publishers: BTreeSet<String>,
    /// The names that said they live as long as what they publish.
    resident: BTreeSet<String>,
}

fn keep<'a>(names: &mut BTreeSet<String>, more: impl IntoIterator<Item = &'a str>) {
    for name in more {
        if names.len() < NAMES {
            names.insert(name.to_string());
        }
    }
}

impl TopicRecord {
    /// `self`, seen by this process, on top of what the store already held.
    fn after(self, stored: TopicRecord) -> TopicRecord {
        let mut all = stored;
        all.mixed |= self.mixed || all.retained != self.retained;
        all.retained = self.retained;
        all.last_seen = all.last_seen.max(self.last_seen);
        keep(&mut all.publishers, self.publishers.iter().map(String::as_str));
        keep(&mut all.resident, self.resident.iter().map(String::as_str));
        all
    }
}

#[derive(Default)]
struct Registry {
    rows: BTreeMap<String, TopicRecord>,
    /// The families whose row the store does not hold as it is here.
    changed: BTreeSet<String>,
    /// The stored rows are in `rows`. Nothing is written before, or a row would lose its past.
    loaded: bool,
}

impl Registry {
    fn saw(&mut self, topic: &str, retained: bool, publisher: Option<&str>, resident: bool, now_ms: u64) {
        let family = topics::family(topic);
        let row = self.rows.entry(family.clone()).or_insert_with(|| TopicRecord {
            family: family.clone(),
            retained,
            mixed: false,
            first_seen: now_ms,
            last_seen: now_ms,
            publishers: BTreeSet::new(),
            resident: BTreeSet::new(),
        });
        row.mixed |= row.retained != retained;
        row.retained = retained;
        row.last_seen = row.last_seen.max(now_ms);
        keep(&mut row.publishers, publisher);
        keep(&mut row.resident, publisher.filter(|_| resident));
        self.changed.insert(family);
    }

    fn load(&mut self, stored: Vec<TopicRecord>) {
        for old in stored {
            let row = match self.rows.remove(&old.family) {
                Some(seen) => seen.after(old),
                None => old,
            };
            self.rows.insert(row.family.clone(), row);
        }
        self.loaded = true;
    }

    fn take_changed(&mut self) -> Vec<TopicRecord> {
        std::mem::take(&mut self.changed).iter().filter_map(|f| self.rows.get(f).cloned()).collect()
    }
}

fn lock(registry: &Mutex<Registry>) -> MutexGuard<'_, Registry> {
    registry.lock().unwrap_or_else(|e| e.into_inner())
}

/// Loads the stored rows once, then writes every changed row in one txn (more only past the
/// store's txn limit). What could not be written stays changed. `writer` names this process.
fn flush<B: Backend>(registry: &Mutex<Registry>, contract: &Contract<B>, writer: &str) -> Result<(), String> {
    if !lock(registry).loaded {
        let (items, _) = contract.list(&[keys::TOPICS], &Selector::Everything).map_err(|e| format!("the record is unread: {e}"))?;
        let stored = items
            .into_iter()
            .map(|i| serde_json::from_value(i.value.data).map_err(|e| format!("the record is unread: {}: {e}", i.key)))
            .collect::<Result<_, String>>()?;
        lock(registry).load(stored);
    }
    let changed = lock(registry).take_changed();
    for (n, chunk) in changed.chunks(CHUNK).enumerate() {
        if let Err(e) = write(contract, chunk, writer) {
            lock(registry).changed.extend(changed[n * CHUNK..].iter().map(|r| r.family.clone()));
            return Err(format!("the record is unwritten: {e}"));
        }
    }
    Ok(())
}

fn write<B: Backend>(contract: &Contract<B>, rows: &[TopicRecord], writer: &str) -> Result<(), String> {
    let mut txn = Txn::new();
    for row in rows {
        txn = txn.put(&keys::topic(&row.family), Object::new(serde_json::to_value(row).map_err(|e| e.to_string())?));
    }
    // The head moves with every commit, so no id that has committed is ever sent again.
    let head = contract.head().map_err(|e| e.to_string())?;
    contract.txn(&txn, &Request::new(&format!("{writer}/{head}"), "lc-serve", "topic-tap")).map(drop).map_err(|e| e.to_string())
}

fn say(line: &str) {
    eprintln!("spira-lc serve: {line}");
}

/// Records what is published on the configured broker in `store`, from two threads of its own. A
/// failure is logged and tried again; the store is served whatever happens here.
pub(crate) fn serve<B: Backend + Send + Sync + 'static>(store: &Arc<Store<B>>) {
    let started = spira_pub::configured_socket(&spira_config::process::cfg)
        .map_err(|e| e.to_string())
        .and_then(|socket| start(store, socket.into()));
    if let Err(e) = started {
        say(&format!("topics are not recorded: {e}"));
    }
}

fn start<B: Backend + Send + Sync + 'static>(store: &Arc<Store<B>>, socket: PathBuf) -> Result<(), String> {
    let registry = Arc::new(Mutex::new(Registry::default()));
    // The broker drops the older of two clients sharing an id.
    let nanos = std::time::UNIX_EPOCH.elapsed().map_or(0, |d| d.as_nanos());
    let client_id = format!("spira-lc-topics-{}-{nanos:x}", std::process::id());
    let spawn = |name: &str, body: Box<dyn FnOnce() + Send>| {
        std::thread::Builder::new().name(name.into()).spawn(body).map(drop).map_err(|e| format!("cannot start {name}: {e}"))
    };
    let (flushed, flushing, writer) = (store.clone(), registry.clone(), client_id.clone());
    spawn(
        "lc-topic-flush",
        Box::new(move || loop {
            if let Err(e) = flush(&flushing, flushed.contract(), &writer) {
                say(&format!("topic tap: {e}"));
            }
            std::thread::sleep(FLUSH);
        }),
    )?;
    let clocked = store.clone();
    spawn(
        "lc-topic-tap",
        Box::new(move || {
            listen(&socket, &client_id, |p| {
                let user = p.properties.as_ref().map_or(&[][..], |p| &p.user_properties);
                let property = |name: &str| user.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
                let topic = String::from_utf8_lossy(&p.topic);
                let now = clocked.contract().wall_ms();
                lock(&registry).saw(&topic, p.retain, property(USER_PUBLISHER), property(USER_RESIDENT) == Some("true"), now);
            })
        }),
    )
}

/// Why a session ended.
struct Ended {
    why: String,
    /// It got as far as the broker's answer to the connect.
    connected: bool,
    refused: bool,
}

/// Holds a subscription for as long as there is a broker, waiting for the next one to make its
/// socket in between.
fn listen(socket: &Path, client_id: &str, mut heard: impl FnMut(&Publish)) {
    let mut watch = SocketWatch::new(socket);
    // Whether the broker's socket has just appeared, so a refusal is a not-yet.
    let mut announced = false;
    // An outage is logged when it starts, not at every try.
    let mut down = false;
    loop {
        let armed = watch.arm();
        watch.drain();
        // Real time, not the store's clock: this bounds socket I/O.
        let began = Instant::now();
        let ended = loop {
            let ended = session(socket, client_id, &mut heard);
            if !(announced && ended.refused && began.elapsed() < CONNECT_DEADLINE) {
                break ended;
            }
            std::thread::sleep(LISTEN_PAUSE);
        };
        if ended.connected {
            say(&format!("topic tap: lost the broker: {}", ended.why));
            // Once at once, in case only this connection went.
            (announced, down) = (false, false);
            continue;
        }
        if !std::mem::replace(&mut down, true) {
            let unwatched = armed.err().map(|e| format!(" ({e})")).unwrap_or_default();
            say(&format!("topic tap: cannot connect: {}{unwatched}", ended.why));
        }
        sleep_until_readable([watch.fd(), None], Some(FLUSH));
        announced = watch.drain();
    }
}

/// One connection, from the attempt to its end.
fn session(socket: &Path, client_id: &str, heard: &mut impl FnMut(&Publish)) -> Ended {
    let unmade = |why: String, refused: bool| Ended { why, connected: false, refused };
    let Some(path) = socket.to_str() else { return unmade(format!("socket path {} is not UTF-8", socket.display()), false) };
    let mut options = MqttOptions::new(client_id, path, 0);
    options.set_transport(Transport::Unix);
    options.set_connection_timeout(CONNECT_DEADLINE.as_secs());
    options.set_max_packet_size(Some(MAX_PACKET));
    let (client, mut connection) = Client::new(options, 8);
    match connection.recv_timeout(CONNECT_DEADLINE) {
        Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) => {}
        Ok(Err(e)) => {
            let refused = matches!(&e, ConnectionError::Io(io) if matches!(io.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound));
            return unmade(e.to_string(), refused);
        }
        other => return unmade(format!("no CONNACK: {other:?}"), false),
    }
    let mut everything = Filter::new("#", QoS::AtMostOnce);
    everything.preserve_retain = true;
    // A value the broker replays at subscribe is not a publish.
    everything.retain_forward_rule = RetainForwardRule::Never;
    let ended = |why: String| Ended { why, connected: true, refused: false };
    if let Err(e) = client.try_subscribe_many([everything]) {
        return unmade(format!("cannot subscribe: {e}"), false);
    }
    loop {
        match connection.recv() {
            Ok(Ok(Event::Incoming(Packet::Publish(p)))) => heard(&p),
            Ok(Ok(Event::Incoming(Packet::SubAck(ack)))) => {
                if !ack.return_codes.iter().all(|c| matches!(c, SubscribeReasonCode::Success(_))) {
                    return unmade(format!("the broker refused the subscription: {:?}", ack.return_codes), false);
                }
                say("topic tap subscribed");
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return ended(e.to_string()),
            Err(_) => return ended("the client closed".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use runtime::ports::VirtualClock;
    use state_store::{Config, MemBackend, Ports};

    use super::*;

    fn contract() -> Contract<MemBackend> {
        Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap()
    }

    fn stored(contract: &Contract<MemBackend>, family: &str) -> TopicRecord {
        serde_json::from_value(contract.get(&keys::topic(family)).unwrap().unwrap().value.data).unwrap()
    }

    #[test_id(t0bllvw)]
    #[test]
    fn a_row_seen_before_the_store_was_read_keeps_the_past_the_store_holds() {
        let contract = contract();
        let before = Mutex::new(Registry::default());
        lock(&before).saw("a/sp-1", true, Some("old"), false, 100);
        flush(&before, &contract, "first").unwrap();
        let mut again = Registry::default();
        again.saw("a/sp-2", false, Some("new"), true, 500);
        flush(&Mutex::new(again), &contract, "second").unwrap();
        let got = stored(&contract, "a/+");
        assert_eq!((got.retained, got.mixed, got.first_seen, got.last_seen), (false, true, 100, 500));
        assert_eq!(got.publishers, BTreeSet::from(["new".to_string(), "old".to_string()]));
        assert_eq!(got.resident, BTreeSet::from(["new".to_string()]));
    }

    #[test_id(tnnwutw)]
    #[test]
    fn any_number_of_publishes_cost_one_write_a_flush_and_a_quiet_flush_writes_nothing() {
        let contract = contract();
        let registry = Mutex::new(Registry::default());
        for n in 0..100 {
            lock(&registry).saw(&format!("a/sp-{n}"), true, None, false, n);
            lock(&registry).saw(&format!("b/sp-{n}"), false, None, false, n);
        }
        let head = contract.head().unwrap();
        flush(&registry, &contract, "w").unwrap();
        assert_eq!(contract.head().unwrap(), head + 1);
        flush(&registry, &contract, "w").unwrap();
        assert_eq!(contract.head().unwrap(), head + 1);
    }

    #[test_id(tnkz8j9)]
    #[test]
    fn more_changed_families_than_one_txn_holds_are_all_written() {
        let contract = contract();
        let registry = Mutex::new(Registry::default());
        // Letters only, so each is a family of its own.
        let letter = |n: usize| char::from(b'a' + (n % 26) as u8);
        for n in 0..CHUNK * 2 + 1 {
            lock(&registry).saw(&format!("f/{}{}", letter(n / 26), letter(n)), true, None, false, 1);
        }
        flush(&registry, &contract, "w").unwrap();
        assert_eq!(contract.list(&[keys::TOPICS], &Selector::Everything).unwrap().0.len(), CHUNK * 2 + 1);
    }

    #[test_id(t2nybxa)]
    #[test]
    fn a_family_keeps_at_most_sixteen_publisher_names_sorted() {
        let mut registry = Registry::default();
        for n in (0..NAMES + 4).rev() {
            registry.saw("a/sp-1", true, Some(&format!("p{n:02}")), true, 1);
        }
        let row = &registry.rows["a/+"];
        assert_eq!((row.publishers.len(), row.resident.len()), (NAMES, NAMES));
        assert_eq!(row.publishers.iter().next().map(String::as_str), Some("p04"));
    }
}
