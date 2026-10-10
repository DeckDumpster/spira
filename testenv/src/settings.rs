//! Every knob the runner reads (DESIGN.md §2.5), per Ryan 2026-10-05: THE ONE SOURCE OF
//! CONFIG is the file `$SPIRA_TOML` names. A registered key (`spira/conf.d/<NAME>`) is read
//! exactly once, through `spira_config::process::cfg`/`cfg_parse`; a key the file does not
//! declare — or a resolution that fails outright — is a refusal naming the key, never a
//! Rust-side default. `Source` survives only for the names below that are NOT registered
//! config at all (bootstrap vars, per-invocation identity, test-only knobs): those still
//! read the environment directly, exactly as before.

use spira_config::process::{cfg, cfg_parse};
use std::path::PathBuf;

pub struct Source<'a> {
    pub env: &'a dyn Fn(&str) -> Option<String>,
}

impl Source<'_> {
    /// Non-empty environment value. Only ever used for a NON-registered name (see the
    /// module doc) — a registered key goes through `cfg`/`cfg_parse` instead.
    pub fn get(&self, env: &str) -> Option<String> {
        (self.env)(env).filter(|v| !v.is_empty())
    }

    /// Set at all in the environment, even empty (SPIRA_BATCH_SKIP_INSTALL semantics differ).
    pub fn env_raw(&self, env: &str) -> Option<String> {
        (self.env)(env)
    }

    fn num<T: std::str::FromStr>(&self, env: &str) -> Option<T> {
        self.get(env).and_then(|v| v.trim().parse().ok())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub run: PathBuf,
    pub results_root: PathBuf,
    pub verdicts: PathBuf,
    pub verdict_ttl: u64,
    pub quarantine_max_age: u64,
    pub repeat_reason: Option<String>,
    /// 0 disables.
    pub suite_timeout: u64,
    pub maxpar_requested: Option<i64>,
    /// Set when SPIRA_BATCH_MAXPAR (declared in spira.toml) was given but is not usable:
    /// zero, negative, or not a whole number. Absent is never folded in here — only a given,
    /// bad value refuses (sp-tj8k3: a removed/empty key must stay "unset", and a zero or a
    /// typo must never be read as "unlimited" or silently become the default either).
    pub maxpar_refusal: Option<String>,
    pub maxpar_ceiling: Option<u32>,
    pub mem_reserve_mib: i64,
    pub mem_per_suite_mib: i64,
    pub mem_avail_mib: Option<i64>,
    /// 0 disables.
    pub psi_threshold: f64,
    pub orphan_min_age: u64,
    pub orphan_prefix: String,
    pub peak_warn_frac: u64,
    pub exec_fault_threshold: usize,
    pub liveness_retries: u32,
    pub liveness_sleep: u64,
    pub instance: Option<String>,
    pub suite_dir: Option<PathBuf>,
    pub skip_install: bool,
    pub tiers: String,
    pub mail_cmd: Option<PathBuf>,
    pub incident_cmd: Option<PathBuf>,
    pub suite_state_file: String,
    /// DESIGN.md §3.7: the checked-in skip-declaration allow list, read from the revision
    /// under test exactly like `suite_state_file`.
    pub skip_allowlist_file: String,
    pub select_head: Option<String>,
    pub round_batch_id: Option<String>,
    pub round_members: u64,
    pub run_id: String,
    pub scratch_slots: usize,
    /// sp-t26yx: a slot on the scratch root needs this much free there, and — on a tmpfs —
    /// this much MemAvailable; otherwise the run is refused (`scratch-short`).
    pub scratch_min_free_mib: u64,
    pub scratch_min_mem_mib: u64,
    /// DESIGN.md §11.2: warm slots tried under `--deadline` (0 = no warm path).
    pub warm_slots: usize,
    /// DESIGN.md D9: setup's share of `--deadline`, percent, clamped to 10..=90.
    pub setup_share: u64,
    /// DESIGN.md §11.2: how long a refill may wait for its slot and boot its spare.
    pub warm_boot_timeout: u64,
    /// sp-s8v5r: below this much free on the scratch root, drop idle warm slots
    /// oldest-first (law-reduce-the-count-never-throttle-the-job) — the same floor gate's
    /// LRU eviction of its tmpfs build targets reads, since both share the /tmp tmpfs.
    pub warm_shed_free_mib: u64,
    pub landing_containers: Option<PathBuf>,
    pub spira_db: Option<String>,
}

/// SPIRA_BATCH_MAXPAR: empty (the key carries no value anywhere) is `(None, None)` — the
/// scheduler's own bounded, host-derived default applies (`schedule::maxpar`). A positive
/// whole number is `(Some(n), None)` — a request, clamped to the hardware bound. Anything
/// else given (zero, negative, or not a number) is `(None, Some(reason))` — refused by name,
/// never folded into "unset" or "unlimited" (sp-tj8k3: a batch_maxpar of 0 in production
/// read as unlimited concurrency and put 61 containers on one host).
///
/// Pure over the already-fetched string (never calls `cfg` itself) so it stays testable
/// without touching spira-config's per-process resolution cache. `pub(crate)`: `run/tests.rs`
/// reuses it to build a `Settings` fixture directly from a fake env map (DESIGN.md's
/// top-level `run()`/`report()`/etc. now read `Settings` off `Deps`, not the environment).
pub(crate) fn resolve_maxpar(raw: &str) -> (Option<i64>, Option<String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    match trimmed.parse::<i64>() {
        Ok(n) if n > 0 => (Some(n), None),
        Ok(n) => (
            None,
            Some(format!(
                "SPIRA_BATCH_MAXPAR={n} is refused — 0 no longer means unlimited; omit the key for the scheduler's bounded default, or set a positive suite count"
            )),
        ),
        Err(_) => (
            None,
            Some(format!("SPIRA_BATCH_MAXPAR={trimmed:?} is not a whole number")),
        ),
    }
}

/// A declared value, empty read as absent — for a registered key whose `conf.d` entry
/// carries NO DEFAULT (resolves to the empty string unless spira.toml sets it). Pure over
/// the already-fetched string; see [`resolve_maxpar`].
fn optional_num<T: std::str::FromStr>(key: &str, raw: &str) -> Result<Option<T>, String>
where
    T::Err: std::fmt::Display,
{
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<T>()
        .map(Some)
        .map_err(|e| format!("{key}={trimmed:?} does not parse: {e}"))
}

impl Settings {
    /// `now` names a local run (SPIRA_BATCH_RUN_ID / GITHUB_RUN_ID are not registered config
    /// — see the module doc — so this is still a plain parameter, not a `cfg` read).
    pub fn load(src: &Source, now: u64) -> Result<Settings, String> {
        let run = PathBuf::from(cfg("SPIRA_RUN")?);
        let path = |env: &str| src.get(env).map(PathBuf::from);
        let (maxpar_requested, maxpar_refusal) = resolve_maxpar(&cfg("SPIRA_BATCH_MAXPAR")?);
        let spira_db = cfg("SPIRA_DB")?;
        Ok(Settings {
            results_root: path("SPIRA_BATCH_RESULTS").unwrap_or_else(|| run.join("batch-results")),
            verdicts: path("SPIRA_VERDICTS").unwrap_or_else(|| run.join("verdicts")),
            verdict_ttl: cfg_parse("SPIRA_VERDICT_TTL")?,
            quarantine_max_age: cfg_parse("SPIRA_QUARANTINE_MAX_AGE")?,
            repeat_reason: src.get("SPIRA_VERDICT_REPEAT_CONSIDERED"),
            suite_timeout: cfg_parse("SPIRA_SUITE_TIMEOUT")?,
            maxpar_requested,
            maxpar_refusal,
            maxpar_ceiling: optional_num("SPIRA_BATCH_MAXPAR_CEILING", &cfg("SPIRA_BATCH_MAXPAR_CEILING")?)?,
            mem_reserve_mib: cfg_parse("SPIRA_BATCH_MEM_RESERVE_MIB")?,
            mem_per_suite_mib: cfg_parse("SPIRA_BATCH_MEM_PER_SUITE_MIB")?,
            mem_avail_mib: optional_num("SPIRA_BATCH_MEM_AVAIL_MIB", &cfg("SPIRA_BATCH_MEM_AVAIL_MIB")?)?,
            psi_threshold: cfg_parse("SPIRA_BATCH_PSI_THRESHOLD")?,
            orphan_min_age: cfg_parse("SPIRA_BATCH_ORPHAN_MIN_AGE")?,
            orphan_prefix: src
                .get("SPIRA_BATCH_ORPHAN_PREFIX")
                .unwrap_or_else(|| "spira-batch-".into()),
            peak_warn_frac: cfg_parse("SPIRA_BATCH_PEAK_WARN_FRAC")?,
            exec_fault_threshold: src
                .num("SPIRA_BATCH_EXEC_FAULT_THRESHOLD")
                .unwrap_or(5)
                .max(1),
            liveness_retries: src
                .num("SPIRA_BATCH_LIVENESS_RETRIES")
                .unwrap_or(3)
                .max(1),
            liveness_sleep: src.num("SPIRA_BATCH_LIVENESS_SLEEP").unwrap_or(3),
            instance: src.get("SPIRA_BATCH_INSTANCE"),
            suite_dir: path("SPIRA_BATCH_SUITE_DIR"),
            skip_install: src
                .env_raw("SPIRA_BATCH_SKIP_INSTALL")
                .is_some_and(|v| !v.is_empty()),
            tiers: src
                .get("SPIRA_BATCH_TIERS")
                .unwrap_or_else(|| "T2,T3".into()),
            mail_cmd: path("SPIRA_BATCH_MAIL_CMD"),
            incident_cmd: path("SPIRA_BATCH_INCIDENT_CMD"),
            suite_state_file: cfg("SPIRA_SUITE_STATE_FILE")?,
            skip_allowlist_file: src
                .get("SPIRA_SKIP_ALLOWLIST_FILE")
                .unwrap_or_else(|| "spira/skip-allowlist.tsv".into()),
            select_head: src.get("SPIRA_GATE_SELECT_HEAD"),
            round_batch_id: src.get("SPIRA_ROUND_BATCH_ID"),
            round_members: src.num("SPIRA_ROUND_MEMBERS").unwrap_or(0),
            run_id: src
                .get("GITHUB_RUN_ID")
                .or_else(|| src.get("SPIRA_BATCH_RUN_ID"))
                .unwrap_or_else(|| format!("local-{now}")),
            scratch_slots: src.num("SPIRA_TESTENV_SCRATCH_SLOTS").unwrap_or(4),
            scratch_min_free_mib: src
                .num("SPIRA_TESTENV_SCRATCH_MIN_FREE_MIB")
                .unwrap_or(4096),
            scratch_min_mem_mib: src
                .num("SPIRA_TESTENV_SCRATCH_MIN_MEM_MIB")
                .unwrap_or(8192),
            warm_slots: src.num("SPIRA_TESTENV_WARM_SLOTS").unwrap_or(3),
            setup_share: src
                .num("SPIRA_TESTENV_SETUP_SHARE")
                .unwrap_or(50u64)
                .clamp(10, 90),
            warm_boot_timeout: src
                .num("SPIRA_TESTENV_WARM_BOOT_TIMEOUT")
                .unwrap_or(600u64)
                .max(1),
            warm_shed_free_mib: src.num("SPIRA_TMPFS_SHED_FREE_MIB").unwrap_or(6144),
            landing_containers: path("SPIRA_LANDING_CONTAINERS"),
            spira_db: Some(spira_db).filter(|v| !v.is_empty()),
            run,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A non-registered-key Source, standing in for the environment: the only thing
    /// `Settings::load` still reads directly. Every REGISTERED key now goes through
    /// `cfg`/`cfg_parse`, which resolve from spira-config's own per-process cache — not
    /// something a unit test can drive per-case, so that part of `load` is exercised by
    /// spira-config's own tests and by the pure helpers below, not here.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| m.get(k).cloned()
    }

    /// sp-tj8k3: unset must stay unset (no refusal, no override) — only a *given* bad value
    /// refuses. Zero and a non-numeric value both refuse, by name; a positive value never
    /// does.
    #[test]
    fn maxpar_unset_is_silent_zero_and_garbage_refuse() {
        assert_eq!(resolve_maxpar(""), (None, None));
        assert_eq!(resolve_maxpar("  "), (None, None));

        let (v, r) = resolve_maxpar("0");
        assert_eq!(v, None);
        assert!(r.as_deref().is_some_and(|r| r.contains('0')), "{r:?}");

        let (v, r) = resolve_maxpar("lots");
        assert_eq!(v, None);
        assert!(r.as_deref().is_some_and(|r| r.contains("lots")), "{r:?}");

        assert_eq!(resolve_maxpar("4"), (Some(4), None));
        assert_eq!(resolve_maxpar("-3").0, None);
    }

    #[test]
    fn optional_num_is_absent_on_empty_and_refuses_a_given_bad_value() {
        assert_eq!(optional_num::<u32>("SPIRA_BATCH_MAXPAR_CEILING", ""), Ok(None));
        assert_eq!(optional_num::<u32>("SPIRA_BATCH_MAXPAR_CEILING", "24"), Ok(Some(24)));
        assert!(optional_num::<u32>("SPIRA_BATCH_MAXPAR_CEILING", "nope").is_err());
    }

    #[test]
    fn run_id_prefers_github_then_explicit() {
        let src = Source { env: &env(&[("GITHUB_RUN_ID", "77"), ("SPIRA_BATCH_RUN_ID", "x")]) };
        let run_id = src
            .get("GITHUB_RUN_ID")
            .or_else(|| src.get("SPIRA_BATCH_RUN_ID"))
            .unwrap_or_else(|| "local-1000".into());
        assert_eq!(run_id, "77");

        let src = Source { env: &env(&[("SPIRA_BATCH_RUN_ID", "x")]) };
        let run_id = src
            .get("GITHUB_RUN_ID")
            .or_else(|| src.get("SPIRA_BATCH_RUN_ID"))
            .unwrap_or_else(|| "local-1000".into());
        assert_eq!(run_id, "x");
    }

    #[test]
    fn skip_install_needs_a_non_empty_value() {
        let src = Source { env: &env(&[("SPIRA_BATCH_SKIP_INSTALL", "1")]) };
        assert!(src.env_raw("SPIRA_BATCH_SKIP_INSTALL").is_some_and(|v| !v.is_empty()));

        let src = Source { env: &env(&[("SPIRA_BATCH_SKIP_INSTALL", "")]) };
        assert!(!src.env_raw("SPIRA_BATCH_SKIP_INSTALL").is_some_and(|v| !v.is_empty()));
    }
}
