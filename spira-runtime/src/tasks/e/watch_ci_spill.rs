//! # watch.ci_spill — contract
//!
//! watchd row `ci-spill-guard` (`spira/watchers:159`), target `@SPIRA_HOME@/ci-spill-guard.sh watch --interval 900`; pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]).
//!
//! Catalogue differs: the children are `lib.sh`, `git rev-parse`, `git archive | tar -x` and `awk` (`spira/ci-spill-guard.sh:89`, `:93`, `:19`), not `gh`. The guard reads each repo's landing ref from git, never the forge.
//!
//! ## Tick
//!
//! - Cadence 900 s (`spira/watchers:159`; loop `:120-137`). A landing on any registered repo is the natural extra wake (the landing ref moved) (unverified: no such event on the bus yet). Single-flight.
//!
//! ## Observe
//!
//! `watch.ci_spill.observe` (git class) takes the carried per-repo seen SHAs and returns:
//!
//! ```text
//! Snapshot { repos: Vec<Repo> }
//! Repo { name, ref_: Option<String>, sha: Option<String>, workflows: Option<Vec<(path, text)>> }  // workflows only when sha != seen
//! ```
//!
//! - Repo set and roots from the repo map: `repo_names` / `repo_root` (`spira/lib.sh:1728`, `:1737`) — in-process from `Settings` (S10).
//! - `ref_` from `spira_landref` (`spira/lib.sh:1930`), the landing component's own answer; `None` → the "no resolvable landing ref" finding (`ci-spill-guard.sh:88`).
//! - `sha` = `git rev-parse <ref>` (`:89`); `workflows` = the `.github/workflows/*.y{a,}ml` blobs at that ref — today `git archive <ref> .github/workflows | tar -x` into a tmpdir (`:92-93`); in-process a `git show`/tree read, no tmpdir.
//!
//! ## Decide
//!
//! - Pure scan: the awk in `_csg_file` (`:18-38`) becomes a Rust function `violations(path, text) -> Vec<String>` — a `uses: …/provision@…` step whose `spill:` is absent or not `ec2-only`. The same function serves `ci-spill-guard.sh --gate` (gate fence, `gate.steps`) once the gate moves; until then both exist (parity test pins them equal).
//! - Carry `watch.ci_spill.state` = `{ said: BTreeSet<String>, seen: BTreeMap<repo, sha> }` — today `run/watchd/ci-spill-guard.said` (append-only, `:75-80`, never cleared) and `run/watchd/ci-spill-guard.refs/<repo>` (`:90`, `:98`).
//! - For each repo with a new sha: every violation line not in `said` → one inbox line; then `seen[repo] = sha`. A repo with no ref → key `noref-<repo>` once.
//! - Note (from code): `said` is never pruned today, so a violation fixed and later reintroduced is not said again. Kept for parity; flagged for Ryan.
//! - Fixtures: none in the release (unverified: no `test-ci-spill-guard*.sh`). New: compliant workflow; missing `spill:`; `spill: ec2` (wrong); quoted value; nested step indentation; repo with no ref; unchanged sha skipped.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.ci_spill.observe` | git | 30 s | (probe) | a repo whose root does not exist → omitted, as `repo_root … || continue` (`:87`) |
//! | `inbox.append` (shared) | host | 10 s | `ci_spill/<said-key>` | — |
//!
//! Inbox line, exactly as today (`spira/ci-spill-guard.sh:79`, text `:96` / `:88`):
//!
//! ```text
//! <YYYY-MM-DDTHH:MM:SSZ> [watch:ci-spill-guard] CI SPILL: law-buy-capacity-rather-than-wait: <repo>:<file>:<line>: provision has no spill: input; override: spill: ec2-only (at <ref>)
//! <YYYY-MM-DDTHH:MM:SSZ> [watch:ci-spill-guard] CI SPILL: <repo> has no resolvable landing ref; its workflows are unchecked
//! ```
//!
//! Today each line also goes to stdout → `run/watchd/ci-spill-guard.log` (`:77`). Readers of that log: none by name in the release or `run/concierge-notes`; watchd's generic walkers only; no cursor file exists. The log copy is dropped.
//!
//! Health (`ci-spill-guard.sh health`, `:109-118`) → S13 `scheduler status`.
//!
//! ## Silence
//!
//! No repo's sha moved → probe only (the snapshot carries no workflows), no `Emit`. A moved sha with no new violation emits the `seen` update only — one `state` event per landing, not per tick.
//!
//! ## Replaces
//!
//! - `spira-watch-ci-spill-guard-prod.service`; row `spira/watchers:159`.
//! - Children: `bash lib.sh` (`:70`), `git rev-parse` (`:89`), `git archive` + `tar` (`:93`), `mktemp`/`rm -rf` (`:92`, `:100`), `awk` (`:19`).
//! - Files retired: `run/watchd/ci-spill-guard.{said,health,log,restarts}`, `run/watchd/ci-spill-guard.refs/`.
//! - Config pinned: the repo map behind `repo_names`/`repo_root`/`spira_landref`; `SPIRA_CONCIERGE_INBOX`.
//!
//! ## Reverse
//!
//! Release rollback restores unit and row; `Observe` mode shadows `inbox.append`. The `scan`/`--gate` subcommands stay in the script until the gate fence moves, so nothing else regresses.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use runtime::event::*;
use runtime::ports::{Proc, ProcSpec, SysProc};
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;
use spira_config::repos::Registry;

use crate::tasks::d::pattern;
use crate::watchers::{carried, INBOX_APPEND};
use crate::Effects;

pub const NAME: &str = "watch.ci_spill";
pub const STREAM: &str = "watch.ci_spill";
pub const TICK: EventKindTag = "watch.ci_spill.tick";
pub const STATE: EventKindTag = "watch.ci_spill.state";
pub const OBSERVE: &str = "watch.ci_spill.observe";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND];
pub const KEYS: &[&str] = &["SPIRA_REPO_MAP", "SPIRA_CONCIERGE_INBOX"];
/// The row's tag in every line it wrote (`spira/ci-spill-guard.sh:79`).
pub const INBOX_TAG: &str = "ci-spill-guard";
/// `--interval 900`.
pub const CADENCE_MS: u64 = 900_000;
const OBSERVE_DEADLINE_MS: u64 = 30_000;
const INBOX_DEADLINE_MS: u64 = 10_000;
const GIT_DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub repo_map: String,
    pub inbox: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { repo_map: cfg("SPIRA_REPO_MAP")?, inbox: cfg("SPIRA_CONCIERGE_INBOX")? })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub sha: Option<String>,
    /// `(path, text)` of each workflow; only when `sha` differs from the carried one.
    pub workflows: Option<Vec<(String, String)>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub repos: Vec<Repo>,
}

/// `{ said, seen: repo → sha }`; `said` is never pruned, as today.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub said: BTreeSet<String>,
    pub seen: BTreeMap<String, String>,
}

/// One line's indentation: its leading spaces, as the awk's `ind()`.
fn ind(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// `/^[ ]*-[ ]/`: a list item.
fn is_item(line: &str) -> bool {
    line.trim_start_matches(' ').starts_with("- ")
}

/// `/uses:[ ]*[^ #]*\/provision@/`: some `uses:` whose value, up to a space or `#`, names a
/// `provision` action at a ref.
fn uses_provision(line: &str) -> bool {
    line.match_indices("uses:").any(|(i, _)| {
        let rest = line[i + 5..].trim_start_matches(' ');
        rest.split([' ', '#']).next().is_some_and(|v| v.contains("/provision@"))
    })
}

/// The value of a `spill:` line as the awk cleans it: the key, a trailing comment and one quote
/// at either end removed.
fn spill_value(line: &str) -> String {
    let v = line.trim_start_matches(' ').trim_start_matches("spill:").trim_start_matches(' ');
    // `sub(/[ ]+#.*$/, "")`: from the spaces before the first `#` that follows a space.
    let v = match v.match_indices(" #").next() {
        Some((i, _)) => v[..i].trim_end_matches(' '),
        None => v,
    };
    let v = v.strip_prefix(['"', '\'']).unwrap_or(v);
    v.strip_suffix(['"', '\'']).unwrap_or(v).to_string()
}

/// The awk of `_csg_file` as Rust: a `uses: …/provision@…` step whose `spill:` is absent or not
/// `ec2-only`, one finding per step, each `<path>:<line>: …` as the awk printed it.
pub fn violations(path: &str, text: &str) -> Vec<String> {
    #[derive(Default)]
    struct Step {
        line: usize,
        indent: usize,
        spill: Option<(usize, String)>,
    }
    fn flush(step: &mut Option<Step>, path: &str, out: &mut Vec<String>) {
        match step.take() {
            Some(Step { line, spill: None, .. }) => out.push(format!("{path}:{line}: provision has no spill: input; override: spill: ec2-only")),
            Some(Step { spill: Some((at, v)), .. }) if v != "ec2-only" => out.push(format!("{path}:{at}: provision spill: {v}; override: spill: ec2-only")),
            _ => {}
        }
    }
    let (mut out, mut step, mut cur) = (vec![], None::<Step>, 0);
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if is_item(line) && step.as_ref().is_some_and(|s| ind(line) <= s.indent) {
            flush(&mut step, path, &mut out);
        }
        if is_item(line) {
            cur = ind(line);
        }
        if line.chars().next().is_some_and(|c| c != ' ' && c != '#') {
            flush(&mut step, path, &mut out);
        }
        if uses_provision(line) {
            flush(&mut step, path, &mut out);
            step = Some(Step { line: n, indent: cur, spill: None });
            continue;
        }
        if let Some(s) = step.as_mut() {
            if line.trim_start_matches(' ').starts_with("spill:") {
                s.spill = Some((n, spill_value(line)));
            }
        }
    }
    flush(&mut step, path, &mut out);
    out
}

/// The text `_csg_once` wrote after the stamp, and the key it marked said.
fn said(carry: &mut Carry, acts: &mut Vec<EffectSpec>, now_ms: u64, key: String, message: String) {
    if carry.said.insert(key) {
        acts.push(EffectSpec { name: INBOX_APPEND.into(), input: pattern::inbox_line(now_ms, INBOX_TAG, &message), deadline_ms: INBOX_DEADLINE_MS });
    }
}

/// For each repo with a new sha, every violation not yet said as one inbox line; then `seen`.
/// A repo whose workflows were not read (the archive failed) keeps its old `seen`, as the
/// script's stamp did.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let (mut next, mut acts) = (carry.clone(), vec![]);
    for repo in &snap.repos {
        let Some(ref_) = &repo.ref_ else {
            said(&mut next, &mut acts, now_ms, format!("noref-{}", repo.name), format!("CI SPILL: {} has no resolvable landing ref; its workflows are unchecked", repo.name));
            continue;
        };
        if repo.sha.is_some() && repo.sha.as_ref() == carry.seen.get(&repo.name) {
            continue;
        }
        let Some(workflows) = &repo.workflows else { continue };
        for (path, text) in workflows {
            for v in violations(&format!("{}:{path}", repo.name), text) {
                let message = format!("CI SPILL: law-buy-capacity-rather-than-wait: {v} (at {ref_})");
                said(&mut next, &mut acts, now_ms, v, message);
            }
        }
        if let Some(sha) = &repo.sha {
            next.seen.insert(repo.name.clone(), sha.clone());
        }
    }
    (next, acts)
}

/// One git read in `root`, bounded; its stdout when git exited 0.
fn git(root: &str, args: &[&str]) -> Option<String> {
    let mut argv = vec!["-C".to_string(), root.to_string()];
    argv.extend(args.iter().map(|a| a.to_string()));
    let out = SysProc.run(&ProcSpec { prog: "git".into(), args: argv, stdin: vec![], deadline: Duration::from_millis(GIT_DEADLINE_MS) }).ok()?;
    (out.code == Some(0)).then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The `.github/workflows/*.yml` then `*.yaml` blobs at `ref_`, each glob sorted as bash expands
/// it; `None` when the tree could not be read.
fn workflows(root: &str, ref_: &str) -> Option<Vec<(String, String)>> {
    let listing = git(root, &["ls-tree", "--name-only", ref_, ".github/workflows/"])?;
    let mut names: Vec<&str> = listing.lines().filter(|l| !l.is_empty()).collect();
    names.sort();
    let mut out = vec![];
    for ext in [".yml", ".yaml"] {
        for path in names.iter().filter(|p| p.ends_with(ext)) {
            out.push((path.to_string(), git(root, &["show", &format!("{ref_}:{path}")])?));
        }
    }
    Some(out)
}

/// The observe runner: every registered repo whose checkout exists, its landing ref and sha, and
/// its workflows when the sha differs from `input`'s carried `seen`.
pub fn observe(e: &Effects, input: &str) -> Result<String, String> {
    let seen: BTreeMap<String, String> = serde_json::from_str(input).unwrap_or_default();
    let home = spira_config::resolve::locate_home_for_process().unwrap_or_default();
    let map = std::fs::read_to_string(&e.world.repo_map).ok();
    let env = BTreeMap::from([("SPIRA_HOME_REPO".to_string(), e.world.home_repo.clone())]);
    let reg = Registry::new(map.as_deref(), &env, &home);
    let mut repos = vec![];
    for name in reg.names() {
        let Some(root) = reg.root(&name).filter(|r| std::path::Path::new(r).exists()) else { continue };
        let Some(ref_) = spira_config::repos::landref(&reg, &name) else {
            repos.push(Repo { name, ..Repo::default() });
            continue;
        };
        let sha = git(&root, &["rev-parse", &ref_]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let workflows = if sha.is_some() && sha.as_ref() == seen.get(&name) { None } else { workflows(&root, &ref_) };
        repos.push(Repo { name, ref_: Some(ref_), sha, workflows });
    }
    serde_json::to_string(&Snapshot { repos }).map_err(|e| e.to_string())
}

pub struct WatchCiSpill {
    pub config: Result<Config, String>,
}

impl WatchCiSpill {
    pub fn new(config: Config) -> Self {
        WatchCiSpill { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchCiSpill { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.ci_spill/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchCiSpill {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let seen = serde_json::to_string(&carried::<Carry>(view, &ev.stream, STATE).seen).unwrap_or_default();
            return pattern::tick(ev, OBSERVE, seen, OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: &str = ".github/workflows/ci.yml";

    #[test]
    fn the_scan_reads_each_provision_step_as_the_awk_did() {
        let ok = "jobs:\n  a:\n    steps:\n      - uses: o/a/provision@v1\n        with:\n          spill: ec2-only # pinned\n";
        assert!(violations(W, ok).is_empty(), "a trailing comment is not part of the value");
        let quoted = "jobs:\n  a:\n    steps:\n      - uses: o/a/provision@v1\n        with:\n          spill: \"ec2-only\"\n";
        assert!(violations(W, quoted).is_empty(), "quotes are stripped");
        let wrong = "jobs:\n  a:\n    steps:\n      - uses: o/a/provision@v1\n        with:\n          spill: ec2\n";
        assert_eq!(violations(W, wrong), [format!("{W}:6: provision spill: ec2; override: spill: ec2-only")]);
        let named = "jobs:\n  a:\n    steps:\n      - name: boot\n        uses: o/a/provision@v1\n      - name: next\n        with:\n          spill: ec2-only\n";
        assert_eq!(violations(W, named), [format!("{W}:5: provision has no spill: input; override: spill: ec2-only")], "a sibling step's spill: does not count");
        let two = "jobs:\n  a:\n    steps:\n      - uses: o/a/provision@v1\njobs2:\n  - uses: o/a/provision@v2\n";
        assert_eq!(violations(W, two).len(), 2, "a top-level key ends the step");
        assert!(violations(W, "jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v4\n").is_empty());
    }

    #[test]
    fn an_unreadable_tree_keeps_its_old_seen_and_a_missing_ref_is_said_once() {
        let cfg = Config { repo_map: String::new(), inbox: String::new() };
        let snap = Snapshot { repos: vec![Repo { name: "r".into(), ref_: Some("main".into()), sha: Some("b".into()), workflows: None }, Repo { name: "d".into(), ..Repo::default() }] };
        let (next, acts) = decide(&Carry::default(), &snap, 0, &cfg);
        assert!(next.seen.is_empty());
        assert_eq!(acts.len(), 1);
        assert!(decide(&next, &snap, 0, &cfg).1.is_empty());
    }
}
