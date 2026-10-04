//! `--dolt-drop-check` — per-minute rate of Dolt "client connection went away" /
//! "connection was closed" warnings in the dolt log. One incident when any of the last
//! `window` minute-buckets seen in the log tail reaches `threshold` warnings. Lines carry
//! `time="YYYY-MM-DDTHH:MM:SS..."`; the first 16 chars of that value are the minute bucket.

use crate::incident::{self, Finding};
use crate::log::log;
use std::collections::BTreeMap;
use std::path::Path;

pub fn per_minute(text: &str) -> BTreeMap<String, usize> {
    let mut by = BTreeMap::new();
    for l in text.lines() {
        if !(l.contains("client connection went away") || l.contains("connection was closed")) {
            continue;
        }
        let Some(i) = l.find("time=\"") else { continue };
        let Some(m) = l.get(i + 6..i + 22) else { continue };
        *by.entry(m.to_string()).or_insert(0) += 1;
    }
    by
}

/// Peak (minute, count) among the newest `window` minute buckets.
pub fn peak(by: &BTreeMap<String, usize>, window: usize) -> Option<(String, usize)> {
    by.iter().rev().take(window).max_by_key(|(_, c)| **c).map(|(m, c)| (m.clone(), *c))
}

pub fn run(log_path: &Path, db: &str, home_repo: &str, incident_sh: &str, window: usize, threshold: usize) {
    let Ok(bytes) = std::fs::read(log_path) else {
        log("watchtower: dolt-drop-check — no dolt log");
        return;
    };
    let start = bytes.len().saturating_sub(4 << 20);
    let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    let Some((minute, n)) = peak(&per_minute(&text), window) else {
        log("watchtower: dolt-drop-check — no drop warnings");
        return;
    };
    if n < threshold {
        log(&format!("watchtower: dolt-drop-check — peak {n}/min below {threshold}"));
        return;
    }
    if !incident::is_usable(incident_sh) {
        log(&format!("watchtower: dolt-drop-check skipped — {incident_sh} not readable"));
        return;
    }
    let body = format!("Dolt is dropping client connections.\n\npeak: {n} warnings/min at {minute}\nthreshold: {threshold}/min over the last {window} minute(s) logged\n\nSee sop-dolt-client-drop-bursts; correlate with load.\n");
    let f = Finding::new(db, home_repo, "DOLT CLIENT-DROP BURST", &body)
        .priority(2)
        .reference("incident:dolt-client-drop-burst".to_string())
        .cause("dolt-client-drop");
    incident::file(incident_sh, &f);
    log(&format!("watchtower: dolt-drop-check filed incident (peak {n}/min)"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(m: &str) -> String {
        format!("time=\"2026-10-04T{m}:04-07:00\" level=warning msg=\"x\" error=\"connection was closed\"\n")
    }

    #[test]
    fn buckets_and_peak() {
        let mut t = String::new();
        for _ in 0..3 { t.push_str(&line("13:15")); }
        t.push_str(&line("13:16"));
        t.push_str("time=\"2026-10-04T13:16:04-07:00\" level=info msg=\"ok\"\n");
        let by = per_minute(&t);
        assert_eq!(by["2026-10-04T13:15"], 3);
        assert_eq!(by["2026-10-04T13:16"], 1);
        assert_eq!(peak(&by, 5), Some(("2026-10-04T13:15".to_string(), 3)));
        assert_eq!(peak(&by, 1), Some(("2026-10-04T13:16".to_string(), 1)));
    }

    #[test]
    fn a_burst_files_one_incident_and_quiet_files_none() {
        let d = testkit::TempDir::new("wt-dolt-drop");
        let inc = d.join("inc.sh");
        let calls = d.join("calls");
        std::fs::write(&inc, format!("#!/usr/bin/env bash\necho \"$SPIRA_INCIDENT_REF\" >> {}\ncat >/dev/null\n", calls.display())).unwrap();
        let inc = inc.to_string_lossy().into_owned();
        let lp = d.join("dolt.log");
        std::fs::write(&lp, line("13:15").repeat(2)).unwrap();
        run(&lp, "db", "spira", &inc, 5, 10);
        assert!(!calls.exists());
        std::fs::write(&lp, line("13:15").repeat(12)).unwrap();
        run(&lp, "db", "spira", &inc, 5, 10);
        assert!(std::fs::read_to_string(&calls).unwrap().contains("incident:dolt-client-drop-burst"));
    }
}
