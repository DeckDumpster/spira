//! `batcher attribute`'s judgement: which suites and reds are attributed, what the pass
//! followed allows, and how the round's end becomes the outcome line. What touches git, the
//! environment or a process is behind [`Edge`], so a replay judges a saved round with the same code.

use std::collections::BTreeMap;
use std::path::Path;

use batcher::core::Member;
use batcher::drive::{self, RoundOps, RoundRunner};

use crate::attribute::Attribution;
use crate::opts::Opts;
use crate::{io, order, vm};

/// What the judgement reaches outside itself for.
pub trait Edge {
    type Runner: RoundRunner;
    type Ops: RoundOps;
    /// Each member's paths changed against the round's base.
    fn changed(&self, members: &[Member]) -> BTreeMap<String, Vec<String>>;
    /// The deny list in force when no pass is followed.
    fn current_deny(&self) -> Result<vm::DenySnapshot, String>;
    /// The runner and the round's effects, for `suites` on the round tree.
    fn open(&mut self, suites: &[String], changed: BTreeMap<String, Vec<String>>, deny: &vm::DenySnapshot, own_commits: vm::RoundCommits) -> Result<(Self::Runner, Self::Ops), String>;
    /// What follows the drive: a host or install fault's incident, the runner closed, results kept; the end the round is judged as.
    fn settle(&mut self, runner: &mut Self::Runner, ops: &mut Self::Ops, end: Result<drive::RoundEnd, String>, members: &[Member], suites: &[String]) -> Result<drive::RoundEnd, String>;
    /// A mender held the round's reds.
    fn mended(&self, ops: &Self::Ops) -> bool;
    /// The round tree's head after the drive.
    fn head(&self) -> String;
}

/// `all` narrowed to the CSV `named` when one is given; a name the tree does not hold is dropped.
pub fn named_suites(all: Vec<String>, named: Option<&str>) -> Vec<String> {
    let Some(named) = named else { return all };
    let want: Vec<&str> = named.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    all.into_iter().filter(|s| want.contains(&s.as_str())).collect()
}

/// Why attribution cannot start: no shell suite to attribute and no red test tier in the pass either.
pub fn blocks_attribution(suites: &[String], red_phases: &[String], red_filters: &[String], pass: Option<&Path>) -> Option<String> {
    if !suites.is_empty() || !red_phases.is_empty() || !red_filters.is_empty() {
        return None;
    }
    let at = pass.map(|p| format!(" in {}", p.display())).unwrap_or_default();
    Some(format!("the pass names no red the round can attribute: no shell suite, no red unit-tests or integration-tests and no red lint or fence{at}"))
}

/// What a failed attribution prints: a round abandoned under it was never judged, so its
/// failure is no red.
pub fn failed_attribution(ended: bool, why: String) -> Attribution {
    if ended { Attribution::Abandoned } else { Attribution::Blocked(why) }
}

/// The outcome line of attributing `members` of a round whose tree lists `listed` suites.
pub fn attribute<E: Edge>(o: &Opts, edge: &mut E, members: Vec<Member>, listed: Result<Vec<String>, String>, history: &str, budget: batcher::attrib::Budget) -> Result<String, String> {
    let suites = match listed {
        Ok(all) => named_suites(order::longest_first(&all, history).list, o.suites.as_deref()),
        Err(why) => return Ok(Attribution::Fault(format!("the round tree's suites could not be listed: {why}")).line()),
    };
    let red_phases = o.after_pass.as_deref().map(io::red_test_phases).unwrap_or_default();
    let red_filters = o.after_pass.as_deref().map(vm::red_filters).unwrap_or_default();
    if let Some(why) = blocks_attribution(&suites, &red_phases, &red_filters, o.after_pass.as_deref()) {
        return Ok(Attribution::Blocked(why).line());
    }
    let changed = edge.changed(&members);
    let deny = match o.after_pass.as_deref() {
        Some(pass) => vm::DenySnapshot::from_pass(pass).ok_or_else(|| format!("{} holds no deny list of the round's pass; refusing to attribute with another", pass.display()))?,
        None => edge.current_deny()?,
    };
    let own_commits = o.after_pass.as_deref().map(vm::round_commits_of_pass).unwrap_or_default();
    let (mut runner, mut ops) = edge.open(&suites, changed, &deny, own_commits)?;
    let end = drive::attribute_round(&mut runner, &mut ops, &suites, members.clone(), budget);
    let end = edge.settle(&mut runner, &mut ops, end, &members, &suites);
    let mended = edge.mended(&ops);
    Ok(outcome(end, mended, || edge.head()).line())
}

fn outcome(end: Result<drive::RoundEnd, String>, mended: bool, head: impl FnOnce() -> String) -> Attribution {
    match end {
        Ok(drive::RoundEnd::Land { members, attribution_secs }) => Attribution::Land { members: members.into_iter().map(|m| (m.id, m.tip)).collect(), attribution_secs, mended, head: head() },
        Ok(drive::RoundEnd::Blocked(why)) => Attribution::Blocked(why),
        Ok(drive::RoundEnd::BaseRed { base, why }) => Attribution::Blocked(format!("base red at {base}: {why}")),
        Ok(drive::RoundEnd::Emptied(why)) => Attribution::Emptied(why),
        Ok(drive::RoundEnd::Faulted(why) | drive::RoundEnd::Stalled(why)) => Attribution::Faulted(why),
        Ok(drive::RoundEnd::RunnerDifference { tests, red_on, green_on }) => Attribution::RunnerDifference { tests, red_on, green_on },
        Ok(drive::RoundEnd::Abandoned) => Attribution::Abandoned,
        Ok(drive::RoundEnd::Unconverged { red, probes, suspects }) => Attribution::Unconverged(format!(
            "{red} red reproduces but {probes} probes named no member; suspects: {}",
            if suspects.is_empty() { "none".to_string() } else { suspects.join(",") }
        )),
        Err(e) if crate::attribute::is_round_vm_fault(&e) => Attribution::Fault(format!("the attribution did not finish: {e}")),
        Err(e) => Attribution::Blocked(format!("the attribution did not finish: {e}")),
    }
}
