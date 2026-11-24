//! A saved round (`replay`'s fixture) judged by the production attribution with no process: the
//! pass it followed answers from its results folder, a probe from `probes.json`, and an eject
//! is written down instead of made. A probe nobody recorded stops the replay, naming it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use batcher::attrib::{Job, JobResult};
use batcher::core::{Id, Member};
use batcher::drive::{DeadlineRed, FilterRed, MainKind, MainPoll, RoundOps, RoundRunner};
use serde::Deserialize;

use crate::judge::Edge;
use crate::replay::RoundFixture;
use crate::vm;

/// What a replayed round must end as; `expect.json` in the fixture.
#[derive(Debug, Clone, Deserialize)]
pub struct Expect {
    /// The members the attribution ejects, in order.
    pub ejected: Vec<String>,
    /// The members left to certify.
    pub survivors: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Probe {
    call: String,
    #[serde(default)]
    name: String,
    members: Vec<String>,
    green: Option<bool>,
}

/// A saved round on disk: `round.json`, `results/`, `expect.json` and the probes the real run made.
#[derive(Debug)]
pub struct Fixture {
    pub dir: PathBuf,
    pub round: RoundFixture,
    pub expect: Expect,
    probes: Vec<Probe>,
    mending: Option<String>,
}

impl Fixture {
    pub fn load(dir: &Path) -> Result<Fixture, String> {
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).map_err(|e| format!("{}: {e}", dir.join(f).display()));
        let json = |f: &str| -> Result<serde_json::Value, String> { serde_json::from_str(&read(f)?).map_err(|e| format!("{f}: {e}")) };
        let round = serde_json::from_str(&read("round.json")?).map_err(|e| format!("round.json: {e}"))?;
        let expect = serde_json::from_str(&read("expect.json")?).map_err(|e| format!("expect.json: {e}"))?;
        let probes = match dir.join("probes.json").is_file() {
            true => serde_json::from_value(json("probes.json")?).map_err(|e| format!("probes.json: {e}"))?,
            false => vec![],
        };
        let mending = read("mending.json").ok();
        Ok(Fixture { dir: dir.to_path_buf(), round, expect, probes, mending })
    }

    /// The followed pass's results folder.
    pub fn results(&self) -> PathBuf {
        self.dir.join("results")
    }

    /// `id:tip` of every member, as `--members` takes them.
    pub fn members(&self) -> Vec<(String, String)> {
        self.round.members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect()
    }
}

/// What the replay did, for the caller to read after the judgement.
#[derive(Debug, Default)]
pub struct Done {
    pub ejected: Vec<String>,
    pub head: String,
}

pub struct ReplayEdge<'a> {
    pub fixture: &'a Fixture,
    pub done: Rc<RefCell<Done>>,
    confirmed: bool,
}

impl<'a> ReplayEdge<'a> {
    pub fn new(fixture: &'a Fixture, confirmed: bool) -> ReplayEdge<'a> {
        let done = Done { ejected: vec![], head: fixture.round.head.clone() };
        ReplayEdge { fixture, done: Rc::new(RefCell::new(done)), confirmed }
    }
}

fn bare(id: &str, tip: &str) -> Member {
    Member { id: id.into(), tip: tip.into(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: BTreeMap::new(), blocked_by: vec![], provisional: false }
}

/// The members `--members` names, as an attribution of a pool that holds none of them.
pub fn bare_members(given: &[(String, String)]) -> Vec<Member> {
    given.iter().map(|(id, tip)| bare(id, tip)).collect()
}

impl Edge for ReplayEdge<'_> {
    type Runner = ReplayRunner;
    type Ops = ReplayOps;

    fn changed(&self, members: &[Member]) -> BTreeMap<String, Vec<String>> {
        let saved: BTreeMap<&str, &Vec<String>> = self.fixture.round.members.iter().map(|m| (m.id.as_str(), &m.changed)).collect();
        members.iter().map(|m| (m.id.clone(), saved.get(m.id.as_str()).map(|c| (*c).clone()).unwrap_or_default())).collect()
    }

    fn current_deny(&self) -> Result<vm::DenySnapshot, String> {
        Err("replay: a round with no followed pass has no deny list to read".into())
    }

    fn open(&mut self, _suites: &[String], changed: BTreeMap<String, Vec<String>>, _deny: &vm::DenySnapshot, own_commits: vm::RoundCommits) -> Result<(ReplayRunner, ReplayOps), String> {
        let runner = ReplayRunner { prior: self.fixture.results(), members: vec![], changed: changed.clone(), own_commits: own_commits.borrow().clone(), round_paths: self.fixture.round.round_commit_paths.clone(), probes: self.fixture.probes.clone(), confirmed: self.confirmed, jobs: vec![] };
        let returned = match &self.fixture.mending {
            Some(rows) => crate::endpass::returns(crate::endpass::parse_rows(rows, 1)?),
            None => vec![],
        };
        let ops = ReplayOps { changed, done: self.done.clone(), rebuilds: 0, returned };
        Ok((runner, ops))
    }

    fn settle(&mut self, _: &mut ReplayRunner, _: &mut ReplayOps, end: Result<batcher::drive::RoundEnd, String>, _: &[Member], _: &[String]) -> Result<batcher::drive::RoundEnd, String> {
        end
    }

    fn mended(&self, _: &ReplayOps) -> bool {
        false
    }

    fn head(&self) -> String {
        self.done.borrow().head.clone()
    }
}

pub struct ReplayRunner {
    prior: PathBuf,
    members: Vec<Member>,
    changed: BTreeMap<String, Vec<String>>,
    own_commits: Vec<String>,
    round_paths: Vec<String>,
    probes: Vec<Probe>,
    confirmed: bool,
    jobs: Vec<(u64, JobResult)>,
}

impl ReplayRunner {
    fn answer(&self, call: &str, name: &str, subset: &[Member]) -> Option<bool> {
        let ids: Vec<&str> = subset.iter().map(|m| m.id.as_str()).collect();
        self.recorded(call, name, ids)
    }

    fn recorded(&self, call: &str, name: &str, mut ids: Vec<&str>) -> Option<bool> {
        ids.sort_unstable();
        let found = self.probes.iter().find(|p| {
            let mut want: Vec<&str> = p.members.iter().map(String::as_str).collect();
            want.sort_unstable();
            p.call == call && p.name == name && want == ids
        });
        match found {
            Some(p) => p.green,
            None => panic!("replay: no recorded probe {call} {name:?} on members [{}]", ids.join(",")),
        }
    }

    fn unrecorded(what: &str) -> String {
        format!("replay: {what} would run a suite, and a replay runs none")
    }
}

impl RoundRunner for ReplayRunner {
    fn set_members(&mut self, members: &[Member]) {
        self.members = members.to_vec();
    }
    fn set_base(&mut self, _: &str) {}
    fn start_main(&mut self, kind: MainKind, _: &[String]) -> Result<(), String> {
        Err(Self::unrecorded(&format!("a {kind:?} main run")))
    }
    fn poll_main(&mut self) -> Result<MainPoll, String> {
        Err(Self::unrecorded("polling a main run"))
    }
    fn stop_main(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn launch(&mut self, job: &Job) -> Result<(), String> {
        let result = match self.recorded("job", &job.suite, job.removal.iter().map(String::as_str).collect()) {
            Some(true) => JobResult::Green,
            Some(false) => JobResult::Red,
            None => JobResult::Fault,
        };
        self.jobs.push((job.id, result));
        Ok(())
    }
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
        std::mem::take(&mut self.jobs)
    }
    fn now(&self) -> u64 {
        0
    }
    fn wait(&mut self) {}

    fn prior_red(&mut self) -> Option<FilterRed> {
        vm::filter_red_in(&self.prior, &self.members, &self.changed, &self.own_commits, &self.round_paths)
    }
    fn prior_red_suites(&mut self) -> Vec<String> {
        vm::prior_red_suites_in(&self.prior)
    }
    fn deadline_red(&mut self, suite: &str) -> Option<DeadlineRed> {
        vm::deadline_red_in(&self.prior, None, suite)
    }
    fn prior_build_output(&mut self) -> Option<String> {
        vm::prior_build_output_in(&self.prior)
    }
    fn prior_runner(&mut self) -> Option<String> {
        vm::prior_runner_in(&self.prior)
    }
    fn confirmed(&self) -> bool {
        self.confirmed
    }
    fn builds(&mut self, subset: &[Member]) -> Option<bool> {
        self.answer("builds", "", subset)
    }
    fn config_applies(&mut self, subset: &[Member]) -> Option<bool> {
        self.answer("config_applies", "", subset)
    }
    fn filter_green(&mut self, name: &str, subset: &[Member]) -> Option<bool> {
        self.answer("filter_green", name, subset)
    }
    fn filter_green_in(&mut self, name: &str, _scope: &[String], subset: &[Member]) -> Option<bool> {
        self.answer("filter_green", name, subset)
    }
    fn suite_green(&mut self, suite: &str, subset: &[Member]) -> Option<bool> {
        self.answer("suite_green", suite, subset)
    }
    fn runner_survives(&mut self, subset: &[Member]) -> Option<bool> {
        self.answer("runner_survives", "", subset)
    }
}

pub struct ReplayOps {
    changed: BTreeMap<String, Vec<String>>,
    done: Rc<RefCell<Done>>,
    rebuilds: usize,
    returned: Vec<batcher::mender::Failure>,
}

impl ReplayOps {
    fn holds(&self, id: &str, path: &str) -> bool {
        self.changed.get(id).is_some_and(|c| c.iter().any(|p| p == path))
    }
}

impl RoundOps for ReplayOps {
    fn suspects(&self, suite: &str, members: &[Id]) -> Vec<Id> {
        self.touching(suite, members)
    }
    fn eject(&mut self, member: &Member, _: &[String], _: bool) -> Result<(), String> {
        let mut done = self.done.borrow_mut();
        if !done.ejected.contains(&member.id) {
            done.ejected.push(member.id.clone());
        }
        Ok(())
    }
    fn rebuild(&mut self, survivors: &[Member]) -> Result<Vec<Member>, String> {
        self.rebuilds += 1;
        self.done.borrow_mut().head = format!("rebuilt-{}-{}", self.rebuilds, survivors.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join("+"));
        Ok(survivors.to_vec())
    }
    fn fix_integration(&mut self, _: &[Member], _: &[String]) -> bool {
        false
    }
    fn base_moved(&mut self, _: &[Member]) -> Result<Option<(String, Vec<Member>)>, String> {
        Ok(None)
    }
    fn judge(&mut self, _: &[String], _: &[Id]) {}
    fn incident(&mut self, _: &str, _: &[String]) {}
    fn touching(&self, suite: &str, members: &[Id]) -> Vec<Id> {
        let path = format!("spira/{suite}");
        members.iter().filter(|m| self.holds(m, &path)).cloned().collect()
    }
    fn disable_flips(&mut self, tests: &[String]) -> Result<(), String> {
        Err(format!("replay: a flip of {} would write a registry row, and a replay writes none", tests.join(",")))
    }
    fn record(&mut self, _: u32, _: &batcher::attrib::Decision) {}
    fn returned_for_attribution(&self) -> Vec<batcher::mender::Failure> {
        self.returned.clone()
    }
    fn escape(&mut self, _: &Member, _: &str, _: Option<JobResult>) {}
}
