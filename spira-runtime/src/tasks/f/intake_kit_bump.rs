//! `intake.kit_bump`: a new release of the kit crate becomes a bump bead per site. Replaces
//! `spira-kit-bump-prod` (`bin/kit-bump pass --config run/kit-bump.conf --state
//! run/kit-bump.state`). Pattern P1. Contract:
//! `wiki/projects/spira/designs/tasks/intake-kit-bump.md`.
//!
//! # Tick
//!
//! Every 10 min (`SPIRA_RUNTIME_INTAKE_KIT_BUMP_SECS`), single-flight, no `world guard`. When
//! `SPIRA_KIT_REGISTRY` is empty the task is unconfigured and a tick does nothing.
//!
//! # Observe
//!
//! `intake.kit_bump.observe` returns a [`Snapshot`]: `release` (`newest_release` over the sparse
//! index, in-process over HTTP as the heartbeat does; a 404 or 410 means nothing is published),
//! `sites` (`git show <ref>:<file>` and the sibling `Cargo.lock`, parsed by `parse_requirement`
//! and `parse_locked`; git stays a class B child) and `open` (the carried bump joined with the
//! `LcClient` show). Sites come from `SPIRA_KIT_SITES` resolved against the repo map; the
//! rendered conf file retires.
//!
//! # Decide
//!
//! The pure core is `kit_bump::pass` and `site_pass` with `title`, `body` and `semver::admits`.
//! Per site, against the newest release: locked at or past it is current; an open bump at that
//! target is current; an open bump at an older target is amended; otherwise file, breaking when
//! the requirement does not admit the release. Carried state is `intake.kit_bump.state { bumps }`.
//! Fixtures port `kit-bump/src/tests.rs` (6 cases) and the two `main.rs` config tests.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `intake.kit_bump.observe` (probe) | host (http) | 30 s | tick id | a registry version that is not semver |
//! | `intake.kit_bump.file` | store | 120 s | `<site>/<release>` | a site whose carry already holds an open bump |
//! | `intake.kit_bump.amend` | store | 120 s | `<site>/<release>` | a bump whose target already equals the release |
//!
//! file: `bead.sh file "<title>" --for <persona> --repo <site>` then the label `kit-bump`; the
//! title is `update <crate> lockfile to <v> in <site>` or `upgrade <crate> to <v> (breaking) in
//! <site>`. amend: `bead.sh amend <id> --replace --note "kit-bump: target moved to <v>"`. Each
//! success is followed by an Emit of the carry. Per-site failures are a wide-event field
//! (`kit_bump.failed`), not an inbox line.
//!
//! # Silence
//!
//! An idle tick (every site current, or no release published) emits the probe only, and no Emit
//! when the bumps map is unchanged.
//!
//! # Replaces
//!
//! `spira-kit-bump-prod.service` and `.timer` and the rendered `run/kit-bump.conf`; children
//! `curl`, `git show` ×2 per site, `spira-lc show`, `bead.sh file/amend/label`. Pinned config:
//! [`KEYS`] (`SPIRA_KIT_PERSONA` stands in for the `--persona` flag's `builder` default).
//!
//! # Reverse
//!
//! Release rollback restores the unit; the carry renders back to `kit-bump.state`
//! (`site\tid\ttarget`). Observe mode shadows `file` and `amend`.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use kit_bump::World;

use super::trigger_groom::walk;
use super::{commands, Cycle, Transition};
use crate::watchers::carried;

pub const NAME: &str = "intake.kit_bump";
pub const STREAM: &str = "task.intake.kit_bump";
pub const TICK: EventKindTag = "intake.kit_bump.tick";
pub const STATE: EventKindTag = "intake.kit_bump.state";

pub const OBSERVE: &str = "intake.kit_bump.observe";
pub const FILE: &str = "intake.kit_bump.file";
pub const AMEND: &str = "intake.kit_bump.amend";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE, AMEND];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INTAKE_KIT_BUMP_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_KIT_REGISTRY", "SPIRA_KIT_CRATE", "SPIRA_KIT_SITES", "SPIRA_KIT_PERSONA", "SPIRA_METRICS_ENDPOINT"];

const DEADLINE_MS: u64 = 30_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Site {
    pub locked: String,
    pub requirement: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Open {
    pub id: String,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub release: Result<Option<String>, String>,
    pub sites: BTreeMap<String, Result<Site, String>>,
    pub open: BTreeMap<String, Result<Option<Open>, String>>,
}

/// `intake.kit_bump.state`: per site, the open bump's bead id and target.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub bumps: BTreeMap<String, (String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SiteState {
    Current,
    BumpOpen,
}

pub const TRANSITIONS: &[Transition<SiteState>] = &[
    Transition { from: SiteState::Current, on: "file", to: SiteState::BumpOpen, effects: &[FILE] },
    Transition { from: SiteState::BumpOpen, on: "amend", to: SiteState::BumpOpen, effects: &[AMEND] },
    Transition { from: SiteState::BumpOpen, on: "landed", to: SiteState::Current, effects: &[] },
];

/// The site's next state on `on`, or a refusal naming the state it is in.
pub fn step(from: SiteState, on: &str) -> Result<SiteState, String> {
    walk(TRANSITIONS, from, on)
}

/// What `intake.kit_bump.file` and `intake.kit_bump.amend` are handed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bump {
    pub site: String,
    pub title: String,
    pub body: String,
    pub target: String,
    pub persona: String,
    /// The open bump an amend retargets.
    pub id: Option<String>,
}

const STORE_DEADLINE_MS: u64 = 120_000;

/// The pure decision, with `config` the pinned [`KEYS`]: the next carry and the acts. Per site,
/// as `kit_bump::site_pass` judges it against the newest release: locked at or past it is
/// current; an open bump at that target is current; an open bump at an older target is amended
/// (its carried target moves now); otherwise a bump is filed, breaking when the requirement does
/// not admit the release, and carried once the file answers its id. A bump the store reads as
/// closed leaves the carry. A site the probe could not read, or a release that is not semver,
/// acts on nothing.
pub fn decide(carry: &Carry, snap: &Snapshot, config: &BTreeMap<String, String>) -> (Carry, Vec<EffectSpec>) {
    let mut next = carry.clone();
    for (site, open) in &snap.open {
        if matches!(open, Ok(None)) {
            next.bumps.remove(site);
        }
    }
    let Ok(Some(release)) = &snap.release else { return (next, vec![]) };
    let Some(latest) = kit_bump::semver::parse_version(release) else { return (next, vec![]) };
    let get = |k: &str| config.get(k).cloned().unwrap_or_default();
    let (crate_name, persona) = (get("SPIRA_KIT_CRATE"), get("SPIRA_KIT_PERSONA"));
    let mut acts = vec![];
    for (name, file) in kit_bump::site_specs(&get("SPIRA_KIT_SITES")) {
        let Some(Ok(site)) = snap.sites.get(&name) else { continue };
        let open = match snap.open.get(&name) {
            Some(Ok(o)) => o.clone(),
            Some(Err(_)) => continue,
            None => next.bumps.get(&name).map(|(id, target)| Open { id: id.clone(), target: target.clone() }),
        };
        if kit_bump::semver::parse_version(&site.locked).is_none_or(|v| v >= latest) {
            continue;
        }
        let Some(admits) = kit_bump::semver::admits(&site.requirement, release) else { continue };
        let lib = kit_bump::Site { name: name.clone(), path: String::new(), git_ref: String::new(), file };
        let body = kit_bump::body(&lib, &crate_name, &site.requirement, &site.locked, release, !admits);
        let bump = |title: String, id: Option<String>| Bump { site: name.clone(), title, body: body.clone(), target: release.clone(), persona: persona.clone(), id };
        match open {
            Some(o) if &o.target == release => {}
            Some(o) => {
                next.bumps.insert(name.clone(), (o.id.clone(), release.clone()));
                acts.push(EffectSpec::keyed(AMEND, format!("{name}/{release}"), serde_json::to_string(&bump(String::new(), Some(o.id))).unwrap_or_default(), STORE_DEADLINE_MS));
            }
            None => acts.push(EffectSpec::keyed(FILE, format!("{name}/{release}"), serde_json::to_string(&bump(kit_bump::title(&lib, &crate_name, release, !admits), None)).unwrap_or_default(), STORE_DEADLINE_MS)),
        }
    }
    (next, acts)
}

/// The spec of the request `ev` answers, read back from the stream.
fn requested_spec(view: &Projection, ev: &Event) -> Option<EffectSpec> {
    #[derive(Deserialize)]
    struct Body {
        spec: EffectSpec,
    }
    let cause = ev.cause.as_ref()?;
    let req = view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED)?;
    serde_json::from_str::<Body>(&req.kind.body).ok().map(|b| b.spec)
}

/// What the observe is handed: the pinned config and the carried bumps whose openness it reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserveInput {
    pub config: BTreeMap<String, String>,
    pub bumps: BTreeMap<String, (String, String)>,
}

const TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];

fn real(e: &crate::Effects, persona: &str) -> kit_bump::real::Real {
    let lc = spira_store::lc_call::LcClient::socket(&e.world.lc_socket);
    kit_bump::real::Real::new(String::new(), String::new(), e.world.run_dir.join("kit-bump.state"), persona.into(), lc)
}

/// `intake.kit_bump.observe`: the newest release over HTTP, each site's requirement and locked
/// version from its base ref, and whether each carried bump is still open. An empty
/// `SPIRA_KIT_REGISTRY` is an unconfigured kit: nothing is published.
pub fn run_observe(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let input: ObserveInput = serde_json::from_str(&s.input).map_err(|e| format!("observe input: {e}"))?;
    let get = |k: &str| input.config.get(k).cloned().unwrap_or_default();
    let (registry, crate_name) = (get("SPIRA_KIT_REGISTRY"), get("SPIRA_KIT_CRATE"));
    if registry.is_empty() {
        return serde_json::to_string(&Snapshot { release: Ok(None), sites: BTreeMap::new(), open: BTreeMap::new() }).map_err(|e| e.to_string());
    }
    let release = kit_bump::real::fetch_release(&registry, &crate_name);
    let home = spira_config::resolve::locate_home_for_process()?;
    let reg = spira_config::repos::Registry::from_env(BTreeMap::new(), &home);
    let mut w = real(e, "");
    w.crate_name = crate_name;
    let sites = kit_bump::site_specs(&get("SPIRA_KIT_SITES"))
        .into_iter()
        .map(|(name, file)| {
            let site = kit_bump::real::resolve_site(&reg, &name, &file).and_then(|site| Ok(Site { locked: w.site_locked(&site)?, requirement: w.site_requirement(&site)? }));
            (name, site)
        })
        .collect();
    let lc = spira_store::lc_call::LcClient::socket(&e.world.lc_socket);
    let open = input
        .bumps
        .iter()
        .map(|(site, (id, target))| {
            let open = lc.call(&["show", id]).map_err(String::from).and_then(|r| match r.code {
                1 => Ok(None),
                0 => {
                    let doc: serde_json::Value = serde_json::from_str(&r.out).map_err(|e| format!("{id}: not JSON: {e}"))?;
                    let state = doc["bead"]["state"].as_str().ok_or_else(|| format!("{id}: no state"))?;
                    Ok((!TERMINAL.contains(&state)).then(|| Open { id: id.clone(), target: target.clone() }))
                }
                _ => Err(format!("cannot tell whether {id} is open: {}", r.why())),
            });
            (site.clone(), open)
        })
        .collect();
    serde_json::to_string(&Snapshot { release, sites, open }).map_err(|e| e.to_string())
}

fn lib_site(b: &Bump) -> kit_bump::Site {
    kit_bump::Site { name: b.site.clone(), path: String::new(), git_ref: String::new(), file: String::new() }
}

/// `intake.kit_bump.file`: the bump bead for the site, labelled `kit-bump`; answers its id.
pub fn run_file(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let b: Bump = serde_json::from_str(&s.input).map_err(|e| format!("file input: {e}"))?;
    real(e, &b.persona).file_bump(&lib_site(&b), &b.title, &b.body, &b.target)
}

/// `intake.kit_bump.amend`: the open bump retargeted at the release.
pub fn run_amend(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let b: Bump = serde_json::from_str(&s.input).map_err(|e| format!("amend input: {e}"))?;
    let id = b.id.clone().ok_or("an amend names no bump")?;
    real(e, &b.persona).amend_bump(&lib_site(&b), &kit_bump::Open { id: id.clone(), target: String::new() }, &b.body, &b.target).map(|()| id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntakeKitBump {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl IntakeKitBump {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(IntakeKitBump { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("intake.kit_bump/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Map { name: "bumps", drop: &[] }];

impl Handler for IntakeKitBump {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 == STREAM && ev.kind.tag == EFFECT_SUCCEEDED {
            if let Some(spec) = requested_spec(view, ev).filter(|s| s.name == FILE) {
                let Ok(b) = serde_json::from_str::<Bump>(&spec.input) else { return vec![] };
                let carry = carried::<Carry>(view, &ev.stream, STATE);
                let mut next = carry.clone();
                next.bumps.insert(b.site, (ev.kind.body.trim().to_string(), b.target));
                return commands(&carry, &next, STATE, vec![]);
            }
        }
        let input = || serde_json::to_string(&ObserveInput { config: self.config.clone(), bumps: carried::<Carry>(view, &ev.stream, STATE).bumps }).unwrap_or_default();
        CYCLE.handle(self.interval_ms, view, ev, input, |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            let (next, acts) = decide(&carry, &snap, &self.config);
            commands(&carry, &next, STATE, acts)
        })
    }
}
