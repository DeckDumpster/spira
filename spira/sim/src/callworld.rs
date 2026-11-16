//! A call world (`driver = "calls"`): the scheduler's `sentinel-call` decision played pass by pass
//! over a fleet the scenario shapes, with every rune's call settings read from the home's own
//! rune files. A pass lets sessions start and exit, says what is ready, and states what the
//! pass must call and what the cockpit's `SP_BOUND` must then read.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use spira_runtime::tasks::a::sentinel_call as sc;

pub const DRIVER: &str = "calls";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rune {
    pub uses: Option<Vec<String>>,
    pub reactive: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pass {
    pub name: String,
    /// Sessions that were live before the pass, by rune.
    #[serde(default)]
    pub starts: BTreeMap<String, u32>,
    /// Sessions that finished before the pass, by rune.
    #[serde(default)]
    pub exits: BTreeMap<String, u32>,
    /// Ready beads by rune; replaces the previous pass's counts when present.
    pub ready: Option<BTreeMap<String, u32>>,
    /// The operator's limits on the other bound classes from this pass on; replaces the previous.
    pub limits: Option<BTreeMap<String, u32>>,
    pub expect_calls: Vec<String>,
    /// `class held limit waiting over` per class, `*` matching any field.
    pub expect_bound: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    #[serde(default)]
    pub driver: String,
    pub max_dwarves: u32,
    pub max_live: u32,
    /// Declarations laid over the rune files, by rune.
    #[serde(default, rename = "rune")]
    pub runes: BTreeMap<String, Rune>,
    #[serde(default, rename = "pass")]
    pub passes: Vec<Pass>,
}

pub fn parse(text: &str) -> Result<Script, String> {
    let s: Script = spira_config::parse_foreign(text).map_err(|e| format!("call scenario: {e}"))?;
    if s.driver != DRIVER {
        return Err(format!("call scenario: driver is {:?}, not {DRIVER:?}", s.driver));
    }
    if s.passes.is_empty() {
        return Err("call scenario: no pass".into());
    }
    Ok(s)
}

pub fn is_calls(text: &str) -> bool {
    #[derive(Deserialize)]
    struct Driver {
        #[serde(default)]
        driver: String,
    }
    spira_config::parse_foreign::<Driver>(text).is_ok_and(|d| d.driver == DRIVER)
}

/// What one pass called and the `SP_BOUND` value the cockpit snapshot then carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub name: String,
    pub calls: Vec<String>,
    pub sp_bound: String,
}

fn rune_cfgs(home: &Path, overlay: &BTreeMap<String, Rune>) -> Result<(Vec<String>, BTreeMap<String, sc::RuneCfg>), String> {
    let get = |f: &str, k: &str, d: &str| spira_config::runes::rune_get(home, f, k, d);
    let (mut roster, mut cfgs) = (vec![], BTreeMap::new());
    for f in spira_config::runes::spira_runes(home, None).split_whitespace() {
        if get(f, "RUNE_CALL", "auto") != "auto" || get(f, "RUNE_ROLE", "task") == "party" {
            continue;
        }
        let uses = match overlay.get(f).and_then(|r| r.uses.clone()) {
            Some(u) => u,
            None => spira_config::runes::rune_uses(home, f)?,
        };
        let cfg = sc::RuneCfg { single_flight: get(f, "RUNE_SINGLE_FLIGHT", "0") == "1", elastic: get(f, "RUNE_ELASTIC", "0") == "1", uses, reactive: overlay.get(f).and_then(|r| r.reactive).unwrap_or_else(|| get(f, "RUNE_REACTIVE", "0") == "1") };
        roster.push(f.to_string());
        cfgs.insert(f.to_string(), cfg);
    }
    if let Some(unknown) = overlay.keys().find(|r| !cfgs.contains_key(*r)) {
        return Err(format!("call scenario: rune {unknown} is not a callable rune of {}", home.display()));
    }
    Ok((roster, cfgs))
}

fn matches_bound(want: &str, got: &str) -> bool {
    let (w, g): (Vec<&str>, Vec<&str>) = (want.split_whitespace().collect(), got.split_whitespace().collect());
    w.len() == g.len() && w.iter().zip(&g).all(|(w, g)| *w == "*" || w == g)
}

/// Plays the passes in order; the fleet carries from one to the next, a call counting as live.
pub fn run(home: &Path, script: &Script) -> Result<Vec<Outcome>, String> {
    let (roster, runes) = rune_cfgs(home, &script.runes)?;
    let (mut live, mut ready, mut limits) = (BTreeMap::<String, u32>::new(), BTreeMap::<String, Option<u32>>::new(), BTreeMap::<String, u32>::new());
    let (mut carry, mut out) = (sc::Carry::default(), vec![]);
    let known = |r: &str| runes.contains_key(r).then_some(()).ok_or_else(|| format!("call scenario: {r} is not a callable rune"));
    for (n, pass) in script.passes.iter().enumerate() {
        for (r, k) in &pass.starts {
            known(r)?;
            *live.entry(r.clone()).or_default() += k;
        }
        for (r, k) in &pass.exits {
            known(r)?;
            let held = live.entry(r.clone()).or_default();
            *held = held.checked_sub(*k).ok_or_else(|| format!("call scenario: pass {} exits {k} {r} but {held} are live", pass.name))?;
        }
        if let Some(rd) = &pass.ready {
            ready = roster.iter().map(|f| (f.clone(), Some(rd.get(f).copied().unwrap_or(0)))).collect();
            if let Some(bad) = rd.keys().find(|r| !runes.contains_key(*r)) {
                return Err(format!("call scenario: {bad} is not a callable rune"));
            }
        }
        if let Some(l) = &pass.limits {
            limits = l.clone();
        }
        let snap = sc::Snapshot {
            live_by_rune: live.clone(),
            ready_by_rune: roster.iter().map(|f| (f.clone(), ready.get(f).copied().flatten().or(Some(0)))).collect(),
            urgent: Some(vec![]),
            express_ready: Some(0),
            max_live: Some(script.max_live),
            max_dwarves: Some(script.max_dwarves),
            class_limits: limits.clone(),
            roster: roster.clone(),
            runes: runes.clone(),
            now_ms: (n as u64 + 1) * 60_000,
            ..sc::Snapshot::default()
        };
        let (next, acts) = sc::decide(&carry, &snap);
        let calls: Vec<String> = acts.iter().filter(|a| a.effect == sc::UNIT_START).filter_map(|a| serde_json::from_str::<sc::Start>(&a.input).ok()).map(|s| s.rune).collect();
        for c in &calls {
            *live.entry(c.clone()).or_default() += 1;
        }
        let sp_bound = next.bound.iter().flatten().map(|b| format!("{} {} {} {} {}", b.class, b.held, b.limit, b.waiting, u8::from(b.over))).collect::<Vec<_>>().join(";");
        carry = next;
        out.push(Outcome { name: pass.name.clone(), calls, sp_bound });
    }
    Ok(out)
}

/// Why the run missed the script's expectations, if it did.
pub fn missed(script: &Script, out: &[Outcome]) -> Option<String> {
    for (pass, o) in script.passes.iter().zip(out) {
        let mut calls = o.calls.clone();
        calls.sort();
        let mut want = pass.expect_calls.clone();
        want.sort();
        if calls != want {
            return Some(format!("pass {}: called {:?}, expected {:?} (SP_BOUND='{}')", o.name, o.calls, pass.expect_calls, o.sp_bound));
        }
        if let Some(want) = &pass.expect_bound {
            let got: Vec<&str> = o.sp_bound.split(';').filter(|s| !s.is_empty()).collect();
            if got.len() != want.len() || !want.iter().zip(&got).all(|(w, g)| matches_bound(w, g)) {
                return Some(format!("pass {}: SP_BOUND='{}', expected {:?}", o.name, o.sp_bound, want));
            }
        }
    }
    None
}
