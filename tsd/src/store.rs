// store — families held in the metrics store instead of a file. A row's numeric fields are
// `spira_tsd_value{tsd_family,tsd_field,..}` gauges, its string and bool fields their labels, its
// `ts` the sample time; a row with no number is one `spira_tsd_row` sample.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Number, Value};
use spira_otel::metrics::{MetricsSink, Series};
use spira_otel::query::{Exported, QueryError, Store};

/// Families whose every reader is an alarm, a report or an ops view. A family with a reader on
/// the landing path (gate, landing-pass, batcher, batcher-cut, suite-select, queue) is not
/// listed and stays a file.
pub const MOVED: &[&str] = &[
    "sentinel-phase",
    "bead-stage",
    "dwarf-session",
    "reconciler-status",
    "reconciler-remedy",
    "unit-run-dbus",
    "vm-signals",
    "landing-event",
];

pub const VALUE: &str = "spira_tsd_value";
pub const ROW: &str = "spira_tsd_row";
pub const SERVICE: &str = "spira-tsd";
const LABEL_MAX: usize = 1000;
const ENVELOPE: [&str; 2] = ["ts", "family"];
const OWN_LABELS: [&str; 3] = ["__name__", "tsd_family", "tsd_field"];
/// What the store adds to a pushed series on its own.
const STORE_LABELS: [&str; 1] = ["service.name"];

pub fn moved(family: &str) -> bool {
    MOVED.contains(&family)
}

fn label_name(k: &str) -> String {
    k.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

/// The samples of one row (`ts` is stamped by the caller as `at`).
pub fn series_of(family: &str, row: &Map<String, Value>, at: SystemTime) -> Vec<Series> {
    let mut labels: Vec<(String, String)> = vec![("tsd_family".into(), family.into())];
    let mut numbers: Vec<(String, f64)> = Vec::new();
    for (k, v) in row {
        if ENVELOPE.contains(&k.as_str()) || OWN_LABELS.contains(&k.as_str()) {
            continue;
        }
        let text = match v {
            Value::Null => continue,
            Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    numbers.push((label_name(k), f));
                }
                continue;
            }
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            other => other.to_string(),
        };
        if text.is_empty() {
            continue;
        }
        labels.push((label_name(k), text.chars().take(LABEL_MAX).collect()));
    }
    if numbers.is_empty() {
        return vec![Series { name: ROW.into(), value: 1.0, labels, at }];
    }
    numbers
        .into_iter()
        .map(|(field, value)| {
            let mut l = labels.clone();
            l.push(("tsd_field".into(), field));
            Series { name: VALUE.into(), value, labels: l, at }
        })
        .collect()
}

/// The millisecond part of a row's sample time: a hash of the whole row, so two different rows
/// the producer stamped in the same second do not overwrite one another, and the same row
/// pushed twice (a retry, a re-merge) lands on the same sample.
fn millis_of(line: &str) -> u32 {
    let h = line.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    (h % 1000) as u32
}

/// Pushes one built row (`tsd::build_row`'s line) to the store.
pub fn push_row(endpoint: Option<&str>, family: &str, line: &str) -> Result<(), String> {
    push_rows(endpoint, family, &[line])
}

/// [`push_row`] for many rows, a request per [`BATCH_SERIES`] samples. All or nothing is not
/// promised: the first refused request is the error, and the rows before it are stored.
pub fn push_rows<L: AsRef<str>>(endpoint: Option<&str>, family: &str, lines: &[L]) -> Result<(), String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let mut series: Vec<Series> = Vec::new();
    let sink = MetricsSink::new(endpoint, SERVICE, Duration::from_secs(2));
    let send = |series: &mut Vec<Series>| -> Result<(), String> {
        let r = sink.try_push_series(series).map_err(|e| format!("{family}: row not stored: {e}"));
        series.clear();
        r
    };
    for line in lines {
        let line = line.as_ref();
        let Value::Object(row) = serde_json::from_str::<Value>(line).map_err(|e| format!("row is not valid JSON: {e}"))? else {
            return Err("row is not a JSON object".into());
        };
        let secs = row.get("ts").and_then(Value::as_str).and_then(crate::parse_ts).unwrap_or(now.as_secs());
        series.extend(series_of(family, &row, UNIX_EPOCH + Duration::new(secs, millis_of(line) * 1_000_000)));
        if series.len() >= BATCH_SERIES {
            send(&mut series)?;
        }
    }
    if series.is_empty() {
        return Ok(());
    }
    send(&mut series)
}

pub const BATCH_SERIES: usize = 2000;

/// The store's address: `SPIRA_METRICS_ENDPOINT` when the caller exported it, else the
/// declared `spira.metrics_endpoint`. `tsd` cannot link `spira-config` (which links `tsd`), so the
/// declaration is read through its CLI. Empty or unreadable is `None`, and `None` is a refusal.
pub fn endpoint() -> Option<String> {
    let from_env = std::env::var("SPIRA_METRICS_ENDPOINT").ok().filter(|v| !v.trim().is_empty());
    from_env.or_else(|| {
        let out = std::process::Command::new("timeout").args(["3", "spira-config", "get", "spira.metrics_endpoint"]).stdin(std::process::Stdio::null()).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|v| out.status.success() && !v.is_empty())
    })
}

/// Pushes rows for a long-running or many-rows-per-pass caller. After a failed push it refuses
/// the next ones for [`SUSPEND_SECS`] with the same error, so a store that stopped answering
/// costs one deadline, not one per row.
pub struct Pusher {
    endpoint: Option<String>,
    down_until: std::sync::atomic::AtomicU64,
    suspend_secs: u64,
}

pub const SUSPEND_SECS: u64 = 30;

impl Pusher {
    pub fn new(endpoint: Option<String>) -> Pusher {
        Pusher { endpoint, down_until: std::sync::atomic::AtomicU64::new(0), suspend_secs: SUSPEND_SECS }
    }

    pub fn suspending_for(mut self, secs: u64) -> Pusher {
        self.suspend_secs = secs;
        self
    }

    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    pub fn push(&self, family: &str, ts: &str, fields: &[(String, Value)]) -> Result<(), String> {
        use std::sync::atomic::Ordering::Relaxed;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if now < self.down_until.load(Relaxed) {
            return Err(format!("{family}: row not stored: the store failed a push in the last {}s", self.suspend_secs));
        }
        let line = crate::build_row(ts, &crate::default_host(), family, fields)?;
        push_row(self.endpoint.as_deref(), family, &line).inspect_err(|_| self.down_until.store(now + self.suspend_secs, Relaxed))
    }
}

pub struct Reader {
    store: Store,
}

impl Reader {
    pub fn new(endpoint: Option<&str>) -> Result<Reader, QueryError> {
        Ok(Reader { store: Store::new(endpoint)? })
    }

    /// Rows of `family` stamped at or after `since` (unix seconds), oldest first. A family with
    /// no row in the window is [`QueryError::NoSeries`]: the caller asks for a window that must
    /// have data, and a store that cannot show it is not an all-clear.
    pub fn rows(&self, family: &str, since: u64) -> Result<Vec<Value>, QueryError> {
        let rows = self.rows_or_empty(family, since)?;
        if rows.is_empty() {
            return Err(QueryError::NoSeries(format!("no {family} row since {}", crate::iso_utc(since))));
        }
        Ok(rows)
    }

    /// [`Reader::rows`] for a window that may legitimately hold nothing; the store was still
    /// reached, so a refusal elsewhere is still a refusal.
    pub fn rows_or_empty(&self, family: &str, since: u64) -> Result<Vec<Value>, QueryError> {
        Ok(rebuild(family, &self.store.export(&family_selector(family)?, since)?))
    }

    /// Rows of `family` stamped in `[since, until]`; a window may hold nothing.
    pub fn rows_between(&self, family: &str, since: u64, until: u64) -> Result<Vec<Value>, QueryError> {
        Ok(rebuild(family, &self.store.export_between(&family_selector(family)?, since, until)?))
    }
}

fn family_selector(family: &str) -> Result<String, QueryError> {
    if !crate::valid_family(family) {
        return Err(QueryError::Malformed(format!("bad family {family:?}")));
    }
    Ok(format!("{{__name__=~\"{VALUE}|{ROW}\",tsd_family=\"{family}\"}}"))
}

/// Rows from exported series: samples sharing a label set and a timestamp are one row.
pub fn rebuild(family: &str, series: &[Exported]) -> Vec<Value> {
    let mut sets: Vec<Vec<(String, String)>> = Vec::new();
    let mut ids: std::collections::HashMap<Vec<(String, String)>, usize> = Default::default();
    let mut rows: BTreeMap<(i64, usize), Map<String, Value>> = BTreeMap::new();
    for s in series {
        let name = s.labels.get("__name__").map(String::as_str).unwrap_or("");
        if name != VALUE && name != ROW {
            continue;
        }
        let row_labels: Vec<(String, String)> = s
            .labels
            .iter()
            .filter(|(k, _)| !OWN_LABELS.contains(&k.as_str()) && !STORE_LABELS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let id = *ids.entry(row_labels.clone()).or_insert_with(|| {
            sets.push(row_labels);
            sets.len() - 1
        });
        let field = (name == VALUE).then(|| s.labels.get("tsd_field")).flatten();
        for (ms, v) in &s.samples {
            let row = rows.entry((*ms, id)).or_insert_with(|| {
                let mut m = Map::new();
                m.insert("ts".into(), Value::String(crate::iso_utc((*ms).max(0) as u64 / 1000)));
                m.insert("family".into(), Value::String(family.into()));
                for (k, text) in &sets[id] {
                    m.insert(k.clone(), typed(text));
                }
                m
            });
            if let Some(field) = field {
                row.insert(field.clone(), number(*v));
            }
        }
    }
    let mut keyed: Vec<((i64, f64), Value)> = rows
        .into_iter()
        .map(|((ms, _), row)| {
            let seq = row.get("seq").and_then(Value::as_f64).unwrap_or(0.0);
            ((ms.div_euclid(1000), seq), Value::Object(row))
        })
        .collect();
    keyed.sort_by(|a, b| a.0 .0.cmp(&b.0 .0).then(a.0 .1.partial_cmp(&b.0 .1).unwrap_or(std::cmp::Ordering::Equal)));
    keyed.into_iter().map(|(_, v)| v).collect()
}

fn typed(text: &str) -> Value {
    match text {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(text.into()),
    }
}

fn number(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        Value::Number(Number::from(v as i64))
    } else {
        Number::from_f64(v).map(Value::Number).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export_of(series: Vec<Series>) -> Vec<Exported> {
        series
            .into_iter()
            .map(|s| {
                let mut labels: BTreeMap<String, String> = s.labels.into_iter().collect();
                labels.insert("__name__".into(), s.name);
                labels.insert("service.name".into(), SERVICE.into());
                let ms = s.at.duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
                Exported { labels, samples: vec![(ms, s.value)] }
            })
            .collect()
    }

    #[test]
    fn a_row_survives_the_trip_with_its_types() {
        let line = crate::build_row(
            "2026-10-22T10:00:00Z",
            "h1",
            "bead-stage",
            &[
                crate::parse_field_str("key=sp-a").unwrap(),
                crate::parse_field("seq=7").unwrap(),
                crate::parse_field("applied=true").unwrap(),
                crate::parse_field("secs=1.5").unwrap(),
                crate::parse_field("gone=null").unwrap(),
            ],
        )
        .unwrap();
        let Value::Object(row) = serde_json::from_str(&line).unwrap() else { panic!() };
        let at = UNIX_EPOCH + Duration::from_secs(1_792_000_000);
        let back = rebuild("bead-stage", &export_of(series_of("bead-stage", &row, at)));
        assert_eq!(back.len(), 1, "{back:?}");
        let r = &back[0];
        assert_eq!((r["key"].as_str(), r["host"].as_str(), r["family"].as_str()), (Some("sp-a"), Some("h1"), Some("bead-stage")));
        assert_eq!((r["seq"].as_i64(), r["applied"].as_bool(), r["secs"].as_f64()), (Some(7), Some(true), Some(1.5)));
        assert!(r.get("gone").is_none());
        assert_eq!(r["ts"], crate::iso_utc(1_792_000_000));
    }

    #[test]
    fn a_row_with_no_number_still_exists() {
        let Value::Object(row) = serde_json::from_str(r#"{"ts":"2026-10-22T10:00:00Z","key":"k","status":"gap"}"#).unwrap() else { panic!() };
        let back = rebuild("reconciler-status", &export_of(series_of("reconciler-status", &row, UNIX_EPOCH + Duration::from_secs(5))));
        assert_eq!(back.len(), 1);
        assert_eq!(back[0]["status"], "gap");
    }

    #[test]
    fn two_rows_in_one_second_with_the_same_labels_stay_two() {
        let Value::Object(row) = serde_json::from_str(r#"{"ts":"2026-10-22T10:00:00Z","check":"C1","secs":3}"#).unwrap() else { panic!() };
        let a = series_of("sentinel-phase", &row, UNIX_EPOCH + Duration::from_millis(5_100));
        let b = series_of("sentinel-phase", &row, UNIX_EPOCH + Duration::from_millis(5_300));
        let back = rebuild("sentinel-phase", &export_of([a, b].concat()));
        assert_eq!(back.len(), 2);
    }

    #[test]
    fn a_stopped_store_is_a_named_refusal_and_never_an_empty_answer() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        drop(l);
        let r = Reader::new(Some(&addr)).unwrap();
        assert_eq!(r.rows("bead-stage", 0).unwrap_err().name(), "metrics-store-unreachable");
        assert_eq!(r.rows_or_empty("bead-stage", 0).unwrap_err().name(), "metrics-store-unreachable");
        assert_eq!(Reader::new(None).err().map(|e| e.name()), Some("metrics-store-not-configured"));
    }

    #[test]
    fn the_same_row_pushed_twice_is_one_row_and_different_rows_in_one_second_are_two() {
        let store = spira_otel::fixture::FakeStore::start();
        let line = |secs: u64| crate::build_row("2026-10-22T10:00:00Z", "h", "sentinel-phase", &[crate::parse_field_str("check=C1").unwrap(), crate::parse_field(&format!("secs={secs}")).unwrap()]).unwrap();
        for l in [line(3), line(3), line(4)] {
            push_row(Some(store.addr()), "sentinel-phase", &l).unwrap();
        }
        let rows = Reader::new(Some(store.addr())).unwrap().rows("sentinel-phase", 0).unwrap();
        let mut secs: Vec<i64> = rows.iter().filter_map(|r| r["secs"].as_i64()).collect();
        secs.sort();
        assert_eq!(secs, [3, 4]);
    }

    #[test]
    fn a_family_is_a_name_not_a_selector() {
        let r = Reader::new(Some("127.0.0.1:1")).unwrap();
        assert_eq!(r.rows("x\"}", 0).unwrap_err().name(), "metrics-store-malformed");
    }

    #[test]
    fn the_moved_families_are_exactly_the_ones_listed() {
        assert!(moved("bead-stage") && moved("unit-run-dbus"));
        for kept in ["cert-history", "suite-timing", "gate-run", "batch-round", "slots", "backlog", "escape", "cert-pass"] {
            assert!(!moved(kept), "{kept}");
        }
    }
}
