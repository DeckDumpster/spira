//! The pane's 24 h counts, read from VictoriaLogs as LogsQL counts over the shipped lifecycle
//! transitions. An unreadable store is an error the caller shows as unknown, never zero.

use spira_otel::vlogs::{transitions_filter, Logs};

const DAY_SECS: u64 = 86_400;
const TERMINAL: &str = "in(\"LANDED\", \"SUPERSEDED\", \"DROPPED\")";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayCounts {
    pub closed: u64,
    pub claims: u64,
    pub submits: u64,
    pub cuts: u64,
    pub landings: u64,
}

/// The configured store's counts: `Err` names why they could not be read.
pub fn from_config() -> Result<DayCounts, String> {
    let url = spira_config::process::cfg("SPIRA_VICTORIALOGS_URL")?;
    read(&url)
}

pub fn read(url: &str) -> Result<DayCounts, String> {
    let logs = Logs::new(url).map_err(|e| e.to_string())?;
    let bead = transitions_filter("bead", None, DAY_SECS);
    let count = |extra: &str| logs.count(&format!("{bead} {extra}")).map_err(|e| e.to_string());
    Ok(DayCounts {
        closed: count(&format!("to:{TERMINAL}"))?,
        claims: count("event:\"Claim\"")?,
        submits: count("event:\"Submit\"")?,
        cuts: count("event:\"Deliver\"")?,
        landings: count("to:\"LANDED\"")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::vlogs::fake::{FakeLogs, Script};

    fn answering(pairs: &[(&str, u64)]) -> FakeLogs {
        let logs = FakeLogs::start();
        for (fragment, n) in pairs {
            logs.answer(fragment, Script { status: 200, body: format!("{{\"n\":\"{n}\"}}\n"), hang: false });
        }
        logs
    }

    #[test]
    fn the_closed_in_24h_is_a_logsql_count_of_bead_transitions_into_a_terminal_state() {
        let logs = answering(&[("to:in(", 41), ("event:\"Claim\"", 50), ("event:\"Submit\"", 44), ("event:\"Deliver\"", 40), ("to:\"LANDED\"", 38)]);
        let got = read(&logs.url()).unwrap();
        assert_eq!(got, DayCounts { closed: 41, claims: 50, submits: 44, cuts: 40, landings: 38 });
        let asked = logs.queries();
        assert!(asked.iter().all(|q| q.starts_with(&transitions_filter("bead", None, 86_400))), "{asked:?}");
        assert!(asked.iter().any(|q| q.contains("to:in(\"LANDED\"")), "{asked:?}");
    }

    #[test]
    fn an_unreadable_store_is_an_error_not_a_zero() {
        let logs = FakeLogs::start();
        logs.answer("stats", Script { status: 503, body: "busy".into(), hang: false });
        assert!(read(&logs.url()).unwrap_err().contains("victorialogs-refused"));
        assert!(read("").unwrap_err().contains("not-configured"));
    }
}
