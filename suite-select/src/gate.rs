//! The landing gate's pipeline (DESIGN.md "The gate pipeline") — what `gate-touched.sh` did,
//! failing closed where it swallowed.

use crate::budget::{self, TierCaps};
use crate::corpus::{self, Corpus};
use crate::glob::case_match;
use crate::io::{self, Git};
use crate::names::{is_suite_name, split_list};
use crate::select::{self, Buckets, Change, Fail, Options};
use crate::timing;
use crate::{refuse, Refusal};
use std::path::PathBuf;

/// The gate command's environment, read once.
#[derive(Clone, Debug, PartialEq)]
pub struct GateEnv {
    pub repo: PathBuf,
    pub files: Option<PathBuf>,
    pub tiers: Option<Vec<String>>,
    pub all: bool,
    pub suites_off: bool,
    pub always_covers: Vec<String>,
    pub ejected: String,
    pub budget_secs: f64,
    /// testenv's setup share of the budget, percent, clamped 10..=90 exactly as testenv
    /// clamps `SPIRA_TESTENV_SETUP_SHARE` (testenv DESIGN.md D9).
    pub setup_share: u64,
    /// Whether a runner enforces the budget on the whole trial, so setup must be reserved:
    /// true when `SPIRA_GATE_BUDGET` is set — the landing gate always sets it and its gate
    /// string hands the same value to `testenv --deadline`. CI's call sets neither, runs
    /// testenv without a deadline, and reserves nothing.
    pub reserve_setup: bool,
    pub width: u64,
    pub runs: usize,
    pub run: Option<PathBuf>,
    pub suite_dir: PathBuf,
    pub caps: TierCaps,
    pub buckets: Buckets,
}

fn digits(v: &str) -> Option<u64> {
    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .then(|| v.parse().ok())
        .flatten()
}

impl GateEnv {
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<GateEnv, Refusal> {
        let or = |k: &str, d: &str| get(k).filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
        let budget_raw = or("SPIRA_GATE_BUDGET", "300");
        let Some(budget_secs) = budget::number(&budget_raw) else {
            return refuse(format!("SPIRA_GATE_BUDGET={budget_raw:?} is not a number of seconds"));
        };
        let share_raw = or("SPIRA_TESTENV_SETUP_SHARE", "50");
        let Some(setup_share) = digits(&share_raw) else {
            return refuse(format!("SPIRA_TESTENV_SETUP_SHARE={share_raw:?} is not a whole percent"));
        };
        let setup_share = setup_share.clamp(10, 90);
        let width_raw = or("SPIRA_BATCH_MAXPAR", &or("SPIRA_GATE_HOST_CORES", "1"));
        let width = digits(&width_raw).filter(|w| *w > 0).unwrap_or(1);
        Ok(GateEnv {
            repo: PathBuf::from(or("SPIRA_GATE_REPO", ".")),
            files: get("SPIRA_GATE_FILES").filter(|v| !v.is_empty()).map(PathBuf::from),
            tiers: select::parse_tiers(&or("SPIRA_GATE_TIERS", "T0,T1")),
            all: get("SPIRA_GATE_ALL").as_deref() == Some("1"),
            suites_off: get("SPIRA_GATE_SUITES").as_deref() == Some("off"),
            always_covers: or("SPIRA_CERTIFY_ALWAYS_COVERS", "spira/lib.sh")
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            ejected: get("SPIRA_GATE_EJECTED_SUITES").unwrap_or_default(),
            budget_secs,
            setup_share,
            reserve_setup: get("SPIRA_GATE_BUDGET").is_some_and(|v| !v.is_empty()),
            width,
            runs: 20,
            run: get("SPIRA_RUN").filter(|v| !v.is_empty()).map(PathBuf::from),
            suite_dir: PathBuf::from(or("SPIRA_BATCH_SUITE_DIR", "spira")),
            caps: TierCaps::from_env(get)?,
            buckets: Buckets::from_env(get),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateOut {
    /// Sorted, each once.
    pub suites: Vec<String>,
    pub log: Vec<String>,
}

fn changes(env: &GateEnv, g: &dyn Git, base: &str, head: &str) -> Result<Vec<Change>, Refusal> {
    match &env.files {
        Some(f) => io::file_changes(f),
        None => io::diff_changes(g, &env.repo, base, head),
    }
}

/// `SPIRA_GATE_SUITES=off`: only the suites whose `# covers:` (file part) names a changed
/// file that matches `SPIRA_CERTIFY_ALWAYS_COVERS`.
fn carve_out(env: &GateEnv, g: &dyn Git, base: &str, head: &str) -> Result<GateOut, Fail> {
    let crit: Vec<String> = changes(env, g, base, head)?
        .into_iter()
        .map(|c| c.path)
        .filter(|p| env.always_covers.iter().any(|pat| case_match(pat, p)))
        .collect();
    if crit.is_empty() {
        return Ok(GateOut {
            suites: vec![],
            log: vec!["suite-select gate: SPIRA_GATE_SUITES=off — no suites here; the batch CI run is the suite gate".into()],
        });
    }
    let corpus = Corpus::load(&env.suite_dir)?;
    let mut suites: Vec<String> = corpus
        .suites
        .iter()
        .filter(|s| {
            s.covers.iter().flatten().any(|tok| {
                let f = tok.split('#').next().unwrap_or("");
                !f.is_empty() && crit.iter().any(|c| case_match(f, c))
            })
        })
        .map(|s| s.name.clone())
        .collect();
    suites.sort();
    Ok(GateOut {
        suites,
        log: vec![format!(
            "suite-select gate: SPIRA_GATE_SUITES=off but the diff touches {} — running its covering suite(s) anyway",
            crit.join(" ")
        )],
    })
}

/// With `SPIRA_GATE_FILES`: the suites the base has that the suite directory also has, so a
/// suite the branch adds cannot select itself through its own covers. A base with no suites
/// at all falls back to the directory; a base that cannot be read is a refusal.
fn base_corpus(env: &GateEnv, g: &dyn Git, base: &str) -> Result<Corpus, Refusal> {
    let names: Vec<String> = g
        .ls_tree(&env.repo, base)?
        .into_iter()
        .filter_map(|p| p.strip_prefix("spira/").map(str::to_string))
        .filter(|n| !n.contains('/') && corpus::is_suite_file(n))
        .filter(|n| env.suite_dir.join(n).is_file())
        .collect();
    if names.is_empty() {
        Corpus::load(&env.suite_dir)
    } else {
        Corpus::load_named(&env.suite_dir, &names)
    }
}

/// THE GATE'S SELECTION: carve-out, or corpus/selection, then the budget cut, then the
/// ejected suites.
pub fn run(env: &GateEnv, g: &dyn Git, base: &str, head: &str) -> Result<GateOut, Fail> {
    let mut log: Vec<String> = Vec::new();
    let (corpus, covered) = if env.all {
        let c = Corpus::load(&env.suite_dir)?;
        let n = c.names();
        log.push(format!("suite-select gate: SPIRA_GATE_ALL=1 — the whole corpus ({} suites)", n.len()));
        (c, n)
    } else if env.suites_off {
        return carve_out(env, g, base, head);
    } else {
        let opts = Options {
            no_all_fallback: true,
            no_nocov: false,
            tiers: env.tiers.clone(),
        };
        let sel = match &env.files {
            Some(f) => {
                let corpus = base_corpus(env, g, base)?;
                let ch = io::file_changes(f)?;
                // A file list has no base/head pair to narrow `file#function` against.
                let s = select::select(&corpus, &ch, &mut |_| vec![], &env.buckets, &opts)?;
                (corpus, s)
            }
            None => {
                let corpus = Corpus::load(&env.suite_dir)?;
                let s = io::select_diff(g, &env.repo, &corpus, base, head, &env.buckets, &opts)?;
                (corpus, s)
            }
        };
        let (corpus, s) = sel;
        log.extend(s.log);
        (corpus, s.suites)
    };

    // The budget cut.
    let mut kept: Vec<String> = Vec::new();
    if !covered.is_empty() {
        let p90 = match &env.run {
            Some(r) => {
                let p = timing::load(r, env.runs)?;
                if p.skipped > 0 {
                    log.push(format!("suite-select gate: skipped {} unreadable suite-timing row(s)", p.skipped));
                }
                p.by_suite
            }
            None => Default::default(),
        };
        let setup = match &env.run {
            Some(r) if env.reserve_setup => timing::load_setup(r, env.runs)?,
            _ => None,
        };
        let (suite_budget, why) = suite_budget(env, setup);
        log.push(format!(
            "suite-select gate: the suites get {suite_budget:.0}s of the {}s budget ({why})",
            env.budget_secs
        ));
        let cands: Vec<&corpus::Suite> = covered.iter().filter_map(|n| corpus.get(n)).collect();
        let cut = budget::fill(&budget::rank(&cands, &p90, &env.caps), suite_budget, env.width);
        log.extend(cut.log);
        kept = cut.selected;
    }

    // Ejected suites: added after the cut, never ranked or dropped.
    let ejected: Vec<String> = split_list(&env.ejected)
        .into_iter()
        .filter(|n| is_suite_name(n) && env.suite_dir.join(n).is_file())
        .collect();
    let n_ej = ejected.len();
    kept.extend(ejected);
    kept.sort();
    kept.dedup();
    log.push(format!(
        "suite-select gate: {} suite(s) selected ({} covered, {} ejected)",
        kept.len(),
        covered.len(),
        n_ej
    ));
    Ok(GateOut { suites: kept, log })
}

/// THE SUITES' SHARE OF THE BUDGET (sp-govet): testenv's `--deadline` bounds the whole
/// trial, so the suites get the budget minus setup. Setup is predicted as the P90 of the
/// runner's measured `setup_secs`, capped at its share (testenv cuts setup there); with no
/// measurement, the share itself — the most setup testenv will allow.
pub fn suite_budget(env: &GateEnv, setup_p90: Option<f64>) -> (f64, String) {
    if !env.reserve_setup {
        return (
            env.budget_secs,
            "SPIRA_GATE_BUDGET unset: no runner deadline, nothing reserved for setup".into(),
        );
    }
    let cap = env.budget_secs * env.setup_share as f64 / 100.0;
    match setup_p90 {
        Some(p) => {
            let setup = p.clamp(0.0, cap);
            (env.budget_secs - setup, format!("setup P90 {p:.0}s measured, reserved {setup:.0}s"))
        }
        None => (
            env.budget_secs - cap,
            format!("no setup measured, reserved its {}% share, {cap:.0}s", env.setup_share),
        ),
    }
}

/// For tests and callers that hold a map instead of the process environment.
pub fn env_from_pairs(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let v: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |k: &str| v.iter().find(|(kk, _)| kk == k).map(|(_, vv)| vv.clone())
}


#[cfg(test)]
mod tests;
