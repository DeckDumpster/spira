//! `intake.gh`: GitHub issues become work beads or untrusted records. Replaces
//! `spira-gh-intake-prod` (ExecStartPre `world guard`; ExecStart `bin/gh-intake`). Pattern P1.
//! Contract: `wiki/projects/spira/designs/tasks/intake-gh.md`. The rules are `gh_intake::logic`'s,
//! its text builders called as a library; GitHub, the store and the operator mail are reached
//! through `gh_intake::real` from the runners below.
//!
//! # Tick
//!
//! Every 15 min (`SPIRA_RUNTIME_INTAKE_GH_SECS`), single-flight; nothing else wakes it. While the
//! world is halted the snapshot carries `halted` and `decide` returns no effects (a per-task read
//! standing in for the S14 halt gate).
//!
//! # Observe
//!
//! `intake.gh.observe` returns a [`Snapshot`]: the targets (`SPIRA_GH_INTAKE_REPO` plus every
//! registered repo with a forge remote, its slug from the `spira-config` registry), the store's
//! ingested refs (the `content` verb under S7), every open-issue page (at most 20, 30 s timeout),
//! and the trust answers GitHub gave inside the probe (`spira:accept` actor, org membership,
//! collaborator permission, the hidden-membership 404), so `decide` makes no calls.
//!
//! # Decide
//!
//! `decide(carry, snap) -> (carry', acts)`, lifted from `logic::run`/`ingest_repo` with every rule
//! verbatim: refuse a zero-bead store; refuse an unresolvable bead repo; the first read of a slug
//! records its open issues as seen and files nothing; a ref already tracked gets a note; trusted
//! authors, own-bot acceptance or `spira:accept` from someone with access become work; everything
//! else gets an untrusted record; one digest mail per slug; a rate-limit deferral alarms after
//! 3 h. Carried state is `intake.gh.state { seen, last_ok_hour }`. Fixtures port
//! `gh-intake/src/tests.rs` (13 cases) and `spira/test-gh-intake.sh`.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `intake.gh.observe` (probe) | forge | 120 s | tick id | a run with no App token |
//! | `intake.gh.file_work` | store | 120 s | `github:<slug>#<n>` | a ref already in the store |
//! | `intake.gh.file_untrusted` | store | 120 s | `github-untrusted:<slug>#<n>` | an existing untrusted ref |
//! | `intake.gh.note_seen` | store | 30 s | `<bead-id>/<ref>` | nothing |
//! | `intake.gh.close_promoted` | store | 30 s | `<untrusted-id>` | a non-open bead; closes through `lifecycle_row::close` |
//! | `intake.gh.digest` | store (operator mail) | 30 s | `<slug>/<sorted new numbers>` | an empty list |
//! | `intake.gh.unverifiable` | store (operator mail) | 30 s | `<slug>/<login>` | a repeat login in one pass |
//! | `inbox.append` (shared) | host | 10 s | `<tick>/<slug>` | only the refusal lines, `<iso> [watch:intake.gh] gh-intake: <msg>` |
//!
//! The post-check (`work bead(s) carry no <scope> label`) is the next tick's observe.
//!
//! # Silence
//!
//! An idle tick (no new issues, every number seen) emits only the probe, and no
//! `intake.gh.state` when the seen sets and `last_ok` (carried at hour granularity) are unchanged.
//!
//! # Replaces
//!
//! `spira-gh-intake-prod.service` and `.timer` and their `world guard`; per tick `broker token`,
//! `bdq list/create/note`, `git remote` ×2 per repo, `mail send operator` and the `spira-lc`
//! close. Pinned config: [`KEYS`]. `closeout`, `unlanded-scan` and `backfill` stay library calls.
//!
//! # Reverse
//!
//! Release rollback restores the unit and timer; short of that, Observe mode shadows every write.
//! The carried seen set is a superset of the old seen file, so seeding the file from it goes back.

use std::collections::BTreeMap;

use gh_intake::logic as gh;
use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{commands, Cycle, Transition, INBOX};
use crate::watchers::{carried, iso8601};
use crate::Effects;

pub const NAME: &str = "intake.gh";
pub const STREAM: &str = "task.intake.gh";
pub const TICK: EventKindTag = "intake.gh.tick";
pub const STATE: EventKindTag = "intake.gh.state";

pub const OBSERVE: &str = "intake.gh.observe";
pub const FILE_WORK: &str = "intake.gh.file_work";
pub const FILE_UNTRUSTED: &str = "intake.gh.file_untrusted";
pub const NOTE_SEEN: &str = "intake.gh.note_seen";
pub const CLOSE_PROMOTED: &str = "intake.gh.close_promoted";
pub const DIGEST: &str = "intake.gh.digest";
pub const UNVERIFIABLE: &str = "intake.gh.unverifiable";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE_WORK, FILE_UNTRUSTED, NOTE_SEEN, CLOSE_PROMOTED, DIGEST, UNVERIFIABLE, INBOX];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INTAKE_GH_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_GH_INTAKE_REPO", "SPIRA_GH_INTAKE_BEAD_REPO", "SPIRA_GH_INTAKE_PRIORITY", "SPIRA_GH_INTAKE_API", "SPIRA_SCOPE_LABEL", "SPIRA_PLAN_LABEL", "SPIRA_MAIL_BIN", "SPIRA_METRICS_ENDPOINT"];

const OBSERVE_DEADLINE_MS: u64 = 120_000;
const STORE_DEADLINE_MS: u64 = 120_000;
const NOTE_DEADLINE_MS: u64 = 30_000;
const INBOX_DEADLINE_MS: u64 = 10_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: OBSERVE_DEADLINE_MS };
const HOUR_S: u64 = 3_600;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub slug: String,
    pub bead_repo: String,
    pub root_ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownRef {
    pub id: String,
    pub ext_ref: String,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhIssue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author: String,
    pub is_pr: bool,
}

/// One slug's issue pages as the probe read them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pages {
    Issues(Vec<GhIssue>),
    RateLimited { reset: u64 },
    Error(String),
}

/// GitHub's trust answer for one issue, settled inside the probe. `actor` is the login that
/// applied `spira:accept`; `None` with `has_access` is a trusted author or the repo's own bot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accept {
    pub slug: String,
    pub number: u64,
    pub actor: Option<String>,
    pub has_access: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub targets: Vec<Target>,
    pub store: Vec<KnownRef>,
    pub store_total: u64,
    pub now: u64,
    pub pages: BTreeMap<String, Pages>,
    pub accept: Vec<Accept>,
    /// (slug, login, hidden) for each contributor whose membership GitHub hides.
    pub contributor_hidden: Vec<(String, String, bool)>,
}

/// `intake.gh.state`: the issue numbers handled per slug (the first read's baseline and every
/// number acted on since), and the last good fetch, by hour.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub seen: BTreeMap<String, Vec<u64>>,
    pub last_ok_hour: Option<u64>,
}

/// One issue's life at intake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IssueState {
    Unseen,
    SeenBaseline,
    WorkFiled,
    UntrustedRecorded,
    Promoted,
}

pub const TRANSITIONS: &[Transition<IssueState>] = &[
    Transition { from: IssueState::Unseen, on: "baseline", to: IssueState::SeenBaseline, effects: &[] },
    Transition { from: IssueState::Unseen, on: "trusted", to: IssueState::WorkFiled, effects: &[FILE_WORK] },
    Transition { from: IssueState::Unseen, on: "untrusted", to: IssueState::UntrustedRecorded, effects: &[FILE_UNTRUSTED, DIGEST] },
    Transition { from: IssueState::UntrustedRecorded, on: "accepted", to: IssueState::Promoted, effects: &[FILE_WORK, CLOSE_PROMOTED] },
];

/// The issue's next state on `on`, or a refusal naming the state it is in.
pub fn step(from: IssueState, on: &str) -> Result<IssueState, String> {
    TRANSITIONS.iter().find(|t| t.from == from && t.on == on).map(|t| t.to).ok_or_else(|| format!("{NAME}: an issue in {from:?} has no transition on {on}"))
}

/// A bead `file_work` or `file_untrusted` writes. The labels and priority are the runner's,
/// from the pinned config: `<scope>,<lane>,repo:<bead_repo>` at the configured priority for
/// work, `gh-untrusted` at 4 for an untrusted record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub title: String,
    pub ext_ref: String,
    pub bead_repo: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Close {
    pub id: String,
    pub reason: String,
}

/// One slug's new untrusted issues: (number, login, title, body).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Digest {
    pub slug: String,
    pub items: Vec<(u64, String, String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unverifiable {
    pub slug: String,
    pub login: String,
}

fn act<T: Serialize>(name: &str, body: &T, deadline_ms: u64) -> EffectSpec {
    EffectSpec::by_input(name, serde_json::to_string(body).unwrap_or_default(), deadline_ms)
}

fn refusal(now: u64, msg: &str) -> EffectSpec {
    EffectSpec::by_input(INBOX, format!("{} [watch:{NAME}] gh-intake: {msg}", iso8601(now)), INBOX_DEADLINE_MS)
}

fn find<'a>(store: &'a [KnownRef], ext_ref: &str) -> Option<&'a KnownRef> {
    store.iter().find(|k| k.ext_ref == ext_ref)
}

/// The pure decision over one snapshot: the next carry and the acts, each an effect spec.
///
/// `logic::ingest_repo`'s rules, in its order, over answers the probe already has. Two
/// differences, both forced by the seen file becoming carried state: the first read of a slug
/// is the read with no carried set (the old file's absence), so it baselines even when the
/// store already holds refs for it; and every number acted on joins the set, so a tracked ref
/// is noted once, not every tick.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<EffectSpec>) {
    let mut next = carry.clone();
    let mut acts = vec![];
    if snap.halted || snap.targets.is_empty() {
        return (next, acts);
    }
    if snap.store_total == 0 {
        return (next, vec![refusal(snap.now, "the store reports zero beads — the query is broken, not the store empty")]);
    }
    if let Some(t) = snap.targets.iter().find(|t| !t.root_ok) {
        let msg = format!("repo:{r} does not resolve to a checkout through $SPIRA_REPO_MAP — every ingested bead would be parked on first claim; add {r} to that map, or set SPIRA_GH_INTAKE_BEAD_REPO to a name that resolves", r = t.bead_repo);
        return (next, vec![refusal(snap.now, &msg)]);
    }
    let work: Vec<KnownRef> = snap.store.iter().filter(|k| k.ext_ref.starts_with("github:")).cloned().collect();
    let untrusted: Vec<KnownRef> = snap.store.iter().filter(|k| k.ext_ref.starts_with("github-untrusted:")).cloned().collect();
    for t in &snap.targets {
        let issues = match snap.pages.get(&t.slug) {
            Some(Pages::Issues(list)) => list,
            Some(Pages::RateLimited { reset }) => {
                let last = carry.last_ok_hour.map(|h| h * HOUR_S);
                match last {
                    None => next.last_ok_hour = Some(snap.now / HOUR_S),
                    Some(last) if snap.now.saturating_sub(last) > gh::RATE_LIMIT_ALARM_SECS as u64 => {
                        let reset = if *reset == 0 { "unknown".to_string() } else { format!("{reset} (epoch seconds)") };
                        acts.push(refusal(snap.now, &format!("GitHub's API rate limit has refused every poll since the last success at {last} (epoch seconds), {}s ago — reset {reset}", snap.now - last)));
                    }
                    Some(_) => {}
                }
                break;
            }
            Some(Pages::Error(_)) | None => continue,
        };
        next.last_ok_hour = Some(snap.now / HOUR_S);
        let mut open: Vec<&GhIssue> = issues.iter().filter(|i| !i.is_pr).collect();
        open.sort_by_key(|i| i.number);
        let Some(skip) = next.seen.get(&t.slug).cloned() else {
            next.seen.insert(t.slug.clone(), open.iter().map(|i| i.number).collect());
            continue;
        };
        let mut handled = skip.clone();
        let mut new_untrusted = vec![];
        let mut unverifiable: Vec<String> = vec![];
        let mut note_unverifiable = |login: &str, acts: &mut Vec<EffectSpec>| {
            if !unverifiable.iter().any(|l| l == login) {
                unverifiable.push(login.to_string());
                acts.push(act(UNVERIFIABLE, &Unverifiable { slug: t.slug.clone(), login: login.into() }, NOTE_DEADLINE_MS));
            }
        };
        for issue in open.into_iter().filter(|i| !skip.contains(&i.number)) {
            handled.push(issue.number);
            let ext_ref = format!("github:{}#{}", t.slug, issue.number);
            let uref = format!("github-untrusted:{}#{}", t.slug, issue.number);
            if let Some(existing) = find(&work, &ext_ref) {
                acts.push(act(NOTE_SEEN, &Note { id: existing.id.clone(), text: gh::seen_again_note(&ext_ref, issue.number) }, NOTE_DEADLINE_MS));
                continue;
            }
            let accept = snap.accept.iter().find(|a| a.slug == t.slug && a.number == issue.number);
            let becomes_work = accept.is_some_and(|a| a.has_access);
            let actor = accept.and_then(|a| a.actor.clone()).unwrap_or_default();
            if becomes_work {
                if let Some(u) = find(&untrusted, &uref) {
                    acts.push(act(CLOSE_PROMOTED, &Close { id: u.id.clone(), reason: gh::promoted_reason(&actor) }, NOTE_DEADLINE_MS));
                }
                let own_bot = actor.is_empty() && issue.author == "github-actions[bot]";
                let body = gh::work_body(&ext_ref, &issue.author, own_bot, &issue.body);
                acts.push(act(FILE_WORK, &Filing { title: issue.title.clone(), ext_ref, bead_repo: t.bead_repo.clone(), body }, STORE_DEADLINE_MS));
                continue;
            }
            if accept.is_some_and(|a| a.hidden) && !actor.is_empty() {
                note_unverifiable(&actor, &mut acts);
            }
            if find(&untrusted, &uref).is_some() {
                continue;
            }
            if snap.contributor_hidden.iter().any(|(s, l, hidden)| *hidden && s == &t.slug && l == &issue.author) {
                note_unverifiable(&issue.author, &mut acts);
            }
            let body = gh::untrusted_body(&uref, &issue.author, &issue.body);
            acts.push(act(FILE_UNTRUSTED, &Filing { title: issue.title.clone(), ext_ref: uref, bead_repo: t.bead_repo.clone(), body }, STORE_DEADLINE_MS));
            new_untrusted.push((issue.number, issue.author.clone(), issue.title.clone(), issue.body.clone()));
        }
        if !new_untrusted.is_empty() {
            acts.push(act(DIGEST, &Digest { slug: t.slug.clone(), items: new_untrusted }, NOTE_DEADLINE_MS));
        }
        handled.sort_unstable();
        handled.dedup();
        next.seen.insert(t.slug.clone(), handled);
    }
    (next, acts)
}

/// The post-check `logic::ingest_repo` ran after filing, run on the next tick's snapshot: every
/// work bead that carries no `scope` label, which no rune can claim.
pub fn unclaimable(snap: &Snapshot, scope: &str) -> Vec<String> {
    snap.store.iter().filter(|k| k.ext_ref.starts_with("github:") && !k.labels.iter().any(|l| l == scope)).map(|k| format!("{} {}", k.id, k.ext_ref)).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntakeGh {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl IntakeGh {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(IntakeGh { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("intake.gh/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    /// A write's input with the pinned config beside the act, so its runner reads no config.
    fn pinned(&self, a: EffectSpec) -> EffectSpec {
        if a.name == INBOX {
            return a;
        }
        let act: serde_json::Value = serde_json::from_str(&a.input).unwrap_or(serde_json::Value::Null);
        EffectSpec { input: serde_json::json!({ "cfg": self.config, "act": act }).to_string(), ..a }
    }
}

impl Handler for IntakeGh {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let input = || serde_json::json!({ "cfg": self.config, "seen": carry.seen }).to_string();
        CYCLE.handle(self.interval_ms, view, ev, input, |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            let (next, mut acts) = decide(&carry, &snap);
            let scope = self.config.get("SPIRA_SCOPE_LABEL").map(String::as_str).unwrap_or("");
            let left = if scope.is_empty() || snap.halted { vec![] } else { unclaimable(&snap, scope) };
            if !left.is_empty() {
                acts.push(refusal(snap.now, &format!("{} work bead(s) carry no {scope} label and no rune can claim them: {}", left.len(), left.join(", "))));
            }
            commands(&carry, &next, STATE, acts.into_iter().map(|a| self.pinned(a)).collect())
        })
    }
}

// ── runners ─────────────────────────────────────────────────────────────────────────────────

/// A write's input: the pinned config and the act `decide` asked for.
#[derive(Deserialize)]
struct Pinned<T> {
    cfg: BTreeMap<String, String>,
    act: T,
}

fn parse<T: serde::de::DeserializeOwned>(input: &str) -> Result<Pinned<T>, String> {
    serde_json::from_str(input).map_err(|e| format!("{NAME}: unreadable input: {e}"))
}

fn get<'a>(cfg: &'a BTreeMap<String, String>, k: &str) -> &'a str {
    cfg.get(k).map(String::as_str).unwrap_or("")
}

fn store(e: &Effects) -> Result<serde_json::Value, String> {
    gh_intake::real::list_all_on(&spira_config::lc_call::LcClient::socket(&e.world.lc_socket)).ok_or_else(|| "could not read the store".to_string())
}

fn bd() -> Result<gh_intake::real::RealBd, String> {
    Ok(gh_intake::real::RealBd { bdq: spira_config::release_env::tool("bdq")? })
}

fn mail(cfg: &BTreeMap<String, String>) -> gh_intake::real::RealMail {
    gh_intake::real::RealMail::new(Some(get(cfg, "SPIRA_MAIL_BIN").to_string()).filter(|m| !m.is_empty()))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) // clock-safe: the snapshot's stamp, read once per probe
}

#[derive(Deserialize)]
struct ObserveInput {
    cfg: BTreeMap<String, String>,
    #[serde(default)]
    seen: BTreeMap<String, Vec<u64>>,
}

/// `intake.gh.observe`: the targets, the store's refs, every open issue page and, for each
/// issue not yet handled, GitHub's trust answer.
pub fn run_observe(e: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    use gh_intake::ports::Repo;
    let input: ObserveInput = serde_json::from_str(&s.input).map_err(|e| format!("{NAME}: unreadable input: {e}"))?;
    let cfg = &input.cfg;
    let now = now_secs();
    let halted = e.world.run_dir.join("world.halted").exists();
    let empty = Snapshot { halted, targets: vec![], store: vec![], store_total: 0, now, pages: BTreeMap::new(), accept: vec![], contributor_hidden: vec![] };
    if halted {
        return serde_json::to_string(&empty).map_err(|e| e.to_string());
    }
    let home = spira_config::resolve::locate_home_for_process()?;
    let repo = gh_intake::real::RealRepo { spira_home: home.to_string_lossy().into_owned() };
    let mut targets: Vec<Target> = vec![];
    let explicit = get(cfg, "SPIRA_GH_INTAKE_REPO");
    if !explicit.is_empty() {
        let bead_repo = get(cfg, "SPIRA_GH_INTAKE_BEAD_REPO").to_string();
        targets.push(Target { slug: explicit.into(), root_ok: repo.root_with_git(&bead_repo).is_some(), bead_repo });
    }
    for name in repo.all_names() {
        if let Some(slug) = repo.forge_slug(&name) {
            if !targets.iter().any(|t| t.slug.eq_ignore_ascii_case(&slug)) {
                targets.push(Target { slug, bead_repo: name, root_ok: true });
            }
        }
    }
    if targets.is_empty() {
        return serde_json::to_string(&empty).map_err(|e| e.to_string());
    }
    let listed = store(e)?;
    let store: Vec<KnownRef> = gh_intake::store::ingested(&listed).into_iter().map(|k| KnownRef { id: k.id, ext_ref: k.ext_ref, labels: k.labels }).collect();
    let store_total = gh_intake::store::total_beads(&listed) as u64;
    let api = Some(get(cfg, "SPIRA_GH_INTAKE_API")).filter(|a| !a.is_empty()).unwrap_or(gh::GITHUB_API).to_string();
    let http = gh_intake::real::RealHttp::from_broker();
    let (mut pages, mut accept, mut hidden) = (BTreeMap::new(), vec![], vec![]);
    for t in &targets {
        match gh::fetch_open_issues(&http, &api, &t.slug) {
            Err(gh::Fetch::RateLimited) => {
                let reset = gh::rate_limit_reset(&http, &api).unwrap_or(0).max(0) as u64;
                pages.insert(t.slug.clone(), Pages::RateLimited { reset });
                break;
            }
            Err(gh::Fetch::Error(err)) => {
                pages.insert(t.slug.clone(), Pages::Error(err));
            }
            Ok(issues) => {
                let seen = input.seen.get(&t.slug);
                for i in issues.iter().filter(|i| seen.is_some_and(|s| !s.contains(&i.number))) {
                    let trusted = gh_intake::model::association_trusted(&i.association) || gh_intake::model::own_bot_acceptance(i);
                    if trusted {
                        accept.push(Accept { slug: t.slug.clone(), number: i.number, actor: None, has_access: true, hidden: false });
                        continue;
                    }
                    if gh_intake::model::has_accept_label(&i.labels) {
                        let actor = gh::accept_actor_of(&http, &api, &t.slug, i.number);
                        if !actor.is_empty() {
                            let has_access = gh::actor_has_access(&http, &api, &t.slug, &actor);
                            let hidden = !has_access && gh::membership_hidden(&http, &api, &t.slug, &actor);
                            accept.push(Accept { slug: t.slug.clone(), number: i.number, actor: Some(actor), has_access, hidden });
                            if has_access {
                                continue;
                            }
                        }
                    }
                    if i.association == "CONTRIBUTOR" {
                        hidden.push((t.slug.clone(), i.login.clone(), gh::membership_hidden(&http, &api, &t.slug, &i.login)));
                    }
                }
                let list = issues.into_iter().map(|i| GhIssue { number: i.number, title: i.title, body: i.body, author: i.login, is_pr: false }).collect();
                pages.insert(t.slug.clone(), Pages::Issues(list));
            }
        }
    }
    serde_json::to_string(&Snapshot { halted, targets, store, store_total, now, pages, accept, contributor_hidden: hidden }).map_err(|e| e.to_string())
}

fn file(e: &Effects, s: &EffectSpec, labels: impl Fn(&BTreeMap<String, String>, &Filing) -> (String, String)) -> Result<String, String> {
    use gh_intake::ports::Bd;
    let p: Pinned<Filing> = parse(&s.input)?;
    let known = gh_intake::store::ingested(&store(e)?);
    if gh_intake::store::find_ref(&known, &p.act.ext_ref).is_some() {
        return Err(format!("{} is already in the store — refusing a second bead", p.act.ext_ref));
    }
    let (labels, priority) = labels(&p.cfg, &p.act);
    if bd()?.create(&p.act.title, &p.act.ext_ref, &labels, &priority, p.act.body.as_bytes()) {
        Ok(format!("filed {}", p.act.ext_ref))
    } else {
        Err(format!("could not create a bead for {}", p.act.ext_ref))
    }
}

/// `intake.gh.file_work`: a work bead for a trusted issue, refused when its ref is already filed.
pub fn run_file_work(e: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    file(e, s, |cfg, f| (format!("{},{},repo:{}", get(cfg, "SPIRA_SCOPE_LABEL"), get(cfg, "SPIRA_PLAN_LABEL"), f.bead_repo), get(cfg, "SPIRA_GH_INTAKE_PRIORITY").to_string()))
}

/// `intake.gh.file_untrusted`: the untrusted record, refused when it already exists.
pub fn run_file_untrusted(e: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    file(e, s, |_, _| (gh::UNTRUSTED_LABEL.to_string(), "4".to_string()))
}

/// `intake.gh.note_seen`: the re-ingest note on the bead already tracking the ref.
pub fn run_note_seen(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    use gh_intake::ports::Bd;
    let p: Pinned<Note> = parse(&s.input)?;
    bd()?.note(&p.act.id, &p.act.text).then(|| format!("noted {}", p.act.id)).ok_or_else(|| format!("could not note {}", p.act.id))
}

/// `intake.gh.close_promoted`: closes the untrusted record through the lifecycle machine.
pub fn run_close_promoted(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    use gh_intake::ports::Bd;
    let p: Pinned<Close> = parse(&s.input)?;
    bd()?.close(&p.act.id, &p.act.reason).then(|| format!("closed {}", p.act.id)).ok_or_else(|| format!("could not close {}", p.act.id))
}

/// `intake.gh.digest`: one operator note per slug listing its new untrusted issues.
pub fn run_digest(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    use gh_intake::ports::Mail;
    let p: Pinned<Digest> = parse(&s.input)?;
    if p.act.items.is_empty() {
        return Err("an empty digest is not sent".into());
    }
    let subject = gh::digest_subject(p.act.items.len(), &p.act.slug);
    mail(&p.cfg).send_operator_note(&subject, gh::render_digest(&p.act.items, &p.act.slug).as_bytes()).then(|| subject.clone()).ok_or_else(|| format!("could not send: {subject}"))
}

/// `intake.gh.unverifiable`: the operator note naming a login whose membership GitHub hides.
pub fn run_unverifiable(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    use gh_intake::ports::Mail;
    let p: Pinned<Unverifiable> = parse(&s.input)?;
    let (subject, body) = gh::unverifiable_note(&p.act.login, &p.act.slug);
    mail(&p.cfg).send_operator_note(&subject, body.as_bytes()).then(|| subject.clone()).ok_or_else(|| format!("could not send: {subject}"))
}
