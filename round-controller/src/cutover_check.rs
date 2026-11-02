//! `round-controller cutover-check`: `--readiness`, whether every registered round repo and the
//! world are ready for the cutover; `--since-rev <rev>`, whether every round repo has landed a
//! round since the store stood at `rev`; and `--probe-base <repo>`, the base run that answers
//! readiness's base check. A check that cannot be read is red, naming why: never green by default.

use std::collections::BTreeMap;

use batcher::drive::{MainEnd, MainKind};
use lifecycle::batch::{BatchRow, BatchState};
use serde::Deserialize;
use state_store::{events_key, Selector};

use crate::ledger::summary_key;
use crate::ports::{
    Attempt, Class, Cause, InitiatedBy, Member, PassIdentity, Ports, SubPass, SubPassKind, Summary,
};
use crate::resources::round_key;
use crate::run::{kill_child, wait_gone, Killed, RunEnv, SubPasses};
use crate::slots::{grant_for_hand, SlotGrant};
use crate::stack::{lane, Lane};

pub const USAGE: &str = "cutover-check --readiness | --since-rev <rev> | --probe-base <repo>";

/// A registered repository's own CI declaration, and the harness's gate definition.
pub const DECL: &str = ".spira/testenv.toml";
pub const GATE_STEPS: &str = "gate.steps";
/// The batcher's per-repo round files: a round the batcher opened carries no lease token.
pub const QUEUE_FILES: [&str; 2] = ["open", "prepared"];

/// The base probe's slot holder and the label its attempt is recorded under.
pub const PROBE_HOLDER: &str = "round-controller-probe";

/// What cutover-check reads beside the store: the registry, the trees at the landing refs, the
/// queue's round files and the lifecycle's beads.
pub trait Facts {
    /// The registered round repos (`queue.local` and `queue.forge`), in registry order.
    fn round_repos(&self) -> Result<Vec<String>, String>;
    /// `path`'s text at `repo`'s landing ref; `None` when the tree has no such file.
    fn at_landing(&self, repo: &str, path: &str) -> Result<Option<String>, String>;
    /// The entry names directly under `dir` at `repo`'s landing ref.
    fn names_at_landing(&self, repo: &str, dir: &str) -> Result<Vec<String>, String>;
    /// Every `open` or `prepared` file under the queue directory, as `<repo>/<file>`.
    fn queue_files(&self) -> Result<Vec<String>, String>;
    /// How many of `repo`'s beads are SUBMITTED.
    fn submitted(&self, repo: &str) -> Result<usize, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Readiness,
    SinceRev(u64),
    ProbeBase(String),
}

pub fn parse(args: &[&str]) -> Result<Mode, String> {
    match args {
        ["--readiness"] => Ok(Mode::Readiness),
        ["--since-rev", rev] => rev
            .parse()
            .map(Mode::SinceRev)
            .map_err(|e| format!("--since-rev {rev:?}: {e}")),
        ["--probe-base", repo] if !repo.is_empty() && !repo.starts_with('-') => {
            Ok(Mode::ProbeBase(repo.to_string()))
        }
        _ => Err(format!("usage: round-controller {USAGE}")),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub green: bool,
    pub why: String,
}

fn green(name: &'static str, why: impl Into<String>) -> Check {
    Check {
        name,
        green: true,
        why: why.into(),
    }
}

fn red(name: &'static str, why: impl Into<String>) -> Check {
    Check {
        name,
        green: false,
        why: why.into(),
    }
}

/// One repo's checks, or the world's under the scope `global`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub scope: String,
    pub checks: Vec<Check>,
}

impl Row {
    pub fn green(&self) -> bool {
        self.checks.iter().all(|c| c.green)
    }

    pub fn check(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.name == name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Readiness {
    pub rows: Vec<Row>,
}

pub const GLOBAL: &str = "global";

fn word(green: bool) -> &'static str {
    if green {
        "green"
    } else {
        "red"
    }
}

impl Readiness {
    pub fn green(&self) -> bool {
        self.rows.iter().all(Row::green)
    }

    pub fn code(&self) -> i32 {
        i32::from(!self.green())
    }

    pub fn row(&self, scope: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.scope == scope)
    }

    /// One line per row naming each check's colour, each check's reason beneath it, then the verdict.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for r in &self.rows {
            let cols: Vec<String> = r
                .checks
                .iter()
                .map(|c| format!("{}={}", c.name, word(c.green)))
                .collect();
            out.push_str(&format!("{} {} {}\n", r.scope, word(r.green()), cols.join(" ")));
            for c in &r.checks {
                out.push_str(&format!("  {}: {}\n", c.name, c.why));
            }
        }
        out.push_str(&format!("readiness={}\n", word(self.green())));
        out
    }
}

pub fn readiness(p: &Ports, f: &dyn Facts) -> Readiness {
    let mut rows = Vec::new();
    let mut global = Vec::new();
    match f.round_repos() {
        Err(e) => global.push(red("repos", format!("cannot read the registry: {e}"))),
        Ok(repos) if repos.is_empty() => {
            global.push(red("repos", "the registry names no queue.local or queue.forge repo"))
        }
        Ok(repos) => {
            global.push(green("repos", repos.join(" ")));
            for repo in repos {
                rows.push(Row {
                    checks: vec![ci(f, &repo), base(p, &repo), submitted(f, &repo)],
                    scope: repo,
                });
            }
        }
    }
    global.push(rounds(p));
    global.push(queue_files(f));
    rows.push(Row {
        scope: GLOBAL.into(),
        checks: global,
    });
    Readiness { rows }
}

#[derive(Deserialize)]
struct Decl {
    #[serde(default)]
    ci_entry: Option<String>,
}

fn steps(text: &str) -> usize {
    text.lines()
        .filter(|l| l.split_whitespace().next() == Some("step") && l.split_whitespace().nth(1).is_some())
        .count()
}

fn ci(f: &dyn Facts, repo: &str) -> Check {
    const NAME: &str = "ci";
    match f.at_landing(repo, DECL) {
        Err(e) => return red(NAME, format!("cannot read {DECL} at the landing ref: {e}")),
        Ok(Some(text)) => match spira_config::parse_foreign::<Decl>(&text) {
            Err(e) => return red(NAME, format!("{DECL} at the landing ref does not parse: {e}")),
            Ok(Decl {
                ci_entry: Some(c),
            }) if !c.trim().is_empty() => {
                return green(NAME, format!("{DECL} declares ci_entry {:?}", c.trim()))
            }
            Ok(_) => {}
        },
        Ok(None) => {}
    }
    match f.at_landing(repo, GATE_STEPS) {
        Err(e) => red(NAME, format!("cannot read {GATE_STEPS} at the landing ref: {e}")),
        Ok(Some(text)) if steps(&text) > 0 => {
            green(NAME, format!("{GATE_STEPS} declares {} steps", steps(&text)))
        }
        Ok(_) => red(
            NAME,
            format!("the landing ref declares no ci_entry in {DECL} and no step in {GATE_STEPS}"),
        ),
    }
}

/// The landing ref's own identity: no members, on the lane's image, harness and selection.
pub fn base_identity(repo: &str, lane: &Lane) -> PassIdentity {
    PassIdentity {
        repo: repo.into(),
        members: vec![],
        base: lane.landing_ref.clone(),
        image_digest: lane.image_digest.clone(),
        harness_version: lane.harness_version.clone(),
        selection: lane.selection.clone(),
    }
}

fn summary(p: &Ports, id: &PassIdentity) -> Result<Option<Summary>, String> {
    let key = summary_key(id);
    p.store
        .get(&key)
        .map_err(|e| format!("{key}: {e}"))?
        .map(|i| serde_json::from_value(i.value.data).map_err(|e| format!("{key}: {e}")))
        .transpose()
}

/// The commit a landed round put under the landing ref: the sha its landing names, else its head.
fn landed_sha(row: &BatchRow) -> Option<String> {
    if !matches!(row.state, BatchState::Landed | BatchState::Deploying) {
        return None;
    }
    row.reason
        .as_deref()
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_string)
        .or_else(|| row.head.clone())
}

/// Every round directly under `batches/<repo>/`, by batch id.
fn rounds_of(p: &Ports, repo: &str) -> Result<Vec<(String, crate::resources::Round, u64)>, String> {
    let prefix = round_key(repo, "");
    let (items, _) = p
        .store
        .list(&[prefix.as_str()], &Selector::Everything)
        .map_err(|e| format!("{prefix}: {e}"))?;
    items
        .into_iter()
        .filter_map(|i| {
            let batch = i.key.strip_prefix(&prefix).filter(|b| !b.contains('/'))?.to_string();
            Some(
                serde_json::from_value(i.value.data)
                    .map(|r| (batch, r, i.mod_rev.main))
                    .map_err(|e| format!("{}: {e}", i.key)),
            )
        })
        .collect()
}

fn base(p: &Ports, repo: &str) -> Check {
    const NAME: &str = "base";
    let lane = match current_lane(p, repo) {
        Err(e) => return red(NAME, e),
        Ok(l) => l,
    };
    let at = format!("{} on image {}", lane.landing_ref, lane.image_digest);
    match summary(p, &base_identity(repo, &lane)) {
        Err(e) => return red(NAME, e),
        Ok(Some(s)) if s.class == Class::Green => {
            return green(NAME, format!("a base probe was green at {at}"))
        }
        Ok(Some(s)) if matches!(s.class, Class::Red { .. } | Class::Flaky) => {
            return red(
                NAME,
                format!("the latest base probe at {at} was {}", s.class.word()),
            )
        }
        Ok(_) => {}
    }
    let rounds = match rounds_of(p, repo) {
        Ok(r) => r,
        Err(e) => return red(NAME, format!("cannot read the landed rounds: {e}")),
    };
    for (batch, r, _) in rounds {
        if landed_sha(&r.row).as_deref() != Some(lane.landing_ref.as_str()) {
            continue;
        }
        let mut members: Vec<Member> = r.members.clone();
        members.sort();
        let id = PassIdentity {
            members,
            base: r
                .tested_base
                .clone()
                .or(r.row.base.clone())
                .unwrap_or_default(),
            ..base_identity(repo, &lane)
        };
        match summary(p, &id) {
            Err(e) => return red(NAME, e),
            Ok(Some(s)) if s.class == Class::Green => {
                return green(NAME, format!("round {batch} landed {at} and was green there"))
            }
            Ok(_) => {}
        }
    }
    red(
        NAME,
        format!("no green verdict at {at}: no green base probe and no landed round green there (round-controller cutover-check --probe-base {repo})"),
    )
}

fn submitted(f: &dyn Facts, repo: &str) -> Check {
    const NAME: &str = "submitted";
    match f.submitted(repo) {
        Err(e) => red(NAME, format!("cannot read the SUBMITTED beads: {e}")),
        Ok(0) => red(NAME, "no SUBMITTED bead"),
        Ok(n) => green(NAME, format!("{n} SUBMITTED")),
    }
}

#[derive(Deserialize)]
struct RowState {
    state: BatchState,
}

/// Every round record directly under `batches/<repo>/`, with its key.
fn every_round(p: &Ports) -> Result<Vec<(String, String, BatchState)>, String> {
    let (items, _) = p
        .store
        .list(&["batches/"], &Selector::Everything)
        .map_err(|e| format!("batches/: {e}"))?;
    let mut out = Vec::new();
    for i in items {
        let Some((repo, batch)) = i.key.strip_prefix("batches/").and_then(|k| k.split_once('/'))
        else {
            continue;
        };
        if batch.contains('/') {
            continue;
        }
        let s: RowState =
            serde_json::from_value(i.value.data).map_err(|e| format!("{}: {e}", i.key))?;
        out.push((repo.to_string(), batch.to_string(), s.state));
    }
    Ok(out)
}

fn rounds(p: &Ports) -> Check {
    const NAME: &str = "rounds";
    match every_round(p) {
        Err(e) => red(NAME, format!("cannot read the rounds: {e}")),
        Ok(all) => {
            let live: Vec<String> = all
                .iter()
                .filter(|(_, _, s)| !s.is_terminal())
                .map(|(r, b, s)| format!("{r}/{b} {}", s.as_str()))
                .collect();
            if live.is_empty() {
                green(NAME, "no non-terminal round")
            } else {
                red(NAME, format!("{} non-terminal: {}", live.len(), live.join(", ")))
            }
        }
    }
}

fn queue_files(f: &dyn Facts) -> Check {
    const NAME: &str = "queue-files";
    match f.queue_files() {
        Err(e) => red(NAME, format!("cannot read the queue directory: {e}")),
        Ok(v) if v.is_empty() => green(NAME, "no open or prepared file"),
        Ok(v) => red(NAME, v.join(" ")),
    }
}

/// The first round a repo landed after the revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landed {
    pub batch: String,
    pub sha: String,
    pub rev: u64,
}

/// Each round repo with the first round that landed after `rev`; `Err` when that cannot be read.
pub type SinceRev = Vec<(String, Result<Option<Landed>, String>)>;

pub fn since_rev(p: &Ports, f: &dyn Facts, rev: u64) -> Result<SinceRev, String> {
    let repos = f.round_repos()?;
    Ok(repos
        .into_iter()
        .map(|r| {
            let l = landed_since(p, &r, rev);
            (r, l)
        })
        .collect())
}

pub fn render_since(rows: &SinceRev) -> (String, String, i32) {
    let (mut out, mut err, mut code) = (String::new(), String::new(), 0);
    for (repo, l) in rows {
        match l {
            Ok(Some(l)) => out.push_str(&format!("{repo} {} {}\n", l.batch, l.sha)),
            Ok(None) => {
                out.push_str(&format!("{repo} none\n"));
                code = 1;
            }
            Err(e) => {
                out.push_str(&format!("{repo} none\n"));
                err.push_str(&format!("{repo}: {e}\n"));
                code = 1;
            }
        }
    }
    (out, err, code)
}

fn landed_since(p: &Ports, repo: &str, rev: u64) -> Result<Option<Landed>, String> {
    let mut first: Option<Landed> = None;
    for (batch, r, mod_rev) in rounds_of(p, repo)? {
        let Some(sha) = landed_sha(&r.row) else {
            continue;
        };
        let at = landed_at(p, &batch, mod_rev)?;
        if at > rev && first.as_ref().is_none_or(|f| at < f.rev) {
            first = Some(Landed { batch, sha, rev: at });
        }
    }
    Ok(first)
}

/// The revision a landed round's ref moved at: its applied FastForward, from its history entries
/// and the audit each one's txn carries in `events/<rev>`. The controller records a landing it
/// finds already under the ref in one write with no audit, so with none the record's own
/// revision is the landing's.
fn landed_at(p: &Ports, batch: &str, mod_rev: u64) -> Result<u64, String> {
    let prefix = format!("history/{batch}/");
    let (items, _) = p
        .store
        .list(&[prefix.as_str()], &Selector::Everything)
        .map_err(|e| format!("{prefix}: {e}"))?;
    let mut revs: Vec<u64> = items.iter().map(|i| i.mod_rev.main).collect();
    revs.sort_unstable();
    revs.dedup();
    for main in revs {
        let key = events_key(main);
        let Some(ev) = p.store.get(&key).map_err(|e| format!("{key}: {e}"))? else {
            continue;
        };
        if fast_forward_in(&ev.value.data, batch) {
            return Ok(main);
        }
    }
    Ok(mod_rev)
}

fn fast_forward_in(event: &serde_json::Value, batch: &str) -> bool {
    if event.get("refused").and_then(|v| v.as_bool()) != Some(false) {
        return false;
    }
    event
        .pointer("/evidence/then")
        .and_then(|t| t.as_array())
        .into_iter()
        .flatten()
        .any(|a| {
            let s = |k: &str| a.get(k).and_then(|v| v.as_str());
            s("machine") == Some("batch")
                && s("lc_key") == Some(batch)
                && s("event") == Some("FastForward")
                && a.get("applied").and_then(|v| v.as_bool()) == Some(true)
        })
}

/// What a base probe recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probed {
    pub identity: PassIdentity,
    pub class: Class,
    pub attempt: u32,
    /// The VM outlived its teardown, so the slot is left to lapse rather than given back.
    pub slot_kept: bool,
}

impl Probed {
    pub fn render(&self) -> String {
        format!(
            "probe={} repo={} base={} image={} attempt={}{}\n",
            self.class.word(),
            self.identity.repo,
            self.identity.base,
            self.identity.image_digest,
            self.attempt,
            if self.slot_kept {
                " slot=kept (the round VM outlived its teardown; the slot lapses with its TTL)"
            } else {
                ""
            }
        )
    }

    pub fn code(&self) -> i32 {
        i32::from(self.class != Class::Green)
    }
}

/// The lane, only while it names the commit git's landing ref is at: a lane the ref moved past
/// describes a base nobody is landing on.
fn current_lane(p: &Ports, repo: &str) -> Result<Lane, String> {
    let l = lane(p, repo).map_err(|r| r.to_string())?.ok_or_else(|| {
        format!("no lane record at lanes/{repo}: the current image digest is unknown")
    })?;
    let at = p
        .git
        .landing_sha(repo)
        .map_err(|e| format!("cannot read {repo}'s landing ref: {e}"))?;
    if at != l.landing_ref {
        return Err(format!(
            "lanes/{repo} names {} but {repo}'s landing ref is at {at}: the lane is stale",
            l.landing_ref
        ));
    }
    Ok(l)
}

pub fn probe_target(p: &Ports, repo: &str) -> Result<Lane, String> {
    current_lane(p, repo)
}

/// The harness's own image: a tree carrying it is judged by `spira/test-*.sh`, not its declaration.
const HARNESS_IMAGE: &str = "spira/testenv/Containerfile";

#[derive(Deserialize)]
struct Tiers {
    #[serde(default)]
    tier: BTreeMap<String, serde::de::IgnoredAny>,
}

/// The suites a selection names at the landing ref: its own list, or for `all` the tree's whole
/// corpus, as testenv would accept it: a repository's declared tiers, else the harness's suites.
pub fn probe_suites(f: &dyn Facts, repo: &str, selection: &str) -> Result<Vec<String>, String> {
    if selection != "all" {
        let named: Vec<String> = selection
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        return if named.is_empty() {
            Err(format!("the lane's selection {selection:?} names no suite"))
        } else {
            Ok(named)
        };
    }
    let decl = f.at_landing(repo, DECL)?;
    let own = decl.is_some() && f.at_landing(repo, HARNESS_IMAGE)?.is_none();
    let suites: Vec<String> = match decl {
        Some(text) if own => spira_config::parse_foreign::<Tiers>(&text)
            .map_err(|e| format!("{DECL} at the landing ref does not parse: {e}"))?
            .tier
            .into_keys()
            .collect(),
        _ => f
            .names_at_landing(repo, "spira")?
            .into_iter()
            .filter(|n| n.starts_with("test-") && n.ends_with(".sh"))
            .collect(),
    };
    if suites.is_empty() {
        return Err(format!("{repo}'s landing ref declares no suite to probe"));
    }
    Ok(suites)
}

/// The base run on the landing ref: the round VM runs `suites` on that tree, under a hand slot
/// renewed while it runs, and the verdict is recorded as an attempt under the base identity.
/// `unit` is what the slot names as its holder, recorded before the VM starts, so a lapsed slot
/// is granted again only on proof that it is dead.
pub fn probe_base<S: SubPasses>(
    p: &Ports,
    env: &RunEnv,
    subs: &mut S,
    lane: (&str, &Lane),
    suites: &[String],
    unit: &str,
) -> Result<Probed, String> {
    let (repo, lane) = lane;
    let id = base_identity(repo, lane);
    let mut slot = grant_for_hand(p, PROBE_HOLDER).map_err(|e| e.to_string())?;
    if let Err(e) = slot.record_holder(p, unit, None) {
        let why = format!(
            "cannot record {unit} as the holder of slot {}: {e:?}",
            slot.slot()
        );
        let _ = slot.release(p);
        return Err(why);
    }
    let ran = run_corpus(p, subs, suites, &slot);
    let gone = match subs.vm() {
        None => true,
        Some(_) => {
            subs.close();
            let mut keep = keeper(p, &slot);
            match subs.vm() {
                None => true,
                Some(c) => {
                    wait_gone(p, env, c, env.teardown_ms, &mut keep)
                        || kill_child(p, env, c, &mut keep) != Killed::Lingering
                }
            }
        }
    };
    if gone {
        let _ = slot.release(p);
    }
    let (class, said) = ran?;
    let sub_passes = match &class {
        Class::Fault { .. } => vec![],
        c => vec![SubPass {
            kind: SubPassKind::Base,
            green: *c == Class::Green,
        }],
    };
    let a = Attempt {
        class: class.clone(),
        initiated_by: InitiatedBy::Judge,
        sub_passes,
        evidence: format!(
            "base probe of {repo} at {} on image {}, selection {}: {said}",
            lane.landing_ref, lane.image_digest, lane.selection
        ),
        at: p.clock.now_ms(),
    };
    let attempt = p.ledger.record_attempt(p, &id, PROBE_HOLDER, &a)?;
    Ok(Probed {
        identity: id,
        class,
        attempt,
        slot_kept: !gone,
    })
}

/// Renews the slot each renewal interval; a renewal error is left to the TTL, as the hold lapses.
fn keeper<'a>(p: &'a Ports<'a>, slot: &'a SlotGrant) -> impl FnMut() + 'a {
    let mut next = p.clock.now_ms();
    move || {
        let now = p.clock.now_ms();
        if now >= next {
            next = now + p.leases.renew_every_ms();
            let _ = slot.renew(p);
        }
    }
}

/// The corpus on the base tree: green only when every suite reported green; a red names its
/// suites; a suite that never reported, or a harness fault, is no verdict.
fn run_corpus<S: SubPasses>(
    p: &Ports,
    subs: &mut S,
    suites: &[String],
    slot: &SlotGrant,
) -> Result<(Class, String), String> {
    subs.set_members(&[]);
    subs.start_main(MainKind::Corpus, suites)
        .map_err(|e| format!("the base run did not start: {e}"))?;
    let mut seen: BTreeMap<String, bool> = BTreeMap::new();
    let mut next = p.clock.now_ms();
    let end = loop {
        match subs.poll_main() {
            Err(e) => return Ok((Class::Fault { cause: Cause::Vm }, e)),
            Ok(poll) => {
                seen.extend(poll.results);
                if let Some(end) = poll.done {
                    break end;
                }
            }
        }
        let now = p.clock.now_ms();
        if now >= next {
            next = now + p.leases.renew_every_ms();
            if let Err(e) = slot.renew(p) {
                return Err(format!("lost the runner slot during the base run: {e:?}"));
            }
        }
        subs.wait();
    };
    if end == MainEnd::WorkspaceBuild {
        return Ok((
            Class::Red {
                step: "workspace-build".into(),
            },
            "the base tree did not build".into(),
        ));
    }
    let red: Vec<&str> = suites
        .iter()
        .filter(|s| seen.get(*s) == Some(&false))
        .map(String::as_str)
        .collect();
    let missing: Vec<&str> = suites
        .iter()
        .filter(|s| !seen.contains_key(*s))
        .map(String::as_str)
        .collect();
    Ok(if !red.is_empty() {
        (
            Class::Red {
                step: red.join(","),
            },
            format!("red: {}", red.join(" ")),
        )
    } else if !missing.is_empty() {
        (
            Class::Fault { cause: Cause::Vm },
            format!("no verdict for {}", missing.join(" ")),
        )
    } else {
        (Class::Green, format!("{} suites green", suites.len()))
    })
}

#[cfg(test)]
mod tests;
