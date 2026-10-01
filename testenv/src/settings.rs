//! Every knob the runner reads (DESIGN.md §2.5): the environment first (a caller's explicit
//! per-run override), then spira.toml through the spira-config library, then the default.
//! testenv never parses spira.toml or the repo-map itself.

use spira_config::SpiraToml;
use std::path::{Path, PathBuf};

pub struct Source<'a> {
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub config: Option<&'a SpiraToml>,
}

impl Source<'_> {
    /// Non-empty environment value, else the config path's value.
    pub fn get(&self, env: &str, config_path: Option<&str>) -> Option<String> {
        if let Some(v) = (self.env)(env).filter(|v| !v.is_empty()) {
            return Some(v);
        }
        let doc = self.config?;
        spira_config::get_path(doc, config_path?).filter(|v| !v.is_empty())
    }

    /// Set at all in the environment, even empty (SPIRA_BATCH_SKIP_INSTALL semantics differ).
    pub fn env_raw(&self, env: &str) -> Option<String> {
        (self.env)(env)
    }

    fn num<T: std::str::FromStr>(&self, env: &str, cfg: Option<&str>) -> Option<T> {
        self.get(env, cfg).and_then(|v| v.trim().parse().ok())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub run: PathBuf,
    pub results_root: PathBuf,
    pub verdicts: PathBuf,
    pub verdict_ttl: u64,
    pub repeat_reason: Option<String>,
    /// 0 disables.
    pub suite_timeout: u64,
    pub maxpar_requested: Option<i64>,
    /// Set when SPIRA_BATCH_MAXPAR (or `spira.batch_maxpar`) was given but is not usable:
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

/// SPIRA_RUN as conf.sh derives it when neither the environment nor the config sets it.
pub fn derive_run(repo: &Path, instance: &str, env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let sfx = if instance.is_empty() || instance == "prod" {
        String::new()
    } else {
        format!("-{instance}")
    };
    let writable = std::fs::metadata(repo)
        .map(|m| !m.permissions().readonly())
        .unwrap_or(false);
    if writable {
        return repo.join(".runtime").join(format!("spira{sfx}"));
    }
    let data = env("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env("HOME").unwrap_or_default()).join(".local/share"));
    data.join(format!("spira{sfx}")).join("run")
}

/// SPIRA_BATCH_MAXPAR / `spira.batch_maxpar`: absent is `(None, None)` — the scheduler's own
/// bounded, host-derived default applies (`schedule::maxpar`). Present and a positive whole
/// number is `(Some(n), None)` — a request, clamped to the hardware bound. Present and
/// anything else (zero, negative, or not a number) is `(None, Some(reason))` — refused by
/// name, never folded into "unset" or "unlimited" (sp-tj8k3: a batch_maxpar of 0 in
/// production read as unlimited concurrency and put 61 containers on one host).
fn resolve_maxpar(src: &Source) -> (Option<i64>, Option<String>) {
    match src.get("SPIRA_BATCH_MAXPAR", Some("spira.batch_maxpar")) {
        None => (None, None),
        Some(raw) => match raw.trim().parse::<i64>() {
            Ok(n) if n > 0 => (Some(n), None),
            Ok(n) => (
                None,
                Some(format!(
                    "SPIRA_BATCH_MAXPAR={n} is refused — 0 no longer means unlimited; omit the key for the scheduler's bounded default, or set a positive suite count"
                )),
            ),
            Err(_) => (
                None,
                Some(format!("SPIRA_BATCH_MAXPAR={raw:?} is not a whole number")),
            ),
        },
    }
}

fn instance_of(src: &Source) -> String {
    src.get("SPIRA_INSTANCE", Some("spira.instance"))
        .unwrap_or_else(|| "prod".into())
}

/// SPIRA_RUN: the environment, then `spira.run`, then derived from SPIRA_REPO (else
/// `harness_repo`) as conf.sh derives it.
pub fn resolve_run(src: &Source, harness_repo: &Path) -> PathBuf {
    src.get("SPIRA_RUN", Some("spira.run"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let repo = src
                .get("SPIRA_REPO", None)
                .map(PathBuf::from)
                .unwrap_or_else(|| harness_repo.to_path_buf());
            derive_run(&repo, &instance_of(src), src.env)
        })
}

impl Settings {
    /// `harness_repo` is where SPIRA_RUN is derived from when nothing sets it (conf.sh's
    /// SPIRA_REPO); `now` names a local run.
    pub fn load(src: &Source, harness_repo: &Path, now: u64) -> Settings {
        let run = resolve_run(src, harness_repo);
        let path = |env: &str| src.get(env, None).map(PathBuf::from);
        let (maxpar_requested, maxpar_refusal) = resolve_maxpar(src);
        Settings {
            results_root: path("SPIRA_BATCH_RESULTS").unwrap_or_else(|| run.join("batch-results")),
            verdicts: path("SPIRA_VERDICTS").unwrap_or_else(|| run.join("verdicts")),
            verdict_ttl: src
                .num("SPIRA_VERDICT_TTL", Some("spira.verdict_ttl"))
                .unwrap_or(86_400),
            repeat_reason: src.get("SPIRA_VERDICT_REPEAT_CONSIDERED", None),
            suite_timeout: src
                .num("SPIRA_SUITE_TIMEOUT", Some("spira.suite_timeout"))
                .unwrap_or(600),
            maxpar_requested,
            maxpar_refusal,
            maxpar_ceiling: src.num(
                "SPIRA_BATCH_MAXPAR_CEILING",
                Some("spira.batch_maxpar_ceiling"),
            ),
            mem_reserve_mib: src
                .num(
                    "SPIRA_BATCH_MEM_RESERVE_MIB",
                    Some("spira.batch_mem_reserve_mib"),
                )
                .unwrap_or(1024),
            mem_per_suite_mib: src
                .num(
                    "SPIRA_BATCH_MEM_PER_SUITE_MIB",
                    Some("spira.batch_mem_per_suite_mib"),
                )
                .unwrap_or(192),
            mem_avail_mib: src.num(
                "SPIRA_BATCH_MEM_AVAIL_MIB",
                Some("spira.batch_mem_avail_mib"),
            ),
            psi_threshold: src
                .num(
                    "SPIRA_BATCH_PSI_THRESHOLD",
                    Some("spira.batch_psi_threshold"),
                )
                .unwrap_or(10.0),
            orphan_min_age: src
                .num(
                    "SPIRA_BATCH_ORPHAN_MIN_AGE",
                    Some("spira.batch_orphan_min_age"),
                )
                .unwrap_or(3600),
            orphan_prefix: src
                .get("SPIRA_BATCH_ORPHAN_PREFIX", None)
                .unwrap_or_else(|| "spira-batch-".into()),
            peak_warn_frac: src
                .num(
                    "SPIRA_BATCH_PEAK_WARN_FRAC",
                    Some("spira.batch_peak_warn_frac"),
                )
                .unwrap_or(60),
            exec_fault_threshold: src
                .num("SPIRA_BATCH_EXEC_FAULT_THRESHOLD", None)
                .unwrap_or(5)
                .max(1),
            liveness_retries: src
                .num("SPIRA_BATCH_LIVENESS_RETRIES", None)
                .unwrap_or(3)
                .max(1),
            liveness_sleep: src.num("SPIRA_BATCH_LIVENESS_SLEEP", None).unwrap_or(3),
            instance: src.get("SPIRA_BATCH_INSTANCE", None),
            suite_dir: path("SPIRA_BATCH_SUITE_DIR"),
            skip_install: src
                .env_raw("SPIRA_BATCH_SKIP_INSTALL")
                .is_some_and(|v| !v.is_empty()),
            tiers: src
                .get("SPIRA_BATCH_TIERS", None)
                .unwrap_or_else(|| "T2,T3".into()),
            mail_cmd: path("SPIRA_BATCH_MAIL_CMD"),
            incident_cmd: path("SPIRA_BATCH_INCIDENT_CMD"),
            suite_state_file: src
                .get("SPIRA_SUITE_STATE_FILE", None)
                .unwrap_or_else(|| "spira/suite-state".into()),
            skip_allowlist_file: src
                .get("SPIRA_SKIP_ALLOWLIST_FILE", None)
                .unwrap_or_else(|| "spira/skip-allowlist.tsv".into()),
            select_head: src.get("SPIRA_GATE_SELECT_HEAD", None),
            round_batch_id: src.get("SPIRA_ROUND_BATCH_ID", None),
            round_members: src.num("SPIRA_ROUND_MEMBERS", None).unwrap_or(0),
            run_id: src
                .get("GITHUB_RUN_ID", None)
                .or_else(|| src.get("SPIRA_BATCH_RUN_ID", None))
                .unwrap_or_else(|| format!("local-{now}")),
            scratch_slots: src.num("SPIRA_TESTENV_SCRATCH_SLOTS", None).unwrap_or(4),
            scratch_min_free_mib: src
                .num("SPIRA_TESTENV_SCRATCH_MIN_FREE_MIB", None)
                .unwrap_or(4096),
            scratch_min_mem_mib: src
                .num("SPIRA_TESTENV_SCRATCH_MIN_MEM_MIB", None)
                .unwrap_or(8192),
            warm_slots: src.num("SPIRA_TESTENV_WARM_SLOTS", None).unwrap_or(3),
            setup_share: src
                .num("SPIRA_TESTENV_SETUP_SHARE", None)
                .unwrap_or(50u64)
                .clamp(10, 90),
            warm_boot_timeout: src
                .num("SPIRA_TESTENV_WARM_BOOT_TIMEOUT", None)
                .unwrap_or(600u64)
                .max(1),
            warm_shed_free_mib: src.num("SPIRA_TMPFS_SHED_FREE_MIB", None).unwrap_or(6144),
            landing_containers: path("SPIRA_LANDING_CONTAINERS"),
            spira_db: src.get("SPIRA_DB", Some("spira.db")),
            run,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load(env: &[(&str, &str)], cfg: Option<&SpiraToml>) -> Settings {
        let m: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let f = move |k: &str| m.get(k).cloned();
        let src = Source {
            env: &f,
            config: cfg,
        };
        Settings::load(&src, Path::new("/nonexistent-harness"), 1000)
    }

    #[test]
    fn defaults_follow_testenv_batch() {
        let s = load(&[("SPIRA_RUN", "/r"), ("HOME", "/h")], None);
        assert_eq!(s.results_root, PathBuf::from("/r/batch-results"));
        assert_eq!(s.verdicts, PathBuf::from("/r/verdicts"));
        assert_eq!((s.verdict_ttl, s.suite_timeout), (86_400, 600));
        assert_eq!(
            (s.mem_reserve_mib, s.mem_per_suite_mib, s.peak_warn_frac),
            (1024, 192, 60)
        );
        assert_eq!(s.orphan_prefix, "spira-batch-");
        assert_eq!(s.tiers, "T2,T3");
        assert_eq!(s.run_id, "local-1000");
        assert!(!s.skip_install);
        assert_eq!(s.suite_state_file, "spira/suite-state");
        assert_eq!(s.skip_allowlist_file, "spira/skip-allowlist.tsv");
        assert_eq!(
            (s.warm_slots, s.setup_share, s.warm_boot_timeout),
            (3, 50, 600)
        );
        assert_eq!(s.warm_shed_free_mib, 6144);
    }

    #[test]
    fn warm_shed_free_mib_is_overridable() {
        let s = load(
            &[("SPIRA_RUN", "/r"), ("SPIRA_TMPFS_SHED_FREE_MIB", "9000")],
            None,
        );
        assert_eq!(s.warm_shed_free_mib, 9000);
    }

    #[test]
    fn the_setup_share_is_clamped_so_the_suites_always_get_some_budget() {
        let share = |v: &str| {
            load(
                &[("SPIRA_RUN", "/r"), ("SPIRA_TESTENV_SETUP_SHARE", v)],
                None,
            )
            .setup_share
        };
        assert_eq!((share("100"), share("0"), share("70")), (90, 10, 70));
    }

    #[test]
    fn env_beats_config_and_config_beats_default() {
        let doc = spira_config::validate(
            "[spira]\nsuite_timeout = \"900\"\nbatch_maxpar = 12\nverdict_ttl = 60\n",
        )
        .unwrap();
        let s = load(&[("SPIRA_RUN", "/r")], Some(&doc));
        assert_eq!(s.suite_timeout, 900);
        assert_eq!(s.maxpar_requested, Some(12));
        assert_eq!(s.verdict_ttl, 60);
        let s = load(
            &[
                ("SPIRA_RUN", "/r"),
                ("SPIRA_BATCH_MAXPAR", "16"),
                ("SPIRA_SUITE_TIMEOUT", "0"),
            ],
            Some(&doc),
        );
        assert_eq!(s.maxpar_requested, Some(16));
        assert_eq!(s.suite_timeout, 0);
    }

    /// sp-tj8k3: unset must stay unset (no refusal, no override) — only a *given* bad value
    /// refuses. Zero and a non-numeric value both refuse, by name; a positive value never
    /// does.
    #[test]
    fn maxpar_unset_is_silent_zero_and_garbage_refuse() {
        let s = load(&[("SPIRA_RUN", "/r")], None);
        assert_eq!((s.maxpar_requested, s.maxpar_refusal), (None, None));

        let s = load(&[("SPIRA_RUN", "/r"), ("SPIRA_BATCH_MAXPAR", "0")], None);
        assert_eq!(s.maxpar_requested, None);
        assert!(
            s.maxpar_refusal.as_deref().is_some_and(|r| r.contains("0")),
            "{:?}",
            s.maxpar_refusal
        );

        let s = load(
            &[("SPIRA_RUN", "/r"), ("SPIRA_BATCH_MAXPAR", "lots")],
            None,
        );
        assert_eq!(s.maxpar_requested, None);
        assert!(
            s.maxpar_refusal
                .as_deref()
                .is_some_and(|r| r.contains("lots")),
            "{:?}",
            s.maxpar_refusal
        );

        let s = load(&[("SPIRA_RUN", "/r"), ("SPIRA_BATCH_MAXPAR", "4")], None);
        assert_eq!((s.maxpar_requested, s.maxpar_refusal), (Some(4), None));
    }

    #[test]
    fn run_id_prefers_github_then_explicit() {
        assert_eq!(
            load(
                &[
                    ("SPIRA_RUN", "/r"),
                    ("GITHUB_RUN_ID", "77"),
                    ("SPIRA_BATCH_RUN_ID", "x")
                ],
                None
            )
            .run_id,
            "77"
        );
        assert_eq!(
            load(&[("SPIRA_RUN", "/r"), ("SPIRA_BATCH_RUN_ID", "x")], None).run_id,
            "x"
        );
    }

    #[test]
    fn run_derivation_uses_xdg_for_an_unwritable_repo_and_suffixes_instances() {
        let s = load(&[("XDG_DATA_HOME", "/x"), ("SPIRA_INSTANCE", "t1")], None);
        assert_eq!(s.run, PathBuf::from("/x/spira-t1/run"));
    }

    #[test]
    fn skip_install_needs_a_non_empty_value() {
        assert!(
            load(
                &[("SPIRA_RUN", "/r"), ("SPIRA_BATCH_SKIP_INSTALL", "1")],
                None
            )
            .skip_install
        );
        assert!(
            !load(
                &[("SPIRA_RUN", "/r"), ("SPIRA_BATCH_SKIP_INSTALL", "")],
                None
            )
            .skip_install
        );
    }
}
