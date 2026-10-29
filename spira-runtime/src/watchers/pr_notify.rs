//! OPENED, FAIL RED, GREEN, MERGED, CLOSED, DEPLOYED, DEPLOY FAILED and `no PR` for every repository
//! registered, whatever its land mode. Each tick asks the world for one snapshot of every forge;
//! the decision over snapshot and carried state is `decide`, which does no IO.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{answers_observe, carried, iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.pr-notify";
pub const TICK: EventKindTag = "pr-notify.tick";
pub const STATE: EventKindTag = "pr-notify.state";
pub const OBSERVE: &str = "pr-notify.observe";
pub const OPERATOR_NOTE: &str = "operator.note";
pub const NAME: &str = "pr-notify";

const OBSERVE_DEADLINE_MS: u64 = 120_000;
const DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrNotify {
    pub interval_ms: u64,
    pub deploy_giveup_secs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracked {
    pub status: String,
    pub title: String,
}

/// A merge awaiting its deploy run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deploy {
    pub sha: String,
    pub n: u64,
    pub at: u64,
    pub title: String,
    pub beads: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCarry {
    pub prs: BTreeMap<u64, Tracked>,
    pub deploys: Vec<Deploy>,
    pub strays: Vec<String>,
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

/// One repository as the forge showed it. `open` is `None` when the forge could not be asked,
/// which is never the same as no open PRs; an absent `finals` or `runs` entry is a question the
/// forge did not answer; `strays` is `None` where the check does not apply or could not run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoObs {
    pub open: Option<Vec<OpenPr>>,
    pub finals: BTreeMap<u64, Final>,
    pub runs: BTreeMap<String, Run>,
    pub strays: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub repos: Vec<(String, RepoObs)>,
}

/// A merge or deploy outcome for the operator's mailbox.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub subject: String,
    pub body: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Said {
    pub lines: Vec<String>,
    pub notes: Vec<Note>,
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
    if !f.sha.is_empty() && !rc.deploys.iter().any(|d| d.sha == f.sha) {
        rc.deploys.push(Deploy { sha: f.sha.clone(), n, at: now, title: title.replace('\t', " "), beads: f.beads.iter().map(|l| l.replacen("- ", "- bead ", 1)).collect() });
    }
}

fn transitions(repo: &str, open: &[OpenPr], obs: &RepoObs, now: u64, rc: &mut RepoCarry, say: &mut Said) {
    let prev = std::mem::take(&mut rc.prs);
    let mut cur = BTreeMap::new();
    for p in open {
        let old = prev.get(&p.number);
        if old.is_none() {
            say.lines.push(format!("OPENED #{} {} [{repo}]", p.number, p.title));
        }
        if old.is_none_or(|o| o.status != p.status) {
            match p.status.as_str() {
                "red" => say.lines.push(format!("FAIL RED #{} {} [{repo}]{}", p.number, p.title, if p.fail.is_empty() { String::new() } else { format!(": {}", p.fail) })),
                "green" => say.lines.push(format!("GREEN #{} {} [{repo}]: lands automatically", p.number, p.title)),
                _ => {}
            }
        }
        cur.insert(p.number, Tracked { status: p.status.clone(), title: p.title.clone() });
    }
    for (m, t) in &prev {
        if open.iter().any(|p| p.number == *m) {
            continue;
        }
        match obs.finals.get(m) {
            Some(f) if f.state == "MERGED" => {
                say.lines.push(format!("MERGED #{m} {} [{repo}]", t.title));
                merged(repo, *m, &t.title, f, now, rc, say);
            }
            Some(f) if f.state == "CLOSED" => say.lines.push(format!("CLOSED #{m} {} [{repo}]", t.title)),
            _ => {
                cur.insert(*m, t.clone());
            }
        }
    }
    rc.prs = cur;
}

fn deploys(repo: &str, obs: &RepoObs, now: u64, giveup: u64, rc: &mut RepoCarry, say: &mut Said) {
    let queued = std::mem::take(&mut rc.deploys);
    for d in queued {
        let Some(run) = obs.runs.get(&d.sha) else {
            if now.saturating_sub(d.at) <= giveup {
                rc.deploys.push(d);
            }
            continue;
        };
        let verdict = if run.conclusion == "success" { "DEPLOYED" } else { "DEPLOY FAILED" };
        let (n, title, sha, concl) = (d.n, &d.title, &d.sha, &run.conclusion);
        say.notes.push(Note {
            subject: format!("{verdict} #{n} [{repo}] {}", short(sha)),
            body: format!("{verdict}: PR #{n} {title} [{repo}]\nMerge: {sha}\nRun: {concl} in {}m{}s\n{}\n{}", run.secs / 60, run.secs % 60, run.url, beads_block(&d.beads.join("\n"))),
        });
        say.lines.push(format!("{verdict} #{n} {title} [{repo}]: {concl} in {}s {}", run.secs, run.url));
    }
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

impl PrNotify {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("pr-notify/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for PrNotify {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let input = serde_json::to_string(&carried::<Carry>(view, &ev.stream, STATE)).unwrap_or_default();
            return vec![
                Command::Effect(EffectSpec { name: OBSERVE.into(), input, deadline_ms: OBSERVE_DEADLINE_MS }),
                Command::Schedule { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if !answers_observe(view, ev, OBSERVE) || ev.kind.tag != EFFECT_SUCCEEDED {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else { return vec![] };
        let now = ev.at / 1000;
        let (next, say) = decide(self, &carried::<Carry>(view, &ev.stream, STATE), &snap, now);
        ctx.event().set("pr_notify.said", (say.lines.len() + say.notes.len()) as u64);
        let mut cmds = vec![Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default()))];
        for m in say.lines {
            cmds.push(Command::Effect(EffectSpec { name: INBOX_APPEND.into(), input: format!("{} [watch:{NAME}] {m}", iso8601(now)), deadline_ms: DEADLINE_MS }));
        }
        for n in say.notes {
            cmds.push(Command::Effect(EffectSpec { name: OPERATOR_NOTE.into(), input: serde_json::to_string(&n).unwrap_or_default(), deadline_ms: DEADLINE_MS }));
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

    #[test]
    fn checks_classify_as_red_green_or_pending() {
        assert_eq!(classify(&[]), ("pending", String::new()));
        assert_eq!(classify(&[check("a", "COMPLETED", "SUCCESS"), check("b", "COMPLETED", "FAILURE"), check("c", "COMPLETED", "TIMED_OUT")]), ("red", "b,c".into()));
        assert_eq!(classify(&[check("a", "COMPLETED", "SUCCESS")]), ("green", String::new()));
        assert_eq!(classify(&[check("a", "COMPLETED", "SUCCESS"), check("b", "IN_PROGRESS", "")]).0, "pending");
        assert_eq!(classify(&[check("a", "COMPLETED", "NEUTRAL")]).0, "pending");
    }

    #[test]
    fn a_pr_is_reported_on_change_only() {
        let (c, say) = decide(&cfg(), &Carry::default(), &one(open(vec![pr(7, "pending", "")])), 1000);
        assert_eq!(say.lines, ["OPENED #7 t7 [r]"]);
        assert!(decide(&cfg(), &c, &one(open(vec![pr(7, "pending", "")])), 1090).1.lines.is_empty());
        let (c, say) = decide(&cfg(), &c, &one(open(vec![pr(7, "red", "build,lint")])), 1180);
        assert_eq!(say.lines, ["FAIL RED #7 t7 [r]: build,lint"]);
        assert_eq!(decide(&cfg(), &c, &one(open(vec![pr(7, "green", "")])), 1270).1.lines, ["GREEN #7 t7 [r]: lands automatically"]);
    }

    #[test]
    fn a_pr_first_seen_resolved_gets_opened_and_its_verdict() {
        let (_, say) = decide(&cfg(), &Carry::default(), &one(open(vec![pr(1, "red", "x")])), 1000);
        assert_eq!(say.lines, ["OPENED #1 t1 [r]", "FAIL RED #1 t1 [r]: x"]);
    }

    #[test]
    fn a_forge_that_did_not_answer_changes_nothing() {
        let (c, _) = decide(&cfg(), &Carry::default(), &one(open(vec![pr(7, "pending", "")])), 1000);
        let (after, say) = decide(&cfg(), &c, &one(RepoObs::default()), 1090);
        assert_eq!((after, say), (c, Said::default()));
    }

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

    #[test]
    fn a_merge_mails_the_operator_and_queues_its_deploy_once() {
        let (c, say) = merged_carry();
        assert_eq!(say.lines, ["MERGED #7 t7 [r]"]);
        assert_eq!(say.notes[0].subject, "MERGED #7 [r] 012345678");
        assert_eq!(say.notes[0].body, "PR #7 t7 [r] merged at 0123456789abcdef.\n\nBeads:\n- bead sp-aaa — one\n");
        assert_eq!(c.repos["r"].deploys.len(), 1);
    }

    #[test]
    fn a_deploy_is_reported_when_its_run_completes_and_dropped_after_the_giveup() {
        let (c, _) = merged_carry();
        let mut done = open(vec![]);
        done.runs.insert("0123456789abcdef".into(), Run { conclusion: "failure".into(), secs: 125, url: "http://u".into() });
        let (after, say) = decide(&cfg(), &c, &one(done), 1100);
        assert_eq!(say.lines, ["DEPLOY FAILED #7 t7 [r]: failure in 125s http://u"]);
        assert_eq!(say.notes[0].body, "DEPLOY FAILED: PR #7 t7 [r]\nMerge: 0123456789abcdef\nRun: failure in 2m5s\nhttp://u\n\nBeads:\n- bead sp-aaa — one\n");
        assert!(after.repos["r"].deploys.is_empty());
        assert_eq!(decide(&cfg(), &c, &one(open(vec![])), 1190).0.repos["r"].deploys.len(), 1, "waiting is not giving up");
        assert!(decide(&cfg(), &c, &one(open(vec![])), 1191).0.repos["r"].deploys.is_empty());
    }

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
}
