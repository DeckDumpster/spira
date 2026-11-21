//! The control store's size gauges: the main file, each day-file family, and the keyspaces,
//! the last found by a walk that reads one bounded page per call.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use spira_otel::metrics::Series;
use state_store::backend::{Usage, UsagePage};
use state_store::Backend;

pub const FILE_BYTES_GAUGE: &str = "spira_state_store_file_bytes";
pub const FREE_BYTES_GAUGE: &str = "spira_state_store_free_bytes";
pub const FAMILY_BYTES_GAUGE: &str = "spira_state_store_family_bytes";
pub const KEYSPACE_BYTES_GAUGE: &str = "spira_state_store_keyspace_bytes";
pub const KEYSPACE_KEYS_GAUGE: &str = "spira_state_store_keyspace_keys";
pub const KEYSPACE_PASS_GAUGE: &str = "spira_state_store_keyspace_pass_seconds";

pub const PASS_GAUGES: &[&str] = &[KEYSPACE_BYTES_GAUGE, KEYSPACE_KEYS_GAUGE, KEYSPACE_PASS_GAUGE];

/// The main file's size and free bytes, and the bytes of each family's files.
pub fn file_series<B: Backend>(backend: &B, at: SystemTime) -> state_store::Result<Vec<Series>> {
    let (bytes, free) = backend.file_bytes()?;
    let gauge = |name: &str, value: u64, labels: Vec<(String, String)>| Series { name: name.into(), value: value as f64, labels, at };
    let mut out = vec![gauge(FILE_BYTES_GAUGE, bytes, vec![]), gauge(FREE_BYTES_GAUGE, free, vec![])];
    for family in backend.family_files()? {
        let total = family.files.iter().map(|f| f.bytes).sum();
        out.push(gauge(FAMILY_BYTES_GAUGE, total, vec![("family".into(), family.family)]));
    }
    Ok(out)
}

/// A completed walk of every live key.
#[derive(Debug, PartialEq, Eq)]
pub struct Pass {
    pub prefixes: BTreeMap<String, Usage>,
    pub took: Duration,
}

impl Pass {
    pub fn series(&self, at: SystemTime) -> Vec<Series> {
        let mut out = vec![Series { name: KEYSPACE_PASS_GAUGE.into(), value: self.took.as_secs_f64(), labels: vec![], at }];
        for (prefix, u) in &self.prefixes {
            let labels = vec![("prefix".to_string(), prefix.clone())];
            out.push(Series { name: KEYSPACE_BYTES_GAUGE.into(), value: u.bytes as f64, labels: labels.clone(), at });
            out.push(Series { name: KEYSPACE_KEYS_GAUGE.into(), value: u.keys as f64, labels, at });
        }
        out
    }
}

/// The keyspace walk's position and partial totals, held in memory only.
#[derive(Default)]
pub struct Walk {
    cursor: Option<String>,
    totals: BTreeMap<String, Usage>,
    started: Option<Instant>,
}

impl Walk {
    /// Reads one page of at most `limit` rows. `Some` only on the page that ends the pass; the
    /// next call then starts a new one. A failed read leaves the walk where it was.
    pub fn step<B: Backend>(&mut self, backend: &B, limit: usize, now: Instant) -> state_store::Result<Option<Pass>> {
        let UsagePage { prefixes, next, .. } = backend.usage_page(self.cursor.as_deref(), limit)?;
        let started = *self.started.get_or_insert(now);
        for (prefix, u) in prefixes {
            let t = self.totals.entry(prefix).or_default();
            t.keys += u.keys;
            t.bytes += u.bytes;
        }
        self.cursor = next;
        if self.cursor.is_some() {
            return Ok(None);
        }
        self.started = None;
        Ok(Some(Pass { prefixes: std::mem::take(&mut self.totals), took: now.saturating_duration_since(started) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use state_store::{MemBackend, Rev, Write};

    fn seeded(keys: &[(&str, usize)]) -> MemBackend {
        let b = MemBackend::new();
        let mut head = 0;
        for (k, len) in keys {
            let put = Write::Put { key: (*k).into(), value: vec![b'x'; *len], create_rev: Rev::new(head + 1, 0), prev: None };
            head = b.commit(head, &[put]).unwrap();
        }
        b
    }

    #[test]
    fn a_walk_totals_nothing_until_its_last_page_then_equals_the_sum_over_all_pages() {
        let keys = [("a/1", 3), ("a/2", 4), ("b/1", 5), ("b/2", 6), ("b/3", 7), ("events/1", 8), ("events/2", 9)];
        let b = seeded(&keys);
        let mut want: BTreeMap<String, Usage> = BTreeMap::new();
        let mut after = None;
        loop {
            let page = b.usage_page(after.as_deref(), 1000).unwrap();
            for (p, u) in page.prefixes {
                let t = want.entry(p).or_default();
                t.keys += u.keys;
                t.bytes += u.bytes;
            }
            if page.next.is_none() {
                break;
            }
            after = page.next;
        }
        assert_eq!(want["b/"], Usage { keys: 3, bytes: 18 });

        let (mut walk, t0) = (Walk::default(), Instant::now());
        for i in 0..2 {
            assert_eq!(walk.step(&b, 3, t0 + Duration::from_secs(i)).unwrap(), None, "page {i} is not the last");
        }
        let pass = walk.step(&b, 3, t0 + Duration::from_secs(2)).unwrap().expect("the short page ends the pass");
        assert_eq!(pass.prefixes, want);
        assert_eq!(pass.took, Duration::from_secs(2));

        assert_eq!(walk.step(&b, 3, t0 + Duration::from_secs(3)).unwrap(), None, "the next call starts a new pass");
    }

    #[test]
    fn a_completed_pass_is_encoded_as_prefix_labelled_gauges_with_its_duration() {
        let pass = Pass { prefixes: BTreeMap::from([("events/".to_string(), Usage { keys: 2, bytes: 17 })]), took: Duration::from_secs(9) };
        let sink = spira_otel::metrics::MetricsSink::new(None, "spira-lc", Duration::from_millis(1));
        let encoded = format!("{:?}", sink.encode_series(&pass.series(SystemTime::now())));
        for want in [KEYSPACE_BYTES_GAUGE, KEYSPACE_KEYS_GAUGE, KEYSPACE_PASS_GAUGE, "prefix", "events/"] {
            assert!(encoded.contains(want), "{want} missing from {encoded}");
        }
    }

    #[test]
    fn file_and_family_gauges_carry_their_names_and_the_family_label() {
        let dir = testkit::TempDir::new("store-gauges");
        let b = state_store_sqlite::SqliteBackend::open(&dir.path().join("control.db"), state_store_sqlite::Options::default()).unwrap();
        let mut head = 0;
        let bulk = vec![b'x'; 8_000];
        for i in 0..200 {
            let put = Write::Put { key: format!("k/{i}"), value: bulk.clone(), create_rev: Rev::new(head + 1, 0), prev: None };
            head = b.commit(head, &[put]).unwrap();
        }
        let dels: Vec<_> = (0..200).map(|i| Write::Delete { key: format!("k/{i}"), prev: Rev::new(i + 1, 0) }).collect();
        head = b.commit(head, &dels).unwrap();
        b.compact(head).unwrap();
        let series = file_series(&b, SystemTime::now()).unwrap();
        let by = |name: &str| series.iter().filter(|s| s.name == name).collect::<Vec<_>>();
        assert!(by(FILE_BYTES_GAUGE)[0].value > 0.0);
        assert!(by(FREE_BYTES_GAUGE)[0].value > 0.0, "deleted rows leave free pages");
        assert!(by(FREE_BYTES_GAUGE)[0].value <= by(FILE_BYTES_GAUGE)[0].value);
        assert!(by(FAMILY_BYTES_GAUGE).iter().any(|s| s.labels == [("family".to_string(), "main".to_string())]), "main is a family");
        let sink = spira_otel::metrics::MetricsSink::new(None, "spira-lc", Duration::from_millis(1));
        let encoded = format!("{:?}", sink.encode_series(&series));
        for want in [FILE_BYTES_GAUGE, FREE_BYTES_GAUGE, FAMILY_BYTES_GAUGE, "family"] {
            assert!(encoded.contains(want), "{want} missing from {encoded}");
        }
    }

    #[test]
    fn a_new_pass_drops_a_prefix_that_has_gone() {
        let at = SystemTime::now();
        let pass = |p: &str| Pass { prefixes: BTreeMap::from([(p.to_string(), Usage { keys: 1, bytes: 1 })]), took: Duration::ZERO }.series(at);
        crate::slow::replace_gauges(PASS_GAUGES, pass("gone/"));
        crate::slow::replace_gauges(PASS_GAUGES, pass("kept/"));
        let held = crate::slow::held();
        let prefixes: Vec<_> = held.iter().filter(|s| s.name == KEYSPACE_BYTES_GAUGE).flat_map(|s| s.labels.iter().map(|l| l.1.clone())).collect();
        assert_eq!(prefixes, ["kept/"]);
    }

    #[test]
    fn the_dashboard_has_the_four_panels_and_queries_the_gauges_by_their_consts() {
        let board: serde_json::Value = serde_json::from_str(include_str!("../../spira/metrics/grafana/dashboards/bead-store.json")).unwrap();
        let panels = board["panels"].as_array().unwrap();
        let exprs = |title: &str| -> String {
            let p = panels.iter().find(|p| p["title"] == title).unwrap_or_else(|| panic!("no panel {title}"));
            p["targets"].as_array().unwrap().iter().map(|t| t["expr"].as_str().unwrap().to_string()).collect::<Vec<_>>().join(" ")
        };
        let file = exprs("control store file: bytes and free bytes");
        assert!(file.contains(FILE_BYTES_GAUGE) && file.contains(FREE_BYTES_GAUGE));
        let share = exprs("control store free-page share");
        assert!(share.contains(FREE_BYTES_GAUGE) && share.contains(FILE_BYTES_GAUGE));
        assert!(exprs("control store largest keyspaces").contains(KEYSPACE_BYTES_GAUGE));
        assert!(exprs("control store day-file bytes by family").contains(FAMILY_BYTES_GAUGE));
    }

    #[test]
    fn the_checkpointing_row_queries_each_metric_by_its_const_and_the_wal_panel_carries_its_lines() {
        use crate::slow::{GATE_WAIT_MAX_SECONDS, READS_ENDED, READS_OVERRAN};
        use state_store_sqlite::{WAL_FORCED_GIVEN_UP, WAL_FORCED_RESETS, WAL_RESETS_COMPLETED, WAL_RESETS_GIVEN_UP};
        let board: serde_json::Value = serde_json::from_str(include_str!("../../spira/metrics/grafana/dashboards/bead-store.json")).unwrap();
        let panels = board["panels"].as_array().unwrap();
        let panel = |title: &str| panels.iter().find(|p| p["title"] == title).unwrap_or_else(|| panic!("no panel {title}"));
        let exprs = |title: &str| -> String { panel(title)["targets"].as_array().unwrap().iter().map(|t| t["expr"].as_str().unwrap().to_string()).collect::<Vec<_>>().join(" ") };
        let resets = exprs("control store WAL resets per hour");
        for name in [WAL_RESETS_COMPLETED, WAL_RESETS_GIVEN_UP, WAL_FORCED_RESETS, WAL_FORCED_GIVEN_UP] {
            assert!(resets.contains(name), "{name} missing from {resets}");
        }
        let reads = exprs("control store reads ended at the limit by verb");
        assert!(reads.contains(READS_ENDED) && reads.contains(READS_OVERRAN), "{reads}");
        let gate = panel("control store longest gate wait by verb");
        assert!(exprs("control store longest gate wait by verb").contains(GATE_WAIT_MAX_SECONDS));
        assert!(exprs("reads ended or held at the gate: what the caller did next").contains(crate::followups::READ_FOLLOWUPS));
        let line = |p: &serde_json::Value| -> Vec<f64> { p["fieldConfig"]["defaults"]["thresholds"]["steps"].as_array().unwrap().iter().filter_map(|s| s["value"].as_f64()).collect() };
        assert_eq!(line(gate), [0.1]);
        let wal = panel("control store WAL size");
        assert_eq!(line(wal), [8.0 * 1048576.0, 9.0 * 1048576.0]);
        assert!(exprs("control store WAL size").contains(state_store_sqlite::WAL_BYTES_GAUGE));
    }
}
