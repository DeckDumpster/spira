//! Task `concierge.ensure`: starts the installed `concierge.service` (`concierge.sh start`) only
//! when the Concierge session is gone or changed. Replaces `concierge.timer`, whose every run
//! started 27 processes to find nothing to do.
//!
//! Observe, with no program started: the session record, the `claude` process holding the
//! session's transcript, the settings file `release session-hook install` writes, and the target
//! of the release `current` link. Decide: any difference from the last look, or no holder, asks
//! for a start; whether to start, resume or refuse stays `concierge.sh start`'s. A start that
//! went red is not repeated under an unchanged observation (`law-a-retry-must-change-an-input`).

use std::path::{Path, PathBuf};

use runtime::event::*;
use runtime::units::UnitHandle;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use crate::tasks::d::pattern;
use crate::tasks::e::alert::Alert;
use crate::tasks::{Reads, Task};
use crate::watchers::{answered_effect, carried};

pub const NAME: &str = "concierge.ensure";
pub const STREAM: &str = "concierge.ensure";
pub const BOOT: EventKindTag = "concierge.ensure.boot";
pub const TICK: EventKindTag = "concierge.ensure.tick";
pub const STATE: EventKindTag = "concierge.ensure.state";
pub const OBSERVE: &str = "concierge.ensure.observe";
pub const START: &str = runtime::units::UNIT_START_INSTALLED;
pub const ALERT: &str = "concierge.ensure.alert";
pub const EFFECTS: &[&str] = &[OBSERVE, START, ALERT];
pub const KEYS: &[&str] = &["SPIRA_RUN", "SPIRA_TOKEN_PROJECTS", "SPIRA_CLIENT_SETTINGS", "SPIRA_RELEASES"];
pub const SERVICE: &str = "concierge.service";
pub const CADENCE_MS: u64 = 600_000;
pub const FIRST_TICK_MS: u64 = 60_000;
const OBSERVE_DEADLINE_MS: u64 = 5_000;
const START_DEADLINE_MS: u64 = 30_000;
const ALERT_DEADLINE_MS: u64 = 10_000;
const MAILBOX: &str = "operator";
const FROM: &str = "Concierge Ensure <concierge-ensure@spira>";

pub const ROW: Task = Task {
    name: NAME,
    reads: Reads::None,
    plane: runtime::Plane::Work,
    signal: &[ALERT],
    boot: ConciergeEnsure::boot,
    cadence_ms: |_| CADENCE_MS,
    effects: EFFECTS,
    runners: &[(OBSERVE, |e, _, s| observe(e, &s.input)), (ALERT, |e, _, s| crate::tasks::e::alert::send(e, &s.input))],
    keys: &[KEYS],
    build: |_| ConciergeEnsure::pinned().registration(),
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub run: PathBuf,
    pub projects: PathBuf,
    pub settings: PathBuf,
    pub releases: PathBuf,
    pub proc: PathBuf,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        let path = |k: &str| cfg(k).map(PathBuf::from);
        Ok(Config { run: path("SPIRA_RUN")?, projects: path("SPIRA_TOKEN_PROJECTS")?, settings: path("SPIRA_CLIENT_SETTINGS")?, releases: path("SPIRA_RELEASES")?, proc: PathBuf::from(system_paths::PROC) })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub cwd: String,
}

/// A process is the same holder only with the same pid and the same start time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub pid: u32,
    pub start: u64,
    pub transcript: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub record: Option<Record>,
    pub holder: Option<Holder>,
    pub settings_mtime_ms: Option<u64>,
    pub release_target: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub obs: Observation,
    /// The service's `Result` as the manager reports it; `success` for a unit that never ran.
    pub unit_result: Option<String>,
}

/// The probe's input: where to look, and the holder to re-check before any scan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Look {
    pub run: PathBuf,
    pub projects: PathBuf,
    pub settings: PathBuf,
    pub releases: PathBuf,
    pub proc: PathBuf,
    pub held: Option<Holder>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub obs: Observation,
    pub start_failed: bool,
    pub alerted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub held: Option<Observation>,
    pub started: Option<Started>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub start: bool,
    pub alert: Option<String>,
}

pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Decision) {
    let obs = &snap.obs;
    let unit_red = snap.unit_result.as_deref().is_some_and(|r| r != "success");
    let mut started = carry.started.clone().filter(|s| s.obs == *obs);
    let red = started.as_ref().is_some_and(|s| s.start_failed || unit_red);
    let start = !red && (carry.held.as_ref() != Some(obs) || obs.holder.is_none());
    let mut alert = None;
    if let Some(s) = started.as_mut().filter(|s| red && !s.alerted) {
        s.alerted = true;
        alert = Some(if s.start_failed { "the start was refused".to_string() } else { format!("the unit's Result is {}", snap.unit_result.as_deref().unwrap_or_default()) });
    }
    if start {
        started = Some(Started { obs: obs.clone(), start_failed: false, alerted: false });
    }
    (Carry { held: Some(obs.clone()), started }, Decision { start, alert })
}

pub struct ConciergeEnsure {
    pub config: Result<Config, String>,
}

impl ConciergeEnsure {
    pub fn new(config: Config) -> Self {
        ConciergeEnsure { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        ConciergeEnsure { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("concierge.ensure/boot", STREAM, EventKind::new(BOOT, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(RowCodec { stream: STREAM, also: &[], tag: STATE, split: runtime::rows::split_whole, join: runtime::rows::join_whole })
    }
}

fn carry_commands(before: &Carry, after: &Carry, acts: Vec<EffectSpec>) -> Vec<Command> {
    pattern::commands(STATE, before, after, acts)
}

impl Handler for ConciergeEnsure {
    fn interests(&self) -> &[EventKindTag] {
        &[BOOT, TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let config = match &self.config {
            Ok(c) => c,
            Err(e) => {
                ctx.event().set("config.refused", e.as_str());
                return vec![];
            }
        };
        let carry: Carry = carried(view, &ev.stream, STATE);
        match ev.kind.tag.as_str() {
            BOOT => vec![Command::Timer { at: ev.at + FIRST_TICK_MS, kind: EventKind::new(TICK, "") }],
            TICK => {
                let look = Look { run: config.run.clone(), projects: config.projects.clone(), settings: config.settings.clone(), releases: config.releases.clone(), proc: config.proc.clone(), held: carry.held.and_then(|o| o.holder) };
                pattern::tick(ev, OBSERVE, serde_json::to_string(&look).unwrap_or_default(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS)
            }
            _ => match answered_effect(view, ev).as_deref() {
                Some(OBSERVE) => {
                    let Some(snap) = pattern::answer::<Snapshot>(view, ev, OBSERVE) else { return vec![] };
                    let (next, d) = decide(&carry, &snap);
                    let mut acts = vec![];
                    if d.start {
                        acts.push(EffectSpec::keyed(START, format!("{NAME}:{}", ev.at), serde_json::to_string(&UnitHandle::named(SERVICE)).unwrap_or_default(), START_DEADLINE_MS));
                    }
                    if let Some(why) = d.alert {
                        let line = format!("{SERVICE} did not bring the Concierge back ({why}); see {}", config.run.join("concierge.log").display());
                        acts.push(Alert::of(MAILBOX, FROM, "concierge", "alert", &line).effect(ALERT, format!("{NAME}:alert:{}", ev.at), ALERT_DEADLINE_MS));
                    }
                    carry_commands(&carry, &next, acts)
                }
                Some(START) => {
                    let mut next = carry.clone();
                    if let Some(s) = next.started.as_mut() {
                        s.start_failed = ev.kind.tag != EFFECT_SUCCEEDED;
                    }
                    carry_commands(&carry, &next, vec![])
                }
                _ => vec![],
            },
        }
    }
}

/// The probe: [`observe_at`] and the service's `Result` from the unit manager.
pub fn observe(e: &crate::Effects, input: &str) -> Result<String, String> {
    let look: Look = serde_json::from_str(input).map_err(|e| format!("{OBSERVE}: bad input: {e}"))?;
    let unit_result = e.units.as_deref().and_then(|u| u.detail(&UnitHandle::named(SERVICE)).ok()).and_then(|d| d.result);
    serde_json::to_string(&Snapshot { obs: observe_at(&look), unit_result }).map_err(|e| e.to_string())
}

pub fn observe_at(look: &Look) -> Observation {
    let record = read_record(&look.run);
    let holder = record.as_ref().and_then(|r| holder_of(look, &r.id));
    let settings_mtime_ms = std::fs::metadata(&look.settings).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
    let release_target = std::fs::read_link(look.releases.join(release::activate::CURRENT)).ok().map(|t| t.display().to_string());
    Observation { record, holder, settings_mtime_ms, release_target }
}

fn read_record(run: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(run.join("concierge-session")).ok()?;
    let mut lines = text.lines();
    let id = lines.next()?.trim().to_string();
    (!id.is_empty()).then(|| Record { id, cwd: lines.next().unwrap_or_default().trim().to_string() })
}

/// The held holder when it is still that process and still has its transcript open; otherwise
/// the first `claude` process in the proc tree with the session's transcript open.
fn holder_of(look: &Look, id: &str) -> Option<Holder> {
    let name = format!("{id}.jsonl");
    if let Some(h) = look.held.as_ref().filter(|h| h.transcript.file_name().is_some_and(|n| *n == *name)) {
        if start_time(&look.proc, h.pid) == Some(h.start) && is_claude(&look.proc, h.pid) && holds(&look.proc, h.pid, &h.transcript) {
            return Some(h.clone());
        }
    }
    let transcript = find_file(&look.projects, &name)?;
    let me = std::process::id();
    std::fs::read_dir(&look.proc).ok()?.flatten().filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok()).filter(|pid| *pid != me).find_map(|pid| {
        (is_claude(&look.proc, pid) && holds(&look.proc, pid, &transcript)).then(|| start_time(&look.proc, pid)).flatten().map(|start| Holder { pid, start, transcript: transcript.clone() })
    })
}

/// Field 22 of `stat`, read after the `)` that ends the command name, which may itself hold spaces.
fn start_time(proc: &Path, pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(proc.join(pid.to_string()).join("stat")).ok()?;
    stat[stat.rfind(')')? + 1..].split_whitespace().nth(19)?.parse().ok()
}

fn is_claude(proc: &Path, pid: u32) -> bool {
    let Ok(cmdline) = std::fs::read(proc.join(pid.to_string()).join("cmdline")) else { return false };
    cmdline.split(|b| *b == 0).next().is_some_and(|argv0| argv0.rsplit(|b| *b == b'/').next() == Some(b"claude".as_slice()))
}

fn holds(proc: &Path, pid: u32, transcript: &Path) -> bool {
    std::fs::read_dir(proc.join(pid.to_string()).join("fd")).is_ok_and(|fds| fds.flatten().any(|fd| std::fs::read_link(fd.path()).is_ok_and(|l| l == transcript)))
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut dirs = vec![];
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let path = e.path();
        if e.file_name() == name {
            return Some(path);
        }
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            dirs.push(path);
        }
    }
    dirs.iter().find_map(|d| find_file(d, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn holder(pid: u32, start: u64) -> Holder {
        Holder { pid, start, transcript: PathBuf::from("/p/a.jsonl") }
    }

    fn observed() -> Observation {
        Observation { record: Some(Record { id: "a".into(), cwd: "/brain".into() }), holder: Some(holder(10, 500)), settings_mtime_ms: Some(7), release_target: Some("r1".into()) }
    }

    fn snap(obs: Observation) -> Snapshot {
        Snapshot { obs, unit_result: Some("success".into()) }
    }

    fn after_a_first_look() -> Carry {
        decide(&Carry::default(), &snap(observed())).0
    }

    #[test_id(tlhjd0y)]
    #[test]
    fn the_same_holder_record_and_settings_start_nothing() {
        let (next, d) = decide(&after_a_first_look(), &snap(observed()));
        assert_eq!(d, Decision::default());
        assert_eq!(next.held, Some(observed()));
    }

    #[test_id(tz00zpm)]
    #[test]
    fn each_change_to_what_was_held_starts_once() {
        let cases: Vec<(&str, Box<dyn Fn(&mut Observation)>)> = vec![
            ("holder pid reused with another start time", Box::new(|o| o.holder = Some(holder(10, 501)))),
            ("record id changed", Box::new(|o| o.record.as_mut().unwrap().id = "b".into())),
            ("settings mtime moved", Box::new(|o| o.settings_mtime_ms = Some(8))),
            ("release target moved", Box::new(|o| o.release_target = Some("r2".into()))),
        ];
        for (why, change) in cases {
            let mut obs = observed();
            change(&mut obs);
            let (next, d) = decide(&after_a_first_look(), &snap(obs.clone()));
            assert!(d.start, "{why}");
            assert_eq!(decide(&next, &snap(obs)).1, Decision::default(), "{why}: the next tick starts nothing");
        }
    }

    #[test_id(tapwyn3)]
    #[test]
    fn a_first_tick_starts_once() {
        let (next, d) = decide(&Carry::default(), &snap(observed()));
        assert!(d.start);
        assert!(!decide(&next, &snap(observed())).1.start);
    }

    #[test_id(tou29sc)]
    #[test]
    fn a_holder_that_is_gone_starts() {
        let gone = Observation { holder: None, ..observed() };
        assert!(decide(&after_a_first_look(), &snap(gone)).1.start);
    }

    #[test_id(tj185kk)]
    #[test]
    fn a_red_under_the_same_observation_starts_nothing_and_alerts_once() {
        let gone = Observation { holder: None, ..observed() };
        let (started, d) = decide(&after_a_first_look(), &snap(gone.clone()));
        assert!(d.start);
        let red = Snapshot { obs: gone.clone(), unit_result: Some("exit-code".into()) };
        let (next, d) = decide(&started, &red);
        assert!(!d.start && d.alert.is_some(), "{d:?}");
        let (next, d) = decide(&next, &red);
        assert_eq!(d, Decision::default(), "the second tick under the same observation is silent");
        let moved = Observation { settings_mtime_ms: Some(9), ..gone };
        assert!(decide(&next, &Snapshot { obs: moved, unit_result: Some("exit-code".into()) }).1.start, "a changed observation is a changed input");
    }

    #[test_id(twxnuol)]
    #[test]
    fn a_start_the_manager_refused_is_a_red_too() {
        let gone = Observation { holder: None, ..observed() };
        let (mut carry, _) = decide(&Carry::default(), &snap(gone.clone()));
        carry.started.as_mut().unwrap().start_failed = true;
        let (_, d) = decide(&carry, &snap(gone));
        assert!(!d.start && d.alert.is_some());
    }

    #[test_id(tuii4po)]
    #[test]
    fn the_unit_name_is_the_one_the_manifest_ships() {
        let inputs = install::manifest::Inputs { instance: "prod".into(), dolt_data_set: false, dolt_durable_set: false, sccache_dav_addr_set: false, repo_is_git_checkout: true, lc_system_mode: false, watch_names: Ok(vec![]) };
        let m = install::manifest::build(&inputs).unwrap();
        assert!(m.units.iter().any(|u| install::manifest::inst_name(&u.name, "prod") == SERVICE), "{SERVICE} is not in the manifest");
        assert_eq!(START, "unit.start_installed");
    }

    struct Tree {
        dir: testkit::TempDir,
        look: Look,
        transcript: PathBuf,
    }

    fn tree() -> Tree {
        let dir = testkit::TempDir::new("concierge-ensure");
        let (run, projects, proc) = (dir.join("run"), dir.join("projects"), dir.join("proc"));
        for d in [&run, &proc, &projects.join("slug")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(run.join("concierge-session"), "sess-1\n/brain\n").unwrap();
        let transcript = projects.join("slug/sess-1.jsonl");
        std::fs::write(&transcript, "").unwrap();
        let look = Look { run, projects, settings: dir.join("settings.json"), releases: dir.join("releases"), proc, held: None };
        Tree { dir, look, transcript }
    }

    fn process(t: &Tree, pid: u32, argv0: &str, start: u64, open: Option<&Path>) {
        let p = t.look.proc.join(pid.to_string());
        std::fs::create_dir_all(p.join("fd")).unwrap();
        std::fs::write(p.join("cmdline"), format!("{argv0}\0--resume\0")).unwrap();
        std::fs::write(p.join("stat"), format!("{pid} (a b) S {} {start} 0\n", ["0"; 18].join(" "))).unwrap();
        if let Some(f) = open {
            symlink(f, p.join("fd/3")).unwrap();
        }
    }

    #[test_id(tqvkvyj)]
    #[test]
    fn a_claude_with_the_transcript_open_is_the_holder_and_a_tail_or_a_claude_without_it_is_not() {
        let t = tree();
        process(&t, 11, "/usr/bin/tail", 100, Some(&t.transcript));
        process(&t, 12, "/opt/claude", 200, None);
        assert_eq!(observe_at(&t.look).holder, None, "neither a tail holding the fd nor a claude without it is the holder");
        process(&t, 13, "/opt/bin/claude", 300, Some(&t.transcript));
        let h = observe_at(&t.look).holder.expect("a claude with the transcript open");
        assert_eq!((h.pid, h.start), (13, 300));
    }

    #[test_id(tzmyjmf)]
    #[test]
    fn a_held_holder_is_rechecked_by_pid_and_a_reused_pid_is_not_it() {
        let t = tree();
        process(&t, 13, "claude", 300, Some(&t.transcript));
        let held = observe_at(&t.look).holder.unwrap();
        std::fs::remove_dir_all(t.look.proc.join("13")).unwrap();
        process(&t, 14, "claude", 400, Some(&t.transcript));
        let look = Look { held: Some(held.clone()), ..t.look.clone() };
        assert_eq!(observe_at(&look).holder.map(|h| h.pid), Some(14), "the pid is gone, so the tree is scanned");
        process(&t, 13, "claude", 301, Some(&t.transcript));
        std::fs::remove_dir_all(t.look.proc.join("14")).unwrap();
        assert_eq!(observe_at(&look).holder.map(|h| (h.pid, h.start)), Some((13, 301)), "pid 13 came back with another start time: a new holder, not the held one");
    }

    #[test_id(tg78z6y)]
    #[test]
    fn the_settings_mtime_and_the_current_link_are_observed() {
        let t = tree();
        assert_eq!((observe_at(&t.look).settings_mtime_ms, observe_at(&t.look).release_target), (None, None));
        std::fs::write(&t.look.settings, "{}").unwrap();
        std::fs::create_dir_all(&t.look.releases).unwrap();
        symlink("spira-releases/abc", t.look.releases.join("current")).unwrap();
        let o = observe_at(&t.look);
        assert!(o.settings_mtime_ms.is_some_and(|m| m > 0));
        assert_eq!(o.release_target.as_deref(), Some("spira-releases/abc"));
        let _ = &t.dir;
    }
}
