//! `inbox-triage append <text>` — record an event in the inbox and announce it, through
//! `inbox_rules::record`, which every other writer of the inbox shares (sp-nasmjd).

use std::path::Path;

fn now_iso() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    spira_world::clock::utc_iso(t as i64)
}

pub fn run(text: &str) -> Result<(), String> {
    let inbox = spira_config::process::cfg("SPIRA_CONCIERGE_INBOX")?;
    let line = inbox_rules::line(&now_iso(), text);
    // With no run directory there is no cursor to count from, so nothing is announced.
    let run = spira_config::process::cfg("SPIRA_RUN");
    let cursor = inbox_rules::concierge_cursor(run.as_deref().unwrap_or_default());
    let publisher = || match &run {
        Ok(_) => spira_pub::Publisher::new("inbox-triage"),
        Err(why) => Err(spira_pub::PubError::Config(why.clone())),
    };
    inbox_rules::record(Path::new(&inbox), &line, &cursor, publisher)
}
