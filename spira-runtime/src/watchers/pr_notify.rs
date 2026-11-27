//! OPENED, FAIL RED, GREEN, MERGED, CLOSED and `no PR` for the PRs the harness opened, read from
//! its own batch and publish records and from the PRs already tracked; and one deploy verdict,
//! DEPLOYED, DEPLOY FAILED or NOT LIVE, for each merge or forge tip move. Each tick asks the world
//! for one snapshot; the decision over snapshot and carried state is `decide`, which does no IO.

use std::collections::BTreeMap;

use runtime::event::*;
use runtime::rows::Part;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{answers_observe, carried, iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.pr-notify";
pub const TICK: EventKindTag = "pr-notify.tick";
pub const STATE: EventKindTag = "pr-notify.state";
pub const OBSERVE: &str = "pr-notify.observe";
/// A publish PR's opened, red, merged or closed, handed over by the task that saw it.
pub const PR_RESULT: EventKindTag = "publish.pr_result";
pub const OPERATOR_NOTE: &str = "operator.note";
/// Publishes the `pr/<repo>/<n>` topics its input lists.
pub const PUBLISH: &str = "pr-notify.publish";
pub const NAME: &str = "pr-notify";

const ROW_PARTS: &[Part] = &[Part::Map { name: "repos", drop: &[] }];

const OBSERVE_DEADLINE_MS: u64 = 120_000;
const DEADLINE_MS: u64 = 10_000;
/// How many judged commits a repo remembers, so a merge seen after its tip's verdict gets no second.
const JUDGED_KEPT: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrNotify {
    pub interval_ms: u64,
    pub deploy_giveup_secs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracked {
    pub status: String,
    pub title: String,
    /// When `status` was first seen.
    #[serde(default)]
    pub at: u64,
}

/// A landed commit awaiting its deploy verdict; `n` is 0 when no PR carried it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deploy {
    pub sha: String,
    pub n: u64,
    pub at: u64,
    pub title: String,
    pub beads: Vec<String>,
}

/// What one [`PR_RESULT`] carries: `result` is `opened`, `red`, `merged` or `closed`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrResult {
    pub repo: String,
    pub pr: u64,
    pub result: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub fail: String,
}

/// What a hand-off already said of one PR, so the poll does not say it again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heard {
    pub opened: bool,
    pub red: u32,
    pub terminal: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCarry {
    pub prs: BTreeMap<u64, Tracked>,
    #[serde(default)]
    pub said: BTreeMap<u64, Heard>,
    pub deploys: Vec<Deploy>,
    pub strays: Vec<String>,
    /// The last forge tip seen; empty before the first.
    #[serde(default)]
    pub tip: String,
    /// Commits already given a verdict, oldest first.
    #[serde(default)]
    pub judged: Vec<String>,
}

impl RepoCarry {
    /// Whether `tip` is a move off the last tip seen onto a commit nothing has queued or judged.
    pub fn moved_to(&self, tip: &str) -> bool {
        !self.tip.is_empty() && self.tip != tip && !self.judged.iter().any(|s| s == tip) && !self.deploys.iter().any(|d| d.sha == tip)
    }
}

/// Carried between ticks, as the body of the latest `pr-notify.state` event; it is also the
/// question each observe asks: which tracked PRs and queued merges does the forge now settle.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub repos: BTreeMap<String, RepoCarry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPr {
    pub number: u64,
    pub status: String,
    pub title: String,
    pub fail: String,
}

/// What the forge said of a tracked PR that left the open list. `beads` are the `- ` bullets of
/// its body.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Final {
    pub state: String,
    pub sha: String,
    pub beads: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub conclusion: String,
    pub secs: u64,
    pub url: String,
}

/// What the deploy workflow's listing said of one commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeployRun {
    Completed(Run),
    NotYet,
    NoWorkflow,
}

/// One repository as the forge showed it. `open` is `None` when the forge could not be asked,
/// which is never the same as no open PRs; an absent `finals` or `runs` entry is a question the
/// forge did not answer; `strays` is `None` where the check does not apply or could not run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoObs {
    pub open: Option<Vec<OpenPr>>,
    pub finals: BTreeMap<u64, Final>,
    pub runs: BTreeMap<String, DeployRun>,
    pub strays: Option<Vec<String>>,
    #[serde(default)]
    pub slug: String,
    /// The origin URL of a repo that is not on GitHub.
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub tip: Option<String>,
    /// Per commit, the repo's own `after_landing` text, shown and never parsed.
    #[serde(default)]
    pub declared: BTreeMap<String, String>,
    /// The home repo, which gets no deploy verdict.
    #[serde(default)]
    pub home: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub repos: Vec<(String, RepoObs)>,
}

/// A merge outcome or deploy verdict for the operator's mailbox.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub subject: String,
    pub body: String,
}

/// One PR's new state: `OPENED`, `FAIL RED`, `GREEN`, `MERGED`, `CLOSED`, `DEPLOYED` or
/// `DEPLOY FAILED`. `sha` is the merge commit, empty before there is one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub repo: String,
    pub pr: u64,
    pub state: &'static str,
    pub sha: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Said {
    pub lines: Vec<String>,
    pub notes: Vec<Note>,
    pub changes: Vec<Change>,
}

impl Said {
    fn change(&mut self, repo: &str, pr: u64, state: &'static str, sha: &str) {
        self.changes.push(Change { repo: repo.into(), pr, state, sha: sha.into() });
    }

    /// The topics to publish as of `at`: each changed PR's latest state, since the topic keeps
    /// only one.
    pub fn topics(&self, at: u64) -> Topics {
        self.changes.iter().map(|c| topic(&c.repo, c.pr, c.state, &c.sha, at)).collect()
    }
}

/// Each topic and its payload.
pub type Topics = BTreeMap<String, Value>;

fn topic(repo: &str, pr: u64, state: &str, sha: &str, at: u64) -> (String, Value) {
    (format!("pr/{repo}/{pr}"), json!({ "state": state, "sha": sha, "at": at }))
}

fn open_state(status: &str) -> &'static str {
    match status {
        "red" => "FAIL RED",
        "green" => "GREEN",
        _ => "OPENED",
    }
}

/// The state of every PR the carry still tracks: those open, then those merged and awaiting
/// their deploy. A PR a hand-off has called finished is left out: its status here is stale.
pub fn carried_topics(carry: &Carry) -> Topics {
    let mut out = Topics::new();
    for (repo, rc) in &carry.repos {
        for (n, t) in &rc.prs {
            let heard = rc.said.get(n);
            if !heard.is_some_and(|h| h.terminal) {
                let state = if heard.is_some_and(|h| h.red > 0) { "FAIL RED" } else { open_state(&t.status) };
                out.extend([topic(repo, *n, state, "", t.at)]);
            }
        }
        out.extend(rc.deploys.iter().filter(|d| d.n != 0).map(|d| topic(repo, d.n, "MERGED", &d.sha, d.at)));
    }
    out
}

/// Status and failing check names from a PR's check runs, each `(name, STATUS, CONCLUSION)`. A PR
/// with no checks is pending, and so is one with any check unfinished or not successful.
pub fn classify(checks: &[(String, String, String)]) -> (&'static str, String) {
    const BAD: [&str; 5] = ["FAILURE", "CANCELLED", "TIMED_OUT", "STALE", "ACTION_REQUIRED"];
    let bad: Vec<&str> = checks.iter().filter(|(_, _, c)| BAD.contains(&c.as_str())).map(|(n, _, _)| n.as_str()).collect();
    if checks.is_empty() {
        ("pending", String::new())
    } else if !bad.is_empty() {
        ("red", bad.join(","))
    } else if checks.iter().all(|(_, s, c)| s == "COMPLETED" && c == "SUCCESS") {
        ("green", String::new())
    } else {
        ("pending", String::new())
    }
}

fn bead_lines(beads: &[String]) -> String {
    beads.iter().map(|l| l.replacen("- ", "- bead ", 1)).collect::<Vec<_>>().join("\n")
}

fn beads_block(beads: &str) -> String {
    if beads.is_empty() {
        String::new()
    } else {
        format!("\nBeads:\n{beads}\n")
    }
}

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

fn merged(repo: &str, n: u64, title: &str, f: &Final, now: u64, rc: &mut RepoCarry, say: &mut Said) {
    let beads = bead_lines(&f.beads);
    let at = if f.sha.is_empty() { String::new() } else { format!(" at {}", f.sha) };
    let tag = if f.sha.is_empty() { String::new() } else { format!(" {}", short(&f.sha)) };
    say.notes.push(Note { subject: format!("MERGED #{n} [{repo}]{tag}"), body: format!("PR #{n} {title} [{repo}] merged{at}.\n{}", beads_block(&beads)) });
    say.change(repo, n, "MERGED", &f.sha);
    if f.sha.is_empty() || rc.judged.contains(&f.sha) {
        return;
    }
    let d = Deploy { sha: f.sha.clone(), n, at: now, title: title.replace('\t', " "), beads: f.beads.iter().map(|l| l.replacen("- ", "- bead ", 1)).collect() };
    match rc.deploys.iter_mut().find(|held| held.sha == f.sha) {
        Some(held) if held.n == 0 => *held = Deploy { at: held.at, ..d },
        Some(_) => {}
        None => rc.deploys.push(d),
    }
}

/// Queues the deploy of a forge tip no merged PR brought: a direct push or a hand landing.
fn follow_tip(obs: &RepoObs, now: u64, rc: &mut RepoCarry) {
    let Some(tip) = &obs.tip else { return };
    if rc.moved_to(tip) {
        rc.deploys.push(Deploy { sha: tip.clone(), at: now, ..Deploy::default() });
    }
    rc.tip = tip.clone();
}

fn transitions(repo: &str, open: &[OpenPr], obs: &RepoObs, now: u64, rc: &mut RepoCarry, say: &mut Said) {
    let prev = std::mem::take(&mut rc.prs);
    rc.said.retain(|n, _| prev.contains_key(n) || open.iter().any(|p| p.number == *n));
    let mut cur = BTreeMap::new();
    for p in open {
        let old = prev.get(&p.number);
        let heard = rc.said.entry(p.number).or_default();
        if old.is_none() && !std::mem::take(&mut heard.opened) {
            say.lines.push(format!("OPENED #{} {} [{repo}]", p.number, p.title));
        }
        if old.is_none_or(|o| o.status != p.status) {
            say.change(repo, p.number, open_state(&p.status), "");
            match p.status.as_str() {
                "red" if heard.red > 0 => heard.red -= 1,
                "red" => say.lines.push(format!("FAIL RED #{} {} [{repo}]{}", p.number, p.title, if p.fail.is_empty() { String::new() } else { format!(": {}", p.fail) })),
                "green" => say.lines.push(format!("GREEN #{} {} [{repo}]: lands automatically", p.number, p.title)),
                _ => {}
            }
            if p.status != "red" {
                heard.red = 0;
            }
        }
        if *heard == Heard::default() {
            rc.said.remove(&p.number);
        }
        let at = old.filter(|o| o.status == p.status && o.at > 0).map_or(now, |o| o.at);
        cur.insert(p.number, Tracked { status: p.status.clone(), title: p.title.clone(), at });
    }
    for (m, t) in &prev {
        if open.iter().any(|p| p.number == *m) {
            continue;
        }
        let heard = rc.said.entry(*m).or_default();
        let told = std::mem::replace(&mut heard.terminal, true);
        match obs.finals.get(m) {
            Some(f) if f.state == "MERGED" => {
                if !told {
                    say.lines.push(format!("MERGED #{m} {} [{repo}]", t.title));
                }
                merged(repo, *m, &t.title, f, now, rc, say);
            }
            Some(f) if f.state == "CLOSED" => {
                say.change(repo, *m, "CLOSED", "");
                if !told {
                    say.lines.push(format!("CLOSED #{m} {} [{repo}]", t.title));
                }
            }
            _ => {
                heard.terminal = told;
                if *heard == Heard::default() {
                    rc.said.remove(m);
                }
                cur.insert(*m, t.clone());
            }
        }
    }
    rc.prs = cur;
}

/// What a hand-off calls for, unless the poll or an earlier hand-off already said it.
fn hear(carry: &mut Carry, r: &PrResult) -> Option<Said> {
    let rc = carry.repos.entry(r.repo.clone()).or_default();
    let held = rc.prs.get(&r.pr);
    let title = held.map_or(r.title.as_str(), |t| t.title.as_str()).to_string();
    let n = r.pr;
    let repo = &r.repo;
    let heard = rc.said.entry(n).or_default();
    let (state, line) = match r.result.as_str() {
        "opened" if held.is_none() && !heard.opened => {
            heard.opened = true;
            ("OPENED", format!("OPENED #{n} {title} [{repo}]"))
        }
        "red" if held.is_none_or(|t| t.status != "red") => {
            heard.red += 1;
            ("FAIL RED", format!("FAIL RED #{n} {title} [{repo}]{}", if r.fail.is_empty() { String::new() } else { format!(": {}", r.fail) }))
        }
        "merged" | "closed" if !heard.terminal => {
            heard.terminal = true;
            (if r.result == "merged" { "MERGED" } else { "CLOSED" }, format!("{} #{n} {title} [{repo}]", r.result.to_uppercase()))
        }
        _ => {
            if *heard == Heard::default() {
                rc.said.remove(&n);
            }
            return None;
        }
    };
    let mut said = Said { lines: vec![line], ..Said::default() };
    said.change(repo, n, state, "");
    Some(said)
}

fn deploys(repo: &str, obs: &RepoObs, now: u64, giveup: u64, rc: &mut RepoCarry, say: &mut Said) {
    let queued = std::mem::take(&mut rc.deploys);
    for d in queued {
        let waited = now.saturating_sub(d.at);
        if obs.home {
            if waited <= giveup {
                rc.deploys.push(d);
            }
            continue;
        }
        match (&obs.origin, obs.runs.get(&d.sha)) {
            (Some(url), _) => not_live(repo, &d, &format!("origin {url} is not a GitHub repository; no deploy run is observable"), obs, say),
            (None, Some(DeployRun::Completed(run))) => completed(repo, &d, run, say),
            (None, Some(DeployRun::NoWorkflow)) => not_live(repo, &d, &format!("no deploy.yml on {}", obs.slug), obs, say),
            (None, Some(DeployRun::NotYet)) if waited > giveup => not_live(repo, &d, &format!("no completed deploy.yml run for {} after {waited}s", short(&d.sha)), obs, say),
            _ => {
                rc.deploys.push(d);
                continue;
            }
        }
        rc.judged.push(d.sha);
        rc.judged.drain(..rc.judged.len().saturating_sub(JUDGED_KEPT));
    }
}

fn completed(repo: &str, d: &Deploy, run: &Run, say: &mut Said) {
    let verdict = if run.conclusion == "success" { "DEPLOYED" } else { "DEPLOY FAILED" };
    let (n, title, sha, concl) = (d.n, &d.title, &d.sha, &run.conclusion);
    let (subject, headline, commit, line) = if n == 0 {
        let named = format!("{verdict} [{repo}] {}", short(sha));
        (named.clone(), format!("{verdict}: [{repo}]"), "Commit", named)
    } else {
        (format!("{verdict} #{n} [{repo}] {}", short(sha)), format!("{verdict}: PR #{n} {title} [{repo}]"), "Merge", format!("{verdict} #{n} {title} [{repo}]"))
    };
    say.notes.push(Note {
        subject,
        body: format!("{headline}\n{commit}: {sha}\nRun: {concl} in {}m{}s\n{}\n{}", run.secs / 60, run.secs % 60, run.url, beads_block(&d.beads.join("\n"))),
    });
    say.lines.push(format!("{line}: {concl} in {}s {}", run.secs, run.url));
    if n != 0 {
        say.change(repo, n, verdict, sha);
    }
}

/// Says what was observed instead of a run. It is no PR topic state.
fn not_live(repo: &str, d: &Deploy, observed: &str, obs: &RepoObs, say: &mut Said) {
    let commit = short(&d.sha);
    let headline = format!("NOT LIVE [{repo}] {commit}: {observed}");
    let (tag, pr, aside) = if d.n == 0 {
        (String::new(), String::new(), String::new())
    } else {
        (format!(" #{}", d.n), format!("PR #{} {}\n", d.n, d.title), format!(" (PR #{} {})", d.n, d.title))
    };
    let declared = obs.declared.get(&d.sha).map(|text| format!("Declared deploy: {text}\n")).unwrap_or_default();
    say.notes.push(Note { subject: format!("NOT LIVE{tag} [{repo}] {commit}"), body: format!("{headline}\n{pr}{declared}{}", beads_block(&d.beads.join("\n"))) });
    say.lines.push(format!("{headline}{aside}"));
}

/// The messages a snapshot calls for, and the state to carry into the next tick.
pub fn decide(cfg: &PrNotify, carry: &Carry, snap: &Snapshot, now: u64) -> (Carry, Said) {
    let mut c = carry.clone();
    let mut say = Said::default();
    for (repo, obs) in &snap.repos {
        let mut rc = c.repos.remove(repo).unwrap_or_default();
        if let Some(open) = &obs.open {
            transitions(repo, open, obs, now, &mut rc, &mut say);
        }
        follow_tip(obs, now, &mut rc);
        deploys(repo, obs, now, cfg.deploy_giveup_secs, &mut rc, &mut say);
        if let Some(found) = &obs.strays {
            for b in found.iter().filter(|b| !rc.strays.contains(b)) {
                say.lines.push(format!("⚠ BRANCH {b}: no PR [{repo}]"));
            }
            rc.strays = found.clone();
        }
        c.repos.insert(repo.clone(), rc);
    }
    (c, say)
}

fn inbox(m: &str, now: u64) -> Command {
    Command::Effect(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:inbox:{}:{:016x}", now, runtime::event::fnv1a(m)), format!("{} [watch:{NAME}] {m}", iso8601(now)), DEADLINE_MS))
}

fn publication(said: &Said, now: u64) -> Option<Command> {
    let topics = said.topics(now);
    let input = serde_json::to_string(&topics).unwrap_or_default();
    (!topics.is_empty()).then(|| Command::Effect(EffectSpec::keyed(PUBLISH, format!("{NAME}:publish:{now}:{:016x}", runtime::event::fnv1a(&input)), input, DEADLINE_MS)))
}

impl PrNotify {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("pr-notify/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for PrNotify {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, PR_RESULT, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == PR_RESULT {
            let Ok(r) = serde_json::from_str::<PrResult>(&ev.kind.body) else { return vec![] };
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            let mut next = carry.clone();
            let Some(said) = hear(&mut next, &r) else { return vec![] };
            let now = ev.at / 1000;
            let mut cmds = vec![];
            if next != carry {
                cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
            }
            cmds.extend(publication(&said, now));
            cmds.extend(said.lines.into_iter().map(|m| inbox(&m, now)));
            return cmds;
        }
        if ev.kind.tag == TICK {
            let input = serde_json::to_string(&carried::<Carry>(view, &ev.stream, STATE)).unwrap_or_default();
            return vec![
                Command::Probe(EffectSpec::probe(OBSERVE, input, OBSERVE_DEADLINE_MS)),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if !answers_observe(view, ev, OBSERVE) || ev.kind.tag != EFFECT_SUCCEEDED {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else { return vec![] };
        let now = ev.at / 1000;
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let (next, say) = decide(self, &carry, &snap, now);
        ctx.event().set("pr_notify.said", (say.lines.len() + say.notes.len()) as u64);
        let mut cmds = vec![];
        if next != carry {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        }
        cmds.extend(publication(&say, now));
        cmds.extend(say.lines.into_iter().map(|m| inbox(&m, now)));
        for n in say.notes {
            let input = serde_json::to_string(&n).unwrap_or_default();
            cmds.push(Command::Effect(EffectSpec::keyed(OPERATOR_NOTE, format!("{NAME}:note:{now}:{:016x}", runtime::event::fnv1a(&input)), input, DEADLINE_MS)));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PrNotify {
        PrNotify { interval_ms: 90_000, deploy_giveup_secs: 100 }
    }

    fn pr(n: u64, status: &str, fail: &str) -> OpenPr {
        OpenPr { number: n, status: status.into(), title: format!("t{n}"), fail: fail.into() }
    }

    fn one(obs: RepoObs) -> Snapshot {
        Snapshot { repos: vec![("r".into(), obs)] }
    }

    fn open(prs: Vec<OpenPr>) -> RepoObs {
        RepoObs { open: Some(prs), ..RepoObs::default() }
    }

    fn check(n: &str, s: &str, c: &str) -> (String, String, String) {
        (n.into(), s.into(), c.into())
    }

    // covers: UC-operator-channel-38
    #[test_id(tunrqkp)]
    #[test]
    fn checks_classify_as_red_green_or_pending() {
        assert_eq!(classify(&[]), ("pending", String::new()));
        assert_eq!(classify(&[check("a", "COMPLETED", "SUCCESS"), check("b", "COMPLETED", "FAILURE"), check("c", "COMPLETED", "TIMED_OUT")]), ("red", "b,c".into()));
        assert_eq!(classify(&[check("a", "COMPLETED", "SUCCESS")]), ("green", String::new()));
        assert_eq!(classify(&[check("a", "COMPLETED", "SUCCESS"), check("b", "IN_PROGRESS", "")]).0, "pending");
        assert_eq!(classify(&[check("a", "COMPLETED", "NEUTRAL")]).0, "pending");
    }

    // covers: UC-operator-channel-38
    #[test_id(t73ul8g)]
    #[test]
    fn a_pr_is_reported_on_change_only() {
        let (c, say) = decide(&cfg(), &Carry::default(), &one(open(vec![pr(7, "pending", "")])), 1000);
        assert_eq!(say.lines, ["OPENED #7 t7 [r]"]);
        assert!(decide(&cfg(), &c, &one(open(vec![pr(7, "pending", "")])), 1090).1.lines.is_empty());
        let (c, say) = decide(&cfg(), &c, &one(open(vec![pr(7, "red", "build,lint")])), 1180);
        assert_eq!(say.lines, ["FAIL RED #7 t7 [r]: build,lint"]);
        assert_eq!(decide(&cfg(), &c, &one(open(vec![pr(7, "green", "")])), 1270).1.lines, ["GREEN #7 t7 [r]: lands automatically"]);
    }

    // covers: UC-operator-channel-38
    #[test_id(tr89u3c)]
    #[test]
    fn a_pr_first_seen_resolved_gets_opened_and_its_verdict() {
        let (_, say) = decide(&cfg(), &Carry::default(), &one(open(vec![pr(1, "red", "x")])), 1000);
        assert_eq!(say.lines, ["OPENED #1 t1 [r]", "FAIL RED #1 t1 [r]: x"]);
    }

    // covers: UC-operator-channel-38
    #[test_id(tk9oix7)]
    #[test]
    fn a_forge_that_did_not_answer_changes_nothing() {
        let (c, _) = decide(&cfg(), &Carry::default(), &one(open(vec![pr(7, "pending", "")])), 1000);
        let (after, say) = decide(&cfg(), &c, &one(RepoObs::default()), 1090);
        assert_eq!((after, say), (c, Said::default()));
    }

    #[test_id(taz846a)]
    #[test]
    fn a_pr_that_left_the_open_list_is_tracked_until_the_forge_says_what_became_of_it() {
        let (c, _) = decide(&cfg(), &Carry::default(), &one(open(vec![pr(7, "pending", "")])), 1000);
        let (c, say) = decide(&cfg(), &c, &one(open(vec![])), 1090);
        assert_eq!((c.repos["r"].prs.len(), say.lines.len()), (1, 0));
        let mut gone = open(vec![]);
        gone.finals.insert(7, Final { state: "OPEN".into(), ..Final::default() });
        assert_eq!(decide(&cfg(), &c, &one(gone), 1180).0.repos["r"].prs.len(), 1);
        let mut closed = open(vec![]);
        closed.finals.insert(7, Final { state: "CLOSED".into(), ..Final::default() });
        let (c, say) = decide(&cfg(), &c, &one(closed), 1270);
        assert_eq!((say.lines, c.repos["r"].prs.len()), (vec!["CLOSED #7 t7 [r]".to_string()], 0));
    }

    fn merged_carry() -> (Carry, Said) {
        let (c, _) = decide(&cfg(), &Carry::default(), &one(open(vec![pr(7, "green", "")])), 1000);
        let mut gone = open(vec![]);
        gone.finals.insert(7, Final { state: "MERGED".into(), sha: "0123456789abcdef".into(), beads: vec!["- sp-aaa — one".into()] });
        decide(&cfg(), &c, &one(gone), 1090)
    }

    #[test_id(t0zx5f6)]
    #[test]
    fn a_merge_mails_the_operator_and_queues_its_deploy_once() {
        let (c, say) = merged_carry();
        assert_eq!(say.lines, ["MERGED #7 t7 [r]"]);
        assert_eq!(say.notes[0].subject, "MERGED #7 [r] 012345678");
        assert_eq!(say.notes[0].body, "PR #7 t7 [r] merged at 0123456789abcdef.\n\nBeads:\n- bead sp-aaa — one\n");
        assert_eq!(c.repos["r"].deploys.len(), 1);
    }

    fn answered(sha: &str, run: DeployRun) -> RepoObs {
        RepoObs { slug: "o/r".into(), runs: BTreeMap::from([(sha.to_string(), run)]), ..open(vec![]) }
    }

    fn completed(conclusion: &str, secs: u64) -> DeployRun {
        DeployRun::Completed(Run { conclusion: conclusion.into(), secs, url: "http://u".into() })
    }

    #[test_id(tuep6ad)]
    #[test]
    fn a_deploy_is_reported_when_its_run_completes() {
        let (c, _) = merged_carry();
        let (after, say) = decide(&cfg(), &c, &one(answered("0123456789abcdef", completed("failure", 125))), 1100);
        assert_eq!(say.lines, ["DEPLOY FAILED #7 t7 [r]: failure in 125s http://u"]);
        assert_eq!(say.notes[0].body, "DEPLOY FAILED: PR #7 t7 [r]\nMerge: 0123456789abcdef\nRun: failure in 2m5s\nhttp://u\n\nBeads:\n- bead sp-aaa — one\n");
        assert!(after.repos["r"].deploys.is_empty());
    }

    #[test_id(t6zkgsb)]
    #[test]
    fn a_deploy_with_no_completed_run_by_the_giveup_is_reported_not_live() {
        let (c, _) = merged_carry();
        let waiting = || one(answered("0123456789abcdef", DeployRun::NotYet));
        let (c, say) = decide(&cfg(), &c, &waiting(), 1190);
        assert_eq!((c.repos["r"].deploys.len(), say), (1, Said::default()), "waiting is not giving up");
        let (c, say) = decide(&cfg(), &c, &waiting(), 1191);
        assert_eq!(say.lines, ["NOT LIVE [r] 012345678: no completed deploy.yml run for 012345678 after 101s (PR #7 t7)"]);
        assert_eq!(say.notes.len(), 1);
        assert_eq!(say.notes[0].subject, "NOT LIVE #7 [r] 012345678");
        assert_eq!(say.notes[0].body, "NOT LIVE [r] 012345678: no completed deploy.yml run for 012345678 after 101s\nPR #7 t7\n\nBeads:\n- bead sp-aaa — one\n");
        assert!(say.changes.is_empty(), "NOT LIVE is no topic state");
        assert_eq!(decide(&cfg(), &c, &waiting(), 1300).1, Said::default());
    }

    #[test_id(tpx68g5)]
    #[test]
    fn a_repo_without_deploy_yml_is_not_live_at_once() {
        let (c, _) = merged_carry();
        let (c, say) = decide(&cfg(), &c, &one(answered("0123456789abcdef", DeployRun::NoWorkflow)), 1091);
        assert_eq!(say.lines, ["NOT LIVE [r] 012345678: no deploy.yml on o/r (PR #7 t7)"]);
        assert!(c.repos["r"].deploys.is_empty());
    }

    const OLD: &str = "bdab763500000000";
    const NEW: &str = "70bf904e00000000";

    fn at_tip(tip: &str, runs: RepoObs) -> Snapshot {
        one(RepoObs { tip: Some(tip.into()), ..runs })
    }

    #[test_id(tzq8win)]
    #[test]
    fn a_forge_tip_moved_without_a_pr_queues_its_deploy() {
        let (c, say) = decide(&cfg(), &Carry::default(), &at_tip(OLD, open(vec![])), 1000);
        assert_eq!((say, c.repos["r"].deploys.len()), (Said::default(), 0), "the first tip seen is no landing");
        let (c, _) = decide(&cfg(), &c, &at_tip(NEW, open(vec![])), 1090);
        assert_eq!(c.repos["r"].deploys.iter().map(|d| (d.sha.as_str(), d.n)).collect::<Vec<_>>(), [(NEW, 0)]);
        assert!(carried_topics(&c).is_empty(), "no PR, no PR topic");
        let (c, say) = decide(&cfg(), &c, &at_tip(NEW, answered(NEW, completed("success", 61))), 1180);
        assert_eq!(say.lines, ["DEPLOYED [r] 70bf904e0: success in 61s http://u"]);
        assert_eq!(say.notes[0].subject, "DEPLOYED [r] 70bf904e0");
        assert_eq!(say.notes[0].body, "DEPLOYED: [r]\nCommit: 70bf904e00000000\nRun: success in 1m1s\nhttp://u\n");
        assert!(say.changes.is_empty());
        let (c, say) = decide(&cfg(), &c, &at_tip(NEW, open(vec![])), 1270);
        assert_eq!((say, c.repos["r"].deploys.len()), (Said::default(), 0));
    }

    fn merged_at(sha: &str) -> RepoObs {
        let mut gone = open(vec![]);
        gone.finals.insert(7, Final { state: "MERGED".into(), sha: sha.into(), beads: vec![] });
        gone
    }

    #[test_id(tykk8i6)]
    #[test]
    fn a_merge_seen_after_its_tips_verdict_is_not_judged_again() {
        let (c, _) = decide(&cfg(), &Carry::default(), &at_tip(OLD, open(vec![pr(7, "green", "")])), 1000);
        let (c, _) = decide(&cfg(), &c, &at_tip(NEW, RepoObs::default()), 1090);
        let (c, say) = decide(&cfg(), &c, &at_tip(NEW, RepoObs { runs: answered(NEW, completed("success", 1)).runs, ..RepoObs::default() }), 1180);
        assert_eq!(say.notes.len(), 1);
        let (c, say) = decide(&cfg(), &c, &at_tip(NEW, merged_at(NEW)), 1270);
        assert_eq!((say.notes.len(), c.repos["r"].deploys.len()), (1, 0), "only the MERGED note");
    }

    #[test_id(t039pa0)]
    #[test]
    fn a_merge_seen_while_its_tips_deploy_waits_gives_it_the_pr() {
        let (c, _) = decide(&cfg(), &Carry::default(), &at_tip(OLD, open(vec![pr(7, "green", "")])), 1000);
        let (c, _) = decide(&cfg(), &c, &at_tip(NEW, RepoObs::default()), 1090);
        let (c, _) = decide(&cfg(), &c, &at_tip(NEW, merged_at(NEW)), 1180);
        assert_eq!(c.repos["r"].deploys.iter().map(|d| (d.n, d.at)).collect::<Vec<_>>(), [(7, 1090)]);
    }

    #[test_id(to8uhwt)]
    #[test]
    fn a_tip_that_moves_with_the_merge_that_carried_it_is_queued_once_as_the_prs() {
        let (c, _) = decide(&cfg(), &Carry::default(), &at_tip(OLD, open(vec![pr(7, "green", "")])), 1000);
        let (c, _) = decide(&cfg(), &c, &at_tip(NEW, merged_at(NEW)), 1090);
        assert_eq!(c.repos["r"].deploys.iter().map(|d| d.n).collect::<Vec<_>>(), [7]);
    }

    // covers: UC-operator-channel-38
    #[test_id(tuoz8m2)]
    #[test]
    fn a_branch_without_a_pr_is_announced_once_while_it_stays() {
        let obs = RepoObs { strays: Some(vec!["spira/x".into()]), ..open(vec![]) };
        let (c, say) = decide(&cfg(), &Carry::default(), &one(obs.clone()), 1000);
        assert_eq!(say.lines, ["⚠ BRANCH spira/x: no PR [r]"]);
        let (c, say) = decide(&cfg(), &c, &one(obs), 1090);
        assert!(say.lines.is_empty());
        let gone = RepoObs { strays: Some(vec![]), ..open(vec![]) };
        assert!(decide(&cfg(), &c, &one(gone), 1180).0.repos["r"].strays.is_empty());
    }

    fn result(pr: u64, what: &str) -> PrResult {
        PrResult { repo: "r".into(), pr, result: what.into(), title: "event".into(), fail: String::new() }
    }

    fn heard(c: &mut Carry, r: &PrResult) -> Option<String> {
        hear(c, r).map(|said| said.lines.join("\n"))
    }

    #[test_id(ta3tsxc)]
    #[test]
    fn what_was_said_of_a_pr_maps_to_its_topic_and_payload() {
        let (c, say) = decide(&cfg(), &Carry::default(), &one(open(vec![pr(1, "red", "x")])), 1000);
        assert_eq!(say.topics(1000), Topics::from([("pr/r/1".to_string(), json!({ "state": "FAIL RED", "sha": "", "at": 1000 }))]), "the later state wins");
        let (c2, quiet) = decide(&cfg(), &c, &one(open(vec![pr(1, "red", "x")])), 1090);
        assert!(quiet.topics(1090).is_empty());
        assert_eq!(carried_topics(&c2), say.topics(1000), "the carry keeps when the state began");
        let (c, say) = merged_carry();
        let merged = Topics::from([("pr/r/7".to_string(), json!({ "state": "MERGED", "sha": "0123456789abcdef", "at": 1090 }))]);
        assert_eq!((say.topics(1090), carried_topics(&c)), (merged.clone(), merged));
        let mut done = open(vec![]);
        done.runs.insert("0123456789abcdef".into(), DeployRun::Completed(Run { conclusion: "success".into(), secs: 1, url: "u".into() }));
        let (c, say) = decide(&cfg(), &c, &one(done), 1100);
        assert_eq!(say.topics(1100)["pr/r/7"]["state"], "DEPLOYED");
        assert!(carried_topics(&c).is_empty());
        let said = hear(&mut Carry::default(), &result(9, "closed")).unwrap();
        assert_eq!(said.topics(5)["pr/r/9"]["state"], "CLOSED");
    }

    #[test_id(tpiiq79)]
    #[test]
    fn a_pr_carried_without_a_time_is_stamped_at_the_next_answered_poll() {
        let old: Tracked = serde_json::from_str(r#"{"status":"green","title":"t7"}"#).unwrap();
        let carry = Carry { repos: BTreeMap::from([("r".into(), RepoCarry { prs: BTreeMap::from([(7, old)]), ..RepoCarry::default() })]) };
        let (c, say) = decide(&cfg(), &carry, &one(open(vec![pr(7, "green", "")])), 1000);
        assert_eq!((carried_topics(&c)["pr/r/7"]["at"].as_u64(), say), (Some(1000), Said::default()));
    }

    #[test_id(tvl145q)]
    #[test]
    fn a_pr_a_hand_off_called_finished_is_not_republished_as_open() {
        let (mut c, _) = decide(&cfg(), &Carry::default(), &one(open(vec![pr(7, "green", "")])), 1000);
        assert_eq!(carried_topics(&c)["pr/r/7"]["state"], "GREEN");
        hear(&mut c, &result(7, "merged"));
        assert!(carried_topics(&c).is_empty());
    }

    #[test_id(tduzaiq)]
    #[test]
    fn a_terminal_hand_off_is_dropped_at_the_first_answered_poll_that_has_no_such_pr() {
        let mut c = Carry::default();
        assert_eq!(heard(&mut c, &result(9, "merged")).as_deref(), Some("MERGED #9 event [r]"));
        assert_eq!(heard(&mut c, &result(9, "closed")), None, "one terminal line per PR");
        assert_eq!(c.repos["r"].said.len(), 1);
        assert!(c.repos["r"].prs.is_empty(), "a hand-off adds no PR to ask about");
        assert!(decide(&cfg(), &c, &one(RepoObs::default()), 1000).0.repos["r"].said.contains_key(&9), "an unanswered poll keeps it");
        assert!(decide(&cfg(), &c, &one(open(vec![])), 1000).0.repos["r"].said.is_empty());
    }

    #[test_id(tc4gyc8)]
    #[test]
    fn a_red_said_by_hand_off_suppresses_one_poll_red_and_a_held_title_wins() {
        let mut c = Carry::default();
        assert_eq!(heard(&mut c, &result(7, "red")).as_deref(), Some("FAIL RED #7 event [r]"));
        let (c, say) = decide(&cfg(), &c, &one(open(vec![pr(7, "red", "x")])), 1000);
        assert_eq!(say.lines, ["OPENED #7 t7 [r]"], "the red was said already, the open was not");
        let mut c = c;
        assert_eq!(heard(&mut c, &result(7, "red")), None, "the poll holds it red");
        assert_eq!(heard(&mut c, &result(7, "merged")).as_deref(), Some("MERGED #7 t7 [r]"));
    }
}
