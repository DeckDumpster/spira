//! The pages the alarms component's rule would send for the sections a scenario published, in
//! order: the real page rule over each committed section, with nothing delivered.

use incident::alarms_pass::{raised, Seen};
use incident::page::{decide, sent, Row, Verdict};
use serde_json::Value;
use spira_desired_state::section::{parse_rfc3339, parse_section};

/// One page the rule would send: what it is about and the body it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub condition: String,
    pub body: String,
}

pub fn pages(published: &[Value]) -> Vec<Page> {
    let mut rows: Vec<Row> = Vec::new();
    let mut out = Vec::new();
    for v in published {
        let Ok(section) = parse_section(v) else { continue };
        let Some(now) = section.metadata.observed_at.as_deref().and_then(parse_rfc3339) else {
            continue;
        };
        for c in raised(&[Seen { section, fresh: true }], now) {
            if decide(&rows, &c, now as i64) == Verdict::Send {
                out.push(Page {
                    condition: c.conditions.join(","),
                    body: c.body(),
                });
                rows = sent(&rows, &c, "", now as i64);
            }
        }
    }
    out
}
