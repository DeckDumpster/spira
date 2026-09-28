//! spira-config — the typed schema behind `spira.toml`.
//!
//! One document, three kinds of table: `[spira]` (host-wide keys), `[repo.<name>]` (a
//! checkout the harness may work in) and `[persona.<name>]` (an aeon's fayth, cut down to
//! the fields the harness itself reads rather than a persona's prose). Every struct here
//! carries `deny_unknown_fields`, so a typo is a hard error instead of a setting nobody is
//! reading — the same failure mode `spira.conf`'s own allowlist exists to catch, now
//! enforced by the type system instead of a hand-maintained string.
//!
//! `[spira]` covers the keys a real install actually sets, not every key `conf.sh` allows —
//! conf.sh's own allowlist runs past 200, most of them derived defaults nothing overrides.
//! Widening this table is scoped to the cutover bead, where each consumer's own reads say
//! which of the rest still need a home.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod convert;

/// The root of `spira.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpiraToml {
    pub spira: Option<SpiraSection>,
    #[serde(default)]
    pub repo: BTreeMap<String, RepoSection>,
    #[serde(default)]
    pub persona: BTreeMap<String, PersonaSection>,
}

/// on/off, spelled as an enum rather than a bool so a TOML reader sees the word `spira.conf`
/// already used (`SPIRA_CERTIFY_SUITES = off`) instead of learning a second spelling for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OnOff {
    On,
    Off,
}

/// A czar stage: shadow investigates and writes CZAR-WOULD notes; act permits mutation.
/// One representative key (`czar_stage_deadlock`) ships here — the other six
/// `SPIRA_CZAR_STAGE_*` classes are the same enum and widen with the rest of `[spira]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CzarStage {
    Shadow,
    Act,
}

/// `[spira]` — host-wide keys. Every field is optional: `conf.sh` derives a default for
/// each of these from where the harness is installed, and a clean clone sets none of them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpiraSection {
    pub home_repo: Option<String>,
    pub db: Option<String>,
    pub run: Option<String>,
    pub goal: Option<String>,
    pub path: Option<String>,
    pub workspaces: Option<String>,
    pub prod: Option<String>,
    pub instance: Option<String>,
    pub dolt_data: Option<String>,
    pub wiki: Option<String>,
    pub wiki_hook: Option<String>,
    pub view: Option<String>,
    pub tz: Option<String>,
    pub operator: Option<String>,
    pub operator_actor: Option<String>,
    pub ask_label: Option<String>,
    pub ci_label: Option<String>,
    pub scope_label: Option<String>,
    pub actionable: Option<String>,
    pub notify_age: Option<u64>,
    pub verdict_ttl: Option<u64>,
    pub max_aeons: Option<u32>,
    pub max_live_aeons: Option<u32>,
    #[serde(default)]
    pub fayths: Vec<String>,
    pub batch_maxpar: Option<u32>,
    pub certify_par: Option<u32>,
    pub certify_suites: Option<OnOff>,
    pub cert_idle_skip: Option<bool>,
    pub queue_batch_max: Option<u32>,
    pub queue_batch_wait: Option<u64>,
    pub queue_ci_maxsec: Option<u64>,
    pub queue_throttle_release_at: Option<u32>,
    pub suites_budget: Option<u64>,
    pub loom_addr: Option<String>,
    pub loom_budget_ms: Option<u64>,
    pub mail_readers: Option<String>,
    pub gh_intake_repo: Option<String>,
    pub cockpit_bottom_pct: Option<u32>,
    pub cockpit_right_pct: Option<u32>,
    pub czar_stage_deadlock: Option<CzarStage>,
    pub bd: Option<String>,
    pub watchers: Option<String>,
    pub watchers_overlay: Option<String>,
    pub cockpit: Option<String>,
    pub batch_mem_per_suite_mib: Option<u32>,
    pub lanes_max_live: Option<u32>,
    pub thrash_minutes: Option<u32>,
    pub summon_lock_wait: Option<u32>,
    pub concierge_inbox: Option<String>,
    pub concierge_inbox_dedup: Option<u32>,
    pub concierge_inbox_stall: Option<u32>,
    pub concierge_inbox_backoff: Option<u32>,
    pub mail_settle: Option<u64>,
    pub cockpit_clipboard: Option<String>,
    pub lc_bin: Option<String>,
    pub queue_throttle_override: Option<String>,
    pub client_settings: Option<String>,
    pub mail: Option<String>,
    pub mail_session_mailbox: Option<String>,
    pub repo_map: Option<String>,
    pub prefix_map: Option<String>,
    pub chamber: Option<String>,
    pub chamber_overlay: Option<String>,
    pub overrides: Option<String>,
    pub id_prefix: Option<String>,
    pub health_timeout: Option<String>,
    pub wake: Option<String>,
    pub ctrl: Option<String>,
    pub mail_kinds: Option<String>,
    pub mail_unread_age: Option<String>,
    pub mail_repeat_window: Option<String>,
    pub mail_tidy_fresh: Option<String>,
    pub mail_wake_backoff: Option<String>,
    pub mail_index: Option<String>,
    pub cockpit_trace_lines: Option<String>,
    pub snap_stale_s: Option<String>,
    pub notify: Option<String>,
    pub panel: Option<String>,
    pub verify_timeout: Option<String>,
    pub reclaim_skip_label: Option<String>,
    pub operated: Option<String>,
    pub ci_park_max: Option<String>,
    pub world_stop_label: Option<String>,
    pub land_maxsec: Option<String>,
    pub land_gate_reserve: Option<String>,
    pub certify_always_covers: Option<String>,
    pub rebase_escalate_at: Option<String>,
    pub eviction_escalate_at: Option<String>,
    pub verdict_window: Option<String>,
    pub check5_max_file: Option<String>,
    pub check5_max_resolve: Option<String>,
    pub remedy_window: Option<String>,
    pub pr_stall_mins: Option<String>,
    pub deferral_escalate_at: Option<String>,
    pub broker_bin: Option<String>,
    pub broker_enable: Option<String>,
    pub broker_gh_config_dir: Option<String>,
    pub broker_gh_token: Option<String>,
    pub czar_pass_bin: Option<String>,
    pub queue_watch_bin: Option<String>,
    pub supervise_bin: Option<String>,
    pub landing_pass_bin: Option<String>,
    pub tsd_bin: Option<String>,
    pub reconciler_bin: Option<String>,
    pub test_plan_bin: Option<String>,
    pub reconciler_flow_bin: Option<String>,
    pub loom_cache_s: Option<String>,
    pub loom_bin: Option<String>,
    pub loom_ready_grace: Option<String>,
    pub flow_window_hours: Option<String>,
    pub flow_baseline_hours: Option<String>,
    pub flow_grace_secs: Option<String>,
    pub flow_unobservable_grace_secs: Option<String>,
    pub desired_dir: Option<String>,
    pub gh: Option<String>,
    pub gh_app_config: Option<String>,
    pub gh_ask_grace_secs: Option<String>,
    pub spike_label: Option<String>,
    pub spike_dir: Option<String>,
    pub spike_paths: Option<String>,
    pub groomer_label: Option<String>,
    pub groom_threshold: Option<String>,
    pub groom_ask_label: Option<String>,
    pub plan_label: Option<String>,
    pub incident_label: Option<String>,
    pub czar_label: Option<String>,
    pub no_loop_label: Option<String>,
    pub express_label: Option<String>,
    pub reconciler_label: Option<String>,
    pub reconciler_grace_secs: Option<String>,
    pub disk_floor_pct: Option<String>,
    pub czar_stage_attribution_failed: Option<String>,
    pub czar_stage_sort_failed: Option<String>,
    pub czar_stage_loop_stalled: Option<String>,
    pub czar_stage_ci_stalled: Option<String>,
    pub czar_stage_starved: Option<String>,
    pub czar_stage_ci_red: Option<String>,
    pub czar_stage_base_red: Option<String>,
    pub maechen_label: Option<String>,
    pub maechen_landing_interval: Option<String>,
    pub maechen_max_gap_seconds: Option<String>,
    pub maechen_max_beads: Option<String>,
    pub maechen_remedy_label: Option<String>,
    pub census_clock_skew_tolerance_s: Option<String>,
    pub cockpit_db: Option<String>,
    pub cockpit_mail: Option<String>,
    pub cockpit_mouse: Option<String>,
    pub cockpit_cwd: Option<String>,
    pub cockpit_sessions: Option<String>,
    pub cockpit_host: Option<String>,
    pub town: Option<String>,
    pub mirror: Option<String>,
    pub exporter: Option<String>,
    pub design: Option<String>,
    pub view_session: Option<String>,
    pub alert_glob: Option<String>,
    pub bd_pin: Option<String>,
    pub bd_tag: Option<String>,
    pub lanes: Option<String>,
    pub qa_depth: Option<String>,
    pub thrash_streak_cap: Option<String>,
    pub rapid_recur_threshold: Option<String>,
    pub brief_keep_recurrences: Option<String>,
    pub brief_notes_max_chars: Option<String>,
    pub claim_retries: Option<String>,
    pub claim_retry_delay_s: Option<String>,
    pub token_window_h: Option<String>,
    pub token_projects: Option<String>,
    pub ctx_warn: Option<String>,
    pub ctx_high: Option<String>,
    pub ctx_limit: Option<String>,
    pub archive: Option<String>,
    pub archivist_every: Option<String>,
    pub archivist_idle: Option<String>,
    pub archivist_model: Option<String>,
    pub archivist_timeout: Option<String>,
    pub archivist_per_pass: Option<String>,
    pub archivist_timeout_retries: Option<String>,
    pub testdb_lib: Option<String>,
    pub testdb_bd: Option<String>,
    pub testdb_data: Option<String>,
    pub testdb_port: Option<String>,
    pub testenv_registry: Option<String>,
    pub testenv_max_concurrent: Option<String>,
    pub testenv_queue_timeout: Option<String>,
    pub testenv_queue_poll: Option<String>,
    pub gh_intake_priority: Option<String>,
    pub gh_intake_bead_repo: Option<String>,
    pub flaky_gh_repo: Option<String>,
    pub release_repo: Option<String>,
    pub release_rust_toolchain: Option<String>,
    pub gate_timeout: Option<String>,
    pub gate_budget: Option<String>,
    pub gate_select_cap: Option<String>,
    pub aeon_cpu_quota: Option<String>,
    pub cutover_round_label: Option<String>,
    pub gate_lock_wait: Option<String>,
    pub land_cpu_quota: Option<String>,
    pub gate_suites: Option<String>,
    pub suite_state_file: Option<String>,
    pub suites_state: Option<String>,
    pub suite_timeout: Option<String>,
    pub tier_budget_t0_ms: Option<String>,
    pub tier_budget_t1_ms: Option<String>,
    pub tier_budget_t2_ms: Option<String>,
    pub tier_budget_t3_ms: Option<String>,
    pub tier_budget_window: Option<String>,
    pub tier_allowlist_margin_pct: Option<String>,
    pub tier_allowlist: Option<String>,
    pub tier_area_allowlist: Option<String>,
    pub batch_maxpar_ceiling: Option<String>,
    pub batch_mem_reserve_mib: Option<String>,
    pub batch_mem_avail_mib: Option<String>,
    pub batch_psi_threshold: Option<String>,
    pub batch_orphan_min_age: Option<String>,
    pub batch_bins_ttl: Option<String>,
    pub attribute_maxpar: Option<String>,
    pub batch_peak_warn_frac: Option<String>,
    pub batch_artifact_days: Option<String>,
    pub batch_tail_lines: Option<String>,
    pub suite_times_log: Option<String>,
    pub batch_ledger: Option<String>,
    pub suites_priority: Option<String>,
    pub suites_stale: Option<String>,
    pub self_test: Option<String>,
    pub incident_priority: Option<String>,
    pub watcher_interval_s: Option<String>,
    pub flake_quarantine_at: Option<String>,
    pub flake_window: Option<String>,
    pub quarantine_clean_runs: Option<String>,
    pub quarantine_max_age: Option<String>,
    pub queue_ci_idle_sec: Option<String>,
    pub ci_queued_max_secs: Option<String>,
    pub loop_stall_secs: Option<String>,
    pub ci_red_max_secs: Option<String>,
    pub base_ci_unreadable_grace_secs: Option<String>,
    pub preflight_wall_secs: Option<String>,
    pub preflight_suite_max_secs: Option<String>,
    pub queue_infra_retries: Option<String>,
    pub queue_stuck_age: Option<String>,
    pub queue_dir: Option<String>,
    pub forge: Option<String>,
    pub forge_repo: Option<String>,
    pub queue_wait_label: Option<String>,
    pub queue_actions_app_id: Option<String>,
    pub queue_batcher: Option<String>,
    pub batcher_bin: Option<String>,
    pub batch_judgement_label: Option<String>,
    pub queue_lock_wait: Option<String>,
    pub queue_lock_starve_max: Option<String>,
    pub queue_repro_ci_pollsec: Option<String>,
    pub queue_repro_ci_maxsec: Option<String>,
    pub submitted_label: Option<String>,
    pub work_close_types: Option<String>,
    pub queue_throttle_depth_at: Option<String>,
    pub queue_throttle_stall_mins: Option<String>,
    pub auron_restarts: Option<String>,
    pub auron_restart_window: Option<String>,
    pub releases: Option<String>,
    pub releases_keep: Option<String>,
    pub gh_repo: Option<String>,
    pub reviewer_model: Option<String>,
    pub reviewer_verdicts: Option<String>,
    pub reviewer_timeout: Option<String>,
    pub reviewer_diff_limit: Option<String>,
    pub review_label: Option<String>,
    pub capacity_probe_model: Option<String>,
    pub capacity_probe_interval: Option<String>,
    pub capacity_probe_window: Option<String>,
    pub capacity_probe_timeout: Option<String>,
    pub self_window: Option<String>,
    pub delivers_check_timeout: Option<String>,
    pub cert_window_mins: Option<String>,
    pub agent: Option<String>,
    pub statute_core: Option<String>,
    pub git_name: Option<String>,
    pub git_email: Option<String>,
    pub gh_app_id: Option<String>,
    pub gh_app_installation_id: Option<String>,
    pub gh_app_key: Option<String>,
    pub gh_app_private_key: Option<String>,
    pub pve_env: Option<String>,
}

/// How a landed branch reaches its base — see `repo-map.example`'s own `land` column.
/// `Queue` and `QueueForge` are the same mode under two spellings — `queue` is the alias a
/// row has always been able to write, `queue.forge` names it explicitly now that
/// `QueueLocal` exists to contrast it with. `QueueLocal` lands on a local branch (`local/main`
/// in the `base` column) with no forge round trip on the critical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LandMode {
    Push,
    Pr,
    Hold,
    Queue,
    #[serde(rename = "queue.forge")]
    QueueForge,
    #[serde(rename = "queue.local")]
    QueueLocal,
}

/// A persona lane a repository admits — the explicit form of `repo-map.example`'s `lanes`
/// column. A row's shorthand mode (`consume`/`develop`/`self`) is expanded to this array by
/// the converter; the schema itself only ever sees the expanded list.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    Plan,
    Incident,
    Groom,
    MaechenSweep,
    Spike,
    CzarTrigger,
}

/// `[repo.<name>]` — a checkout the harness may work in. `path` and `mode` are the two
/// facts nothing can derive: where the checkout is, and what a green gate does with a
/// branch. Everything else has a sensible empty reading (no format, no explicit base to
/// resolve, no extra lanes).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoSection {
    pub path: String,
    pub mode: LandMode,
    pub base: Option<String>,
    pub format: Option<String>,
    pub gate: Option<String>,
    #[serde(default)]
    pub lanes: Vec<Lane>,
    /// Overrides the host's default forge script for this one repository. Absent means
    /// "use `[spira]`'s own", which is every repository today.
    pub forge: Option<String>,
}

/// `append` keeps Claude Code's own coding guidance underneath the persona layer;
/// `replace` is for a persona narrow enough that the default guidance would mislead it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SystemPromptMode {
    Append,
    Replace,
}

/// The claim TTL and heartbeat cadence a persona's lease runs on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub minutes: Option<u32>,
    pub heartbeat_seconds: Option<u32>,
}

/// `[persona.<name>]` — a fayth, cut down to what the harness itself reads to summon and
/// dispatch it. `model` is the one field with no sensible default: an aeon summoned under
/// no model is not a cheaper aeon, it is a dead one, so it is required.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PersonaSection {
    pub model: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub lane: Option<String>,
    #[serde(default)]
    pub lease: Option<Lease>,
    pub system_prompt: Option<SystemPromptMode>,
}

/// The JSON Schema for [`SpiraToml`], as shipped in `schema/spira.schema.json`. Generated
/// from the same types `validate` deserializes into, so the shipped schema and the actual
/// hard errors can never name different fields.
pub fn json_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(SpiraToml)
}

/// A `[spira]` key dropped from `SpiraSection` whose presence in a live config must not
/// break loading it. `validate` accepts these fields with a warning instead of the hard
/// "unknown field" error a genuine typo gets.
pub struct RetiredKey {
    pub key: &'static str,
    pub bead: &'static str,
}

/// Removing a field from `SpiraSection` requires adding it here, or
/// `retiring_a_key_requires_updating_the_history` (tests/validate.rs) fails: it diffs the
/// schema's current field set against `schema/spira-key-history.txt`, the all-time set, and
/// a key present in history but neither active nor listed here is reported as silently
/// dropped.
pub const RETIRED_SPIRA_KEYS: &[RetiredKey] = &[
    RetiredKey { key: "queue_local_gate", bead: "sp-vsob2" },
    RetiredKey { key: "queue_batch_idle_cut", bead: "sp-vsob2" },
    RetiredKey { key: "hook_lines", bead: "sp-o9nkc" },
    RetiredKey { key: "answer_state", bead: "sp-xsl8i" },
    RetiredKey { key: "answer_mark", bead: "sp-xsl8i" },
    RetiredKey { key: "answer_comment_mark", bead: "sp-xsl8i" },
    RetiredKey { key: "self_closed", bead: "sp-xsl8i" },
    RetiredKey { key: "wake_watchers", bead: "sp-xsl8i" },
];

/// Every key in `history` that is neither an active `[spira]` field (`active`) nor listed in
/// [`RETIRED_SPIRA_KEYS`] — a key the schema dropped without retiring it.
pub fn missing_from_retirement(
    history: &std::collections::BTreeSet<String>,
    active: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let retired: std::collections::BTreeSet<&str> =
        RETIRED_SPIRA_KEYS.iter().map(|k| k.key).collect();
    history
        .iter()
        .filter(|k| !active.contains(k.as_str()) && !retired.contains(k.as_str()))
        .cloned()
        .collect()
}

/// Parses `text` as `spira.toml` and reports the first error at the TOML path it occurred
/// on (`spira.max_aeons`, `repo.service.mode`, ...) rather than a bare line/column, so a
/// hard error names the thing to fix instead of the place the parser gave up. A
/// [`RETIRED_SPIRA_KEYS`] member under `[spira]` is dropped before deserializing rather than
/// refused: see [`validate_with_warnings`] for the warning that names it.
pub fn validate(text: &str) -> Result<SpiraToml, String> {
    validate_with_warnings(text).map(|(doc, _)| doc)
}

/// Like [`validate`], but returns one warning per [`RETIRED_SPIRA_KEYS`] member found under
/// `[spira]` — naming the key and the bead that retired it — instead of silently dropping
/// them. A key `SPIRA_CONF_KEYS`/`SpiraSection` never accepted still hard-errors: only
/// listed retirements are stripped before the deserialize that would otherwise refuse them.
pub fn validate_with_warnings(text: &str) -> Result<(SpiraToml, Vec<String>), String> {
    let mut root: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let mut warnings = Vec::new();
    if let Some(spira) = root.get_mut("spira").and_then(|v| v.as_table_mut()) {
        for retired in RETIRED_SPIRA_KEYS {
            if spira.remove(retired.key).is_some() {
                warnings.push(format!(
                    "{} is retired ({}) and ignored — remove it",
                    retired.key, retired.bead
                ));
            }
        }
    }
    let doc = serde_path_to_error::deserialize(root).map_err(|e| {
        let path = e.path().to_string();
        if path.is_empty() {
            e.inner().to_string()
        } else {
            format!("{path}: {}", e.inner())
        }
    })?;
    Ok((doc, warnings))
}

/// Whether `new` is a SHRINK of `existing` — fewer `[repo.*]` tables, or fewer
/// `[spira].fayths` entries — the two counts a converted `spira.toml` can lose without any
/// parse error to show for it, if it is regenerated from a narrower source (a repo-map
/// missing rows, a worktree with only some of the real `chamber/*.fayth` files) and written
/// over a fuller document already in force. `Some(reason)` names both counts for the
/// caller to report; `None` means `new` carries at least as much as `existing` in both
/// dimensions.
pub fn shrink_reason(existing: &SpiraToml, new: &SpiraToml) -> Option<String> {
    let existing_repos = existing.repo.len();
    let new_repos = new.repo.len();
    let existing_fayths = existing.spira.as_ref().map(|s| s.fayths.len()).unwrap_or(0);
    let new_fayths = new.spira.as_ref().map(|s| s.fayths.len()).unwrap_or(0);
    if new_repos < existing_repos || new_fayths < existing_fayths {
        Some(format!(
            "existing has {existing_repos} [repo.*] table(s) and {existing_fayths} fayth(s); \
             new document has {new_repos} and {new_fayths}"
        ))
    } else {
        None
    }
}

/// Writes `contents` to `path` atomically: a temp file beside it, then a rename. Without
/// this a reader racing the writer (`spira_toml_read`, another `validate`) can observe a
/// half-written document — truncated by a writer killed mid-write — as a parse error on a
/// file that was never actually invalid.
pub fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("spira-config-out");
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Reads one dotted path out of an already-validated document — `spira.max_aeons`,
/// `repo.service.mode`, `persona.builder.lease.minutes` — for `spira-config get`.
pub fn get_path(doc: &SpiraToml, path: &str) -> Option<String> {
    let value = serde_json::to_value(doc).ok()?;
    let mut cur = &value;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    match cur {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Renders `[spira]` as quoted `KEY=value` lines — `spira-config export --sh` — for the
/// bash callers this schema has not replaced yet. Only scalar and list fields have a bash
/// shape; tables (`repo`, `persona`) are not exported.
pub fn export_sh(doc: &SpiraToml) -> String {
    let Some(spira) = &doc.spira else {
        return String::new();
    };
    let value = serde_json::to_value(spira).unwrap_or(serde_json::Value::Null);
    let serde_json::Value::Object(map) = value else {
        return String::new();
    };
    let mut out = String::new();
    for (key, val) in map {
        let shell_val = match val {
            serde_json::Value::Null => continue,
            serde_json::Value::String(s) => s,
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Array(items) => {
                if items.is_empty() {
                    continue;
                }
                items
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| v.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            serde_json::Value::Object(_) => continue,
        };
        out.push_str(&key.to_uppercase());
        out.push('=');
        out.push_str(&shell_quote(&shell_val));
        out.push('\n');
    }
    out
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Sets one dotted path (`spira.prod`, `spira.max_live_aeons`) to a leaf JSON value on `doc`.
/// Shared by `set_path`'s two attempts, below.
fn set_path_leaf(
    doc: &SpiraToml,
    path: &str,
    leaf_value: serde_json::Value,
) -> Result<SpiraToml, String> {
    let segs: Vec<&str> = path.split('.').collect();
    let Some((leaf, parents)) = segs.split_last() else {
        return Err("set: empty path".to_string());
    };
    let mut json = serde_json::to_value(doc).map_err(|e| e.to_string())?;
    let mut cur = &mut json;
    for seg in parents {
        if !cur.is_object() {
            *cur = serde_json::Value::Object(Default::default());
        }
        cur = cur
            .as_object_mut()
            .expect("just made an object")
            .entry(seg.to_string())
            .or_insert_with(|| serde_json::Value::Object(Default::default()));
    }
    if !cur.is_object() {
        *cur = serde_json::Value::Object(Default::default());
    }
    cur.as_object_mut()
        .expect("just made an object")
        .insert(leaf.to_string(), leaf_value);
    serde_path_to_error::deserialize(json).map_err(|e| {
        let p = e.path().to_string();
        if p.is_empty() {
            e.inner().to_string()
        } else {
            format!("{p}: {}", e.inner())
        }
    })
}

/// Sets one dotted path (`spira.prod`, `spira.instance`, `spira.max_live_aeons`) to a value
/// given as a plain command-line string, for `spira-config set` — the writer `deploy.sh`,
/// `install.sh` and `conf.sh`'s own `spira_config_set` shell helper use once nothing writes
/// `spira.conf`'s `KEY=value` lines anymore. Goes through a JSON round-trip rather than a
/// hand-written per-field match arm, so a field this schema already knows needs no writer of
/// its own; `deny_unknown_fields` still refuses a path this schema does not carry, via the
/// same `serde` deserialize the rest of this crate validates through.
///
/// TRIES THE VALUE AS A STRING FIRST, because most fields here are paths and names, and a
/// literal like `"3"` must stay the string `"3"` when the field is one of those — only a
/// field the schema itself types as a number or bool (`max_live_aeons`, `cert_idle_skip`, ...)
/// ever takes the second attempt, which parses the same text as JSON and retries. Reports the
/// first (string) attempt's error when both fail, since "expected u32" names the fix; the
/// generic JSON-parse failure on a bare word does not.
pub fn set_path(doc: &SpiraToml, path: &str, value: &str) -> Result<SpiraToml, String> {
    let as_string_err = match set_path_leaf(doc, path, serde_json::Value::String(value.to_string()))
    {
        Ok(d) => return Ok(d),
        Err(e) => e,
    };
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) {
        if !parsed.is_string() {
            if let Ok(d) = set_path_leaf(doc, path, parsed) {
                return Ok(d);
            }
        }
    }
    Err(as_string_err)
}

/// Removes one dotted path's value from `doc`, for `spira-config unset` — the writer side of
/// retiring a key (`aeons.sh unset`'s fleet ceiling) without leaving an empty string behind.
/// A JSON null deserializes to `None` for every `Option<T>` field this schema has, and `toml`
/// omits a `None` field entirely on serialization, so the key simply stops appearing in the
/// document — never a leftover `key = ""` a reader would have to know means "unset".
pub fn unset_path(doc: &SpiraToml, path: &str) -> Result<SpiraToml, String> {
    set_path_leaf(doc, path, serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_minimal_document() {
        let doc = validate("[spira]\nmax_aeons = 4\n").expect("valid");
        assert_eq!(doc.spira.unwrap().max_aeons, Some(4));
    }

    #[test]
    fn unknown_key_names_its_path() {
        let err = validate("[spira]\nbogus = 1\n").unwrap_err();
        assert!(err.starts_with("spira.bogus"), "{err}");
    }

    #[test]
    fn wrong_type_names_its_path() {
        let err = validate("[spira]\nmax_aeons = \"four\"\n").unwrap_err();
        assert!(err.starts_with("spira.max_aeons"), "{err}");
    }

    #[test]
    fn bad_enum_names_its_path() {
        let err = validate("[spira]\nczar_stage_deadlock = \"sometimes\"\n").unwrap_err();
        assert!(err.starts_with("spira.czar_stage_deadlock"), "{err}");
    }

    #[test]
    fn missing_required_repo_field() {
        let err = validate("[repo.home]\nmode = \"push\"\n").unwrap_err();
        assert!(err.starts_with("repo.home"), "{err}");
    }

    #[test]
    fn missing_required_persona_field() {
        let err = validate("[persona.builder]\ntools = [\"Bash\"]\n").unwrap_err();
        assert!(err.starts_with("persona.builder"), "{err}");
    }

    #[test]
    fn the_inline_comment_scar_is_refused_unquoted() {
        // sp-upkae's own scar: an inline `#` comment after a bare, unquoted value used to
        // be silently absorbed into that value by conf.sh's KEY=value reader. TOML has no
        // such ambiguity to inherit: a bare word is not a legal value at all, so the same
        // line is a hard parse error here rather than a value nobody refused.
        let err = validate("[spira]\ndb = /home/x # a trailing comment\n").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn the_inline_comment_scar_is_harmless_quoted() {
        let doc = validate("[spira]\ndb = \"/home/x\" # a trailing comment\n").expect("valid");
        assert_eq!(doc.spira.unwrap().db, Some("/home/x".to_string()));
    }

    #[test]
    fn get_path_reads_nested_tables() {
        let doc =
            validate("[repo.home]\npath = \"/srv/checkouts/home\"\nmode = \"push\"\n").unwrap();
        assert_eq!(get_path(&doc, "repo.home.mode"), Some("push".to_string()));
        assert_eq!(get_path(&doc, "repo.home.base"), None);
    }

    #[test]
    fn export_sh_quotes_and_uppercases() {
        let doc = validate("[spira]\nhome_repo = \"a b\"\nmax_aeons = 4\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("HOME_REPO='a b'\n"), "{out}");
        assert!(out.contains("MAX_AEONS='4'\n"), "{out}");
    }

    #[test]
    fn set_path_on_empty_document_creates_the_table() {
        let doc = set_path(&SpiraToml::default(), "spira.prod", "/srv/x").unwrap();
        assert_eq!(doc.spira.unwrap().prod, Some("/srv/x".to_string()));
    }

    #[test]
    fn set_path_replaces_without_disturbing_siblings() {
        let doc = validate("[spira]\nprod = \"/old\"\nmax_aeons = 4\n").unwrap();
        let doc = set_path(&doc, "spira.prod", "/new").unwrap();
        let spira = doc.spira.unwrap();
        assert_eq!(spira.prod, Some("/new".to_string()));
        assert_eq!(spira.max_aeons, Some(4));
    }

    #[test]
    fn set_path_refuses_an_unknown_field() {
        let err = set_path(&SpiraToml::default(), "spira.bogus", "x").unwrap_err();
        assert!(err.starts_with("spira.bogus"), "{err}");
    }

    #[test]
    fn a_retired_key_warns_instead_of_erroring() {
        let (doc, warnings) = validate_with_warnings("[spira]\nqueue_local_gate = 1\n")
            .expect("a retired key must not be a hard error");
        assert!(doc.spira.is_some());
        assert!(
            warnings.iter().any(|w| w.contains("queue_local_gate") && w.contains("sp-vsob2")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_misspelt_key_still_errors() {
        // Positive control for a_retired_key_warns_instead_of_erroring: a key that looks
        // like a retired one but isn't must still be refused, not silently accepted.
        let err = validate("[spira]\nqueue_local_gatee = 1\n").unwrap_err();
        assert!(err.starts_with("spira.queue_local_gatee"), "{err}");
    }

    #[test]
    fn missing_from_retirement_catches_a_dropped_key() {
        use std::collections::BTreeSet;
        let active: BTreeSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let mut history = active.clone();
        history.insert("queue_local_gate".to_string());
        // Dropped, but listed in RETIRED_SPIRA_KEYS — not an offender.
        assert!(missing_from_retirement(&history, &active).is_empty());

        // Planted offender: dropped from `active` and never retired.
        history.insert("never_retired".to_string());
        assert_eq!(
            missing_from_retirement(&history, &active),
            vec!["never_retired".to_string()]
        );
    }

    #[test]
    fn set_path_coerces_a_numeric_field() {
        let doc = set_path(&SpiraToml::default(), "spira.max_live_aeons", "3").unwrap();
        assert_eq!(doc.spira.unwrap().max_live_aeons, Some(3));
    }

    #[test]
    fn set_path_coerces_a_bool_field() {
        let doc = set_path(&SpiraToml::default(), "spira.cert_idle_skip", "true").unwrap();
        assert_eq!(doc.spira.unwrap().cert_idle_skip, Some(true));
    }

    #[test]
    fn set_path_keeps_a_numeric_looking_string_field_a_string() {
        // `instance` is a string field; a value that happens to parse as JSON must not be
        // coerced away from the string the schema actually wants.
        let doc = set_path(&SpiraToml::default(), "spira.instance", "123").unwrap();
        assert_eq!(doc.spira.unwrap().instance, Some("123".to_string()));
    }

    #[test]
    fn unset_path_removes_the_key_and_keeps_siblings() {
        let doc = validate("[spira]\nmax_live_aeons = 3\nmax_aeons = 4\n").unwrap();
        let doc = unset_path(&doc, "spira.max_live_aeons").unwrap();
        // omitted entirely from the serialized document, not written as an empty value.
        let out = toml::to_string_pretty(&doc).unwrap();
        assert!(!out.contains("max_live_aeons"), "{out}");
        let spira = doc.spira.unwrap();
        assert_eq!(spira.max_live_aeons, None);
        assert_eq!(spira.max_aeons, Some(4));
    }
}
