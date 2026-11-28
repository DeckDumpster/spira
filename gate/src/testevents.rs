//! The `test.result` events of a unit gate's test step. Best effort: nothing here is read by a
//! verdict, and what cannot be sent is dropped and counted.

use crate::compose::{Targets, SUBSTEP_MARK};
use crate::ports::{Ctx, World};
use spira_config::unit_tier::{self, TestLine};
use spira_otel::logs::LogPush;
use spira_otel::testresult::{self, Tier};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// One request to the Collector.
const DEADLINE: Duration = Duration::from_secs(2);
/// The most a finishing gate waits for its queued records.
pub const FLUSH: Duration = Duration::from_secs(5);
/// Several times the whole workspace's tests: a step's records are queued at once.
const QUEUE: usize = 1 << 16;

/// What the trial judged, as every record of it says.
pub struct Judged {
    pub repo_name: String,
    pub bead: String,
    /// The verdict key.
    pub gate: String,
    /// The merged tree.
    pub tree: String,
}

/// Each tier's tests: a tier's output ends at its substep marker, and each is its own nextest
/// run. Output after the last marker is the tier that was cut before it could print one.
fn by_tier(out: &str, targets: Targets) -> Vec<(Tier, TestLine)> {
    let mut found = Vec::new();
    let mut run = String::new();
    let mut unit_ended = false;
    let take = |tier: Tier, run: &mut String, found: &mut Vec<(Tier, TestLine)>| {
        found.extend(unit_tier::results(run).into_iter().map(|t| (tier, t)));
        run.clear();
    };
    for line in out.lines() {
        match line.strip_prefix(SUBSTEP_MARK).and_then(|r| r.split_whitespace().next()) {
            Some("unit") => {
                take(Tier::Unit, &mut run, &mut found);
                unit_ended = true;
            }
            Some("integration") => take(Tier::Integration, &mut run, &mut found),
            _ => {
                run.push_str(line);
                run.push('\n');
            }
        }
    }
    let cut = if unit_ended || !(targets.lib || targets.bin) { Tier::Integration } else { Tier::Unit };
    take(cut, &mut run, &mut found);
    found
}

fn input_hashes<W: World>(w: &W, ctx: &Ctx, repo: &Path, checkout: &Path, tree: &str) -> Result<BTreeMap<String, String>, String> {
    let members: Vec<_> = crate::telemetry::members(w, ctx, checkout)?.iter().map(spira_config::input_hash::Member::from).collect();
    Ok(spira_config::input_hash::input_hashes(&members, &w.input_trees(repo, tree)?))
}

/// Queues one record per test `out` reports onto `sink`, opening it on first use. Does nothing
/// when no Collector is configured.
#[allow(clippy::too_many_arguments)]
pub fn emit<W: World>(w: &W, ctx: &Ctx, repo: &Path, checkout: &Path, targets: Targets, at: &Judged, out: &str, sink: &mut Option<LogPush>) {
    let endpoint = w.otlp_endpoint();
    if endpoint.trim().is_empty() {
        return;
    }
    let load1 = w.load1();
    let tests = by_tier(out, targets);
    if tests.is_empty() {
        return;
    }
    let hashes = input_hashes(w, ctx, repo, checkout, &at.tree).unwrap_or_else(|e| {
        w.eprint(&format!("gate: test.result records carry no input hash: {}", e.trim()));
        BTreeMap::new()
    });
    let runner = crate::telemetry::host();
    let sink = sink.get_or_insert_with(|| LogPush::new(&endpoint, "spira-gate", DEADLINE, QUEUE));
    for (tier, t) in tests {
        let record = testresult::Result {
            test_id: t.test_id().to_string(),
            crate_name: t.crate_name().to_string(),
            tier,
            input_hash: hashes.get(t.crate_name()).cloned().unwrap_or_default(),
            outcome: t.outcome,
            duration_ms: (t.secs * 1000.0).round() as u64,
            name: t.name,
            runner: runner.clone(),
            load1,
            source: "gate",
            gate: at.gate.clone(),
            bead: at.bead.clone(),
            repo: at.repo_name.clone(),
            tree: at.tree.clone(),
        };
        sink.emit(testresult::EVENT, &testresult::attrs(&record));
    }
}
