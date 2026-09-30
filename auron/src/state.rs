//! `$SPIRA_RUN/auron.state` — TSV, not JSON, because nothing else reads it (auron.sh's own
//! reasoning, kept): one line per condition, plus `#first_run`, `#probe_id` and `#restart`
//! header lines.
//!
//! EVERY FIELD HERE EXCEPT `flaps` IS A CACHE. The bead id is re-derived from the database
//! by label on every run, so a lost state file costs the flap count and nothing else — the
//! alert is still found, still updated, still closed. A flap count cannot be recomputed
//! from the graph, which is the one reason this file exists at all.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyState {
    pub state: String, // "clear" or "firing"
    pub seen: i64,
    pub unseen: i64,
    pub since: i64,
    pub first: i64,
    pub flaps: i64,
    pub bead: String,
    pub refreshed: i64,
}

impl Default for KeyState {
    fn default() -> KeyState {
        KeyState { state: "clear".to_string(), seen: 0, unseen: 0, since: 0, first: 0, flaps: 0, bead: String::new(), refreshed: 0 }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestartBaseline {
    pub baseline: i64,
    pub baseline_at: i64,
    pub last: i64,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub first_run: i64,
    pub probe_id: String,
    pub keys: BTreeMap<String, KeyState>,
    pub restarts: BTreeMap<String, RestartBaseline>,
}

fn n(s: &str) -> i64 {
    s.trim().parse().unwrap_or(0)
}

impl State {
    /// Parses the TSV. A short line (fewer tab-separated fields than the current shape)
    /// still parses — the older state-line spelling before `refreshed` was added — with
    /// every trailing field defaulted exactly as the bash reader's `"${x:-default}"` did.
    pub fn parse(text: &str) -> State {
        let mut st = State::default();
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') && !line.starts_with("#first_run") && !line.starts_with("#probe_id") && !line.starts_with("#restart") {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            match f.first().copied() {
                Some("#first_run") => st.first_run = n(f.get(1).copied().unwrap_or("0")),
                Some("#probe_id") => st.probe_id = f.get(1).copied().unwrap_or("").to_string(),
                Some("#restart") => {
                    if let Some(unit) = f.get(1) {
                        st.restarts.insert(
                            unit.to_string(),
                            RestartBaseline { baseline: n(f.get(2).copied().unwrap_or("0")), baseline_at: n(f.get(3).copied().unwrap_or("0")), last: n(f.get(4).copied().unwrap_or("0")) },
                        );
                    }
                }
                Some(k) if !k.is_empty() => {
                    st.keys.insert(
                        k.to_string(),
                        KeyState {
                            state: f.get(1).filter(|s| !s.is_empty()).unwrap_or(&"clear").to_string(),
                            seen: n(f.get(2).copied().unwrap_or("0")),
                            unseen: n(f.get(3).copied().unwrap_or("0")),
                            since: n(f.get(4).copied().unwrap_or("0")),
                            first: n(f.get(5).copied().unwrap_or("0")),
                            flaps: n(f.get(6).copied().unwrap_or("0")),
                            bead: f.get(7).copied().unwrap_or("").to_string(),
                            refreshed: n(f.get(8).copied().unwrap_or("0")),
                        },
                    );
                }
                _ => {}
            }
        }
        st
    }

    /// Renders back to TSV. Map iteration order (`BTreeMap`) is deterministic, unlike
    /// bash's `"${!S_STATE[@]}"`, which is not guaranteed — a cosmetic improvement, since
    /// nothing but this same reader ever reads the file back.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("#first_run\t{}\n", self.first_run));
        out.push_str(&format!("#probe_id\t{}\n", self.probe_id));
        for (k, s) in &self.keys {
            out.push_str(&format!("{k}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n", s.state, s.seen, s.unseen, s.since, s.first, s.flaps, s.bead, s.refreshed));
        }
        for (u, r) in &self.restarts {
            out.push_str(&format!("#restart\t{u}\t{}\t{}\t{}\n", r.baseline, r.baseline_at, r.last));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_parses_to_defaults() {
        let s = State::parse("");
        assert_eq!(s.first_run, 0);
        assert_eq!(s.probe_id, "");
        assert!(s.keys.is_empty());
    }

    #[test]
    fn round_trips_a_full_key_line() {
        let mut s = State::default();
        s.first_run = 1000;
        s.probe_id = "sp-probe1".to_string();
        s.keys.insert("sentinel-stalled".to_string(), KeyState { state: "firing".to_string(), seen: 2, unseen: 0, since: 1200, first: 1100, flaps: 3, bead: "sp-alert1".to_string(), refreshed: 1200 });
        let rendered = s.render();
        let back = State::parse(&rendered);
        assert_eq!(back.first_run, 1000);
        assert_eq!(back.probe_id, "sp-probe1");
        assert_eq!(back.keys["sentinel-stalled"], s.keys["sentinel-stalled"]);
    }

    #[test]
    fn round_trips_restart_baselines() {
        let mut s = State::default();
        s.restarts.insert("spira-sentinel.service".to_string(), RestartBaseline { baseline: 2, baseline_at: 5000, last: 7 });
        let back = State::parse(&s.render());
        assert_eq!(back.restarts["spira-sentinel.service"], s.restarts["spira-sentinel.service"]);
    }

    #[test]
    fn older_short_line_spelling_still_parses_with_defaults() {
        // Only through `bead` — no `refreshed` column, the shape before it was added.
        let text = "sentinel-stalled\tfiring\t2\t0\t1200\t1100\t3\tsp-alert1\n";
        let s = State::parse(text);
        let k = &s.keys["sentinel-stalled"];
        assert_eq!(k.state, "firing");
        assert_eq!(k.flaps, 3);
        assert_eq!(k.bead, "sp-alert1");
        assert_eq!(k.refreshed, 0, "a missing trailing column defaults to 0, exactly bash's parameter-expansion default");
    }

    #[test]
    fn a_bare_key_with_no_other_fields_defaults_everything() {
        let s = State::parse("some-key\n");
        let k = &s.keys["some-key"];
        assert_eq!(*k, KeyState::default());
    }

    #[test]
    fn comment_lines_other_than_the_three_headers_are_ignored() {
        let s = State::parse("# a stray human comment\nreal-key\tfiring\n");
        assert_eq!(s.keys.len(), 1);
        assert!(s.keys.contains_key("real-key"));
    }
}
