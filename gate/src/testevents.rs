//! The `test.result` events of a unit gate's test step. Best effort: nothing here is read by a
//! verdict, and what cannot be sent is dropped and counted.

use crate::compose::{Member, Targets, SUBSTEP_MARK};
use crate::ports::World;
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

/// The `tree=` a testenv run's `batch.meta` names: the tree it built and tested.
pub fn batch_tree(meta: &str) -> Option<String> {
    meta.lines().find_map(|l| l.strip_prefix("tree=")).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// What the trial judged, as every record of it says.
pub struct Judged<'a> {
    pub repo_name: &'a str,
    pub bead: &'a str,
    /// The verdict key.
    pub gate: &'a str,
    /// The merged tree.
    pub tree: &'a str,
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

fn input_hashes<W: World>(w: &W, repo: &Path, members: &[Member], tree: &str) -> Result<BTreeMap<String, String>, String> {
    if members.is_empty() {
        return Err("the gate parsed no workspace members".into());
    }
    let members: Vec<_> = members.iter().map(spira_config::input_hash::Member::from).collect();
    Ok(spira_config::input_hash::input_hashes(&members, &w.input_trees(repo, tree)?))
}

/// Queues one record per test `out`, a finished test phase's output, reports onto `sink`,
/// opening it on first use.
#[allow(clippy::too_many_arguments)]
pub fn emit<W: World>(w: &W, repo: &Path, members: &[Member], targets: Targets, at: &Judged, out: &str, load1: Option<f64>, sink: &mut Option<LogPush>) {
    let tests = by_tier(out, targets);
    if tests.is_empty() {
        return;
    }
    let hashes = input_hashes(w, repo, members, at.tree).unwrap_or_else(|e| {
        w.eprint(&format!("gate: test.result records carry no input hash: {}", e.trim()));
        BTreeMap::new()
    });
    let runner = crate::telemetry::host();
    let endpoint = w.otlp_endpoint();
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
            gate: at.gate.to_string(),
            bead: at.bead.to_string(),
            repo: at.repo_name.to_string(),
            tree: at.tree.to_string(),
            round: String::new(),
            members: Vec::new(),
        };
        sink.emit(testresult::EVENT, &testresult::attrs(&record));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIT: &str = "        PASS [   0.100s] (1/1) a x\n";
    const INTEGRATION: &str = "        PASS [   0.100s] (1/1) a::it y\n";

    fn tiers(out: &str, lib: bool, test: bool) -> Vec<(Tier, String)> {
        by_tier(out, Targets { lib, bin: false, test }).into_iter().map(|(tier, t)| (tier, t.name)).collect()
    }

    #[test_id(ty5fxuh)]
    #[test]
    fn a_step_cut_before_a_tiers_marker_still_names_that_tiers_tests() {
        let cut_in_integration = format!("{UNIT}     Summary [ 1.0s] 1 test run\ngate-substep unit secs=1\n{INTEGRATION}");
        assert_eq!(tiers(&cut_in_integration, true, true), [(Tier::Unit, "x".to_string()), (Tier::Integration, "y".to_string())]);
        assert_eq!(tiers(UNIT, true, true), [(Tier::Unit, "x".to_string())], "cut in the unit tier");
        assert_eq!(tiers(INTEGRATION, false, true), [(Tier::Integration, "y".to_string())], "no unit tier to be cut in");
    }
}
