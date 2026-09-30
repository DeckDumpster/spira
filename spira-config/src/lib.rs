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
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod build;
pub mod convert;
pub mod legacy_map;

/// The one filename this schema's document is ever named on disk — every path-resolution
/// function below builds on this instead of a caller spelling `"spira.toml"` itself.
pub const FILE_NAME: &str = "spira.toml";

/// `dir/spira.toml`, if it is a file — the check a per-checkout caller (`spira-lc`'s
/// migration detector) uses instead of naming the file itself.
pub fn find_under(dir: &Path) -> Option<PathBuf> {
    let p = dir.join(FILE_NAME);
    p.is_file().then_some(p)
}

/// The search a host-wide reader with no explicit path resolves one from: `explicit` if
/// given (a caller's own `--config`/`$SPIRA_TOML` precedence), else `$SPIRA_TOML`, else
/// `$SPIRA_REPO/spira.toml`, else `$XDG_CONFIG_HOME/spira/spira.toml` (`$HOME/.config` when
/// `XDG_CONFIG_HOME` is unset), else `/etc/spira/spira.toml` — first of these that exists.
/// The one search order every host-wide reader (`queue-watch`) shares, so two daemons can
/// never disagree about which file is in force on the same host.
pub fn discover(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if explicit.is_some() {
        return explicit;
    }
    if let Ok(p) = std::env::var("SPIRA_TOML") {
        return Some(PathBuf::from(p));
    }
    let mut cands = Vec::new();
    if let Ok(r) = std::env::var("SPIRA_REPO") {
        cands.push(PathBuf::from(r).join(FILE_NAME));
    }
    let xdg = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Ok(x) = xdg {
        cands.push(x.join("spira").join(FILE_NAME));
    }
    cands.push(PathBuf::from("/etc/spira").join(FILE_NAME));
    cands.into_iter().find(|p| p.is_file())
}

/// Reads and [`validate`]s the document at `path` — the one place a caller turns a resolved
/// path into a [`SpiraToml`], instead of pairing its own `fs::read_to_string` with `validate`.
pub fn load(path: &Path) -> Result<SpiraToml, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    validate(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The environment variable that pins `lifecycle_enforce` (a unit's `Environment=`, a
/// fixture, or conf.sh, which exports it with a default of `0`).
/// The one variable a launcher builds PATH from (brain `runtime-is-a-release-2026-09-29`):
/// the root of the release the running system executes — `spira-releases/<sha>`, and until
/// the release deploy lands (sp-gkfg1) the harness checkout's root, which has `bin/` and
/// `spira/` in the same places.
pub const RELEASE_ENV: &str = "SPIRA_RELEASE";

/// The system directories every launcher's PATH ends with (the release's name-clash rule
/// checks these, `release::SYSTEM_DIRS`).
pub const SYSTEM_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The launcher's PATH, set outright — never appended to an inherited one (sp-31gtu):
/// `<release>/bin:<release>/spira:/usr/local/bin:/usr/bin:/bin`.
pub fn release_path(release: &str) -> String {
    format!("{release}/bin:{release}/spira:{SYSTEM_PATH}")
}

/// [`release_path`] of `$SPIRA_RELEASE`, or the refusal naming it: unset (or empty) is a hard
/// failure, never a fallback to an inherited PATH.
pub fn release_path_from_env(value: Option<&str>) -> Result<String, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(r) => Ok(release_path(r.trim_end_matches('/'))),
        None => Err(format!(
            "{RELEASE_ENV} is not set — the launcher sets it to the release the running system executes, and PATH is built from it; there is no fallback"
        )),
    }
}

/// A single colon-separated segment of `tail` (sp-c7b85's fix to sp-31gtu's launcher PATH,
/// which carried the release and the system directories but never the box's own —
/// `spira.path` — so a Rust binary launched directly by a unit, rather than through
/// `conf.sh`, resolved `bd`/`claude`/`dolt`/`gh`/`cargo` by bare name against nothing) that
/// resolves inside a release (any path with a `spira-releases` component) or inside a
/// checkout (a directory that is, or is enclosed by, one tracking `.git`) — the two shapes
/// "ABSOLUTELY ZERO ambiguity about which binaries to use" (runtime-is-a-release) exists to
/// rule out. `None` when the segment is neither.
fn tail_segment_refusal(seg: &str) -> Option<String> {
    let p = Path::new(seg);
    if p.components().any(|c| c.as_os_str() == "spira-releases") {
        return Some(format!(
            "spira.path entry {seg:?} is inside spira-releases — a launcher's PATH tail may name only the box's own tool directories, never a release"
        ));
    }
    let mut cur = Some(p);
    while let Some(c) = cur {
        if c.join(".git").exists() {
            return Some(format!(
                "spira.path entry {seg:?} is inside a checkout ({} tracks .git) — a launcher's PATH tail may name only the box's own tool directories, never a checkout",
                c.display()
            ));
        }
        cur = c.parent();
    }
    None
}

/// Every colon-separated entry of `tail` that [`tail_segment_refusal`] flags, in order — the
/// pure check [`release_path_with_tail`] applies before it will append anything.
pub fn tail_refusals(tail: &str) -> Vec<String> {
    tail.split(':').filter(|s| !s.is_empty()).filter_map(tail_segment_refusal).collect()
}

/// [`release_path`] plus the box's own tool-directory tail (`spira.path`: `~/.local/bin` for
/// `bd`, `dolt`, `gh`, `claude` and `duckdb`, `~/.cargo/bin` for an aeon's `cargo test`),
/// appended after the system directories — never before them, so a bare Spira tool name still
/// means only the release's own copy (sp-c7b85, amending sp-31gtu's rendering, which omitted
/// this tail for every launcher that invokes a Rust binary directly instead of sourcing
/// `conf.sh`, whose own PATH-append already carried it). An empty tail changes nothing.
/// Refuses, naming the offending entry, when [`tail_refusals`] finds one.
pub fn release_path_with_tail(release: &str, tail: &str) -> Result<String, String> {
    let base = release_path(release);
    let tail = tail.trim();
    if tail.is_empty() {
        return Ok(base);
    }
    let refusals = tail_refusals(tail);
    if !refusals.is_empty() {
        return Err(refusals.join("; "));
    }
    Ok(format!("{base}:{tail}"))
}

/// [`release_path_with_tail`] of `$SPIRA_RELEASE`, or the refusal naming it — the tail-aware
/// counterpart to [`release_path_from_env`], for a launcher that resets its environment
/// (`env -i`) and so cannot lean on an inherited PATH for either half.
pub fn release_path_from_env_with_tail(release: Option<&str>, tail: &str) -> Result<String, String> {
    let base = release_path_from_env(release)?;
    let tail = tail.trim();
    if tail.is_empty() {
        return Ok(base);
    }
    let refusals = tail_refusals(tail);
    if !refusals.is_empty() {
        return Err(refusals.join("; "));
    }
    Ok(format!("{base}:{tail}"))
}

pub const LIFECYCLE_ENFORCE_ENV: &str = "SPIRA_LIFECYCLE_ENFORCE";

/// THE lifecycle switch's resolution rule (operator decision 2026-09-28: `lifecycle_enforce`
/// is the one switch for everything that touches the lifecycle machine), identical to the
/// aeon crate's `conf::lifecycle_enforce`: an environment value, when present, wins — `1` or
/// `true` is on, anything else (including empty) is off; else the typed
/// `spira.lifecycle_enforce`; else **off**. Whether a `spira-lc` binary exists is never an
/// input.
pub fn resolve_lifecycle_enforce(env_value: Option<&str>, configured: Option<bool>) -> bool {
    match env_value {
        Some(v) => v == "1" || v == "true",
        None => configured.unwrap_or(false),
    }
}

/// [`resolve_lifecycle_enforce`] for this process: `$SPIRA_LIFECYCLE_ENFORCE`, else
/// `spira.lifecycle_enforce` in `toml_file` (or, when `None`, the document [`discover`]
/// finds), else off. An unreadable or invalid document is off, as in the aeon crate. A
/// non-UTF-8 environment value is present-but-not-`1`, so off.
pub fn lifecycle_enforce(toml_file: Option<&Path>) -> bool {
    if let Some(v) = std::env::var_os(LIFECYCLE_ENFORCE_ENV) {
        return resolve_lifecycle_enforce(Some(v.to_str().unwrap_or("")), None);
    }
    let path = toml_file.map(Path::to_path_buf).or_else(|| discover(None));
    let configured = path
        .filter(|p| p.is_file())
        .and_then(|p| load(&p).ok())
        .and_then(|d| d.spira)
        .and_then(|s| s.lifecycle_enforce);
    resolve_lifecycle_enforce(None, configured)
}

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

/// `[spira]` — host-wide keys. Every field but one is optional: `conf.sh` derives a default
/// for each of these from where the harness is installed, and a clean clone sets none of
/// them. The exception is `id_prefix` — see [`require_id_prefix`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpiraSection {
    pub home_repo: Option<String>,
    pub db: Option<String>,
    pub run: Option<String>,
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
    /// Deepest a dependent's stack may go (stacked-dependents-2026-09-28 §1) before a claim
    /// naming it is refused. Hard ceiling 4: raising it past what the schema below allows is
    /// a design change, not a config edit (per Ryan 2026-09-28: "4 is a good place to start,
    /// no higher"). 0 reproduces today's behaviour — no stacking at all.
    pub stack_max_depth: Option<u32>,
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
    pub queue_transition_pollsec: Option<u32>,
    pub queue_transition_maxsec: Option<u32>,
    pub local_backlog_count: Option<u32>,
    pub local_backlog_age: Option<u32>,
    pub summon_lock_wait: Option<u32>,
    pub concierge_inbox: Option<String>,
    pub concierge_inbox_dedup: Option<u32>,
    pub concierge_inbox_stall: Option<u32>,
    pub concierge_inbox_backoff: Option<u32>,
    pub mail_settle: Option<u64>,
    pub cockpit_clipboard: Option<String>,
    /// Switches an aeon onto the lifecycle semantic layer (work-env.sh, no bd, lc_claim_bead
    /// CAS). Default off: a tree that has simply built work/spira-lc must not flip onto it by
    /// binary presence alone (sp-74gzo) — aeon.sh only takes the restricted path when this is
    /// set, and refuses rather than falling back when set with either binary missing.
    pub lifecycle_enforce: Option<bool>,
    pub mail_settle_event: Option<String>,
    pub queue_throttle_override: Option<String>,
    pub client_settings: Option<String>,
    pub mail: Option<String>,
    /// Mutes outgoing mail: `sendmail`/`send` file the message into `cur/` already-seen
    /// instead of `new/`, so it is recorded but never wakes a reader. Replaces the tracked
    /// edit to `spira/mail.sh` that local-overrides held (sp-9hwim, design
    /// runtime-is-a-release #5) — the override checked for a file's existence
    /// (`~/.config/spira/mail-mute`); this is the same switch as a typed key so the checkout
    /// carries no uncommitted patch. Default off (unset): mail flows normally.
    pub mail_mute: Option<bool>,
    pub mail_session_mailbox: Option<String>,
    pub repo_map: Option<String>,
    pub prefix_map: Option<String>,
    pub chamber: Option<String>,
    pub chamber_overlay: Option<String>,
    pub overrides: Option<String>,
    /// REQUIRED whenever `[spira]` sets anything: the prefix of this installation's own bead
    /// ids, without the hyphen (`sp` for `sp-k6m1m`). Nothing derives it — it used to be cut
    /// from the retired `goal` key (sp-k6m1m). `spira-config validate` (doctor,
    /// pre-activate) refuses a `[spira]` table without it ([`require_id_prefix`]).
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
    pub verify_timeout: Option<String>,
    pub reclaim_grace_secs: Option<String>,
    pub operated: Option<String>,
    pub ci_park_max: Option<String>,
    pub world_stop_label: Option<String>,
    pub land_maxsec: Option<String>,
    pub land_gate_reserve: Option<String>,
    pub certify_always_covers: Option<String>,
    pub rebase_escalate_at: Option<String>,
    pub rebase_stale_log: Option<String>,
    pub eviction_escalate_at: Option<String>,
    pub verdict_window: Option<String>,
    pub check5_max_file: Option<String>,
    pub check5_max_resolve: Option<String>,
    pub remedy_window: Option<String>,
    pub pr_stall_mins: Option<String>,
    pub deferral_escalate_at: Option<String>,
    pub broker_enable: Option<String>,
    pub broker_gh_config_dir: Option<String>,
    pub broker_gh_token: Option<String>,
    pub loom_cache_s: Option<String>,
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
    pub cutover_round_label: Option<String>,
    pub gate_lock_wait: Option<String>,
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
    pub batcher_wall_secs: Option<String>,
    pub preflight_suite_max_secs: Option<String>,
    pub queue_infra_retries: Option<String>,
    pub queue_stuck_age: Option<String>,
    pub queue_dir: Option<String>,
    pub forge: Option<String>,
    pub forge_repo: Option<String>,
    pub queue_wait_label: Option<String>,
    pub queue_actions_app_id: Option<String>,
    pub queue_batcher: Option<String>,
    /// The batcher's cut on or off (`SPIRA_BATCHER_ENABLE`, "1"/"0", default "1" in
    /// conf.sh). "0": queue cuts no rounds — the operator does (sp-gypjk; replaces the
    /// retired `batcher_bin = "/bin/true"` idiom).
    pub batcher_enable: Option<String>,
    pub batch_judgement_label: Option<String>,
    pub queue_lock_wait: Option<String>,
    pub queue_lock_starve_max: Option<String>,
    pub queue_repro_ci_pollsec: Option<String>,
    pub queue_repro_ci_maxsec: Option<String>,
    pub submitted_label: Option<String>,
    pub publish_remote: Option<String>,
    pub open_children_label: Option<String>,
    pub work_close_types: Option<String>,
    pub queue_throttle_depth_at: Option<String>,
    pub queue_throttle_stall_mins: Option<String>,
    pub auron_restarts: Option<String>,
    pub auron_restart_window: Option<String>,
    pub releases: Option<String>,
    pub releases_keep: Option<String>,
    /// Hours a standing hotfix (`release activate --hotfix`) may run before `release
    /// status` emits its ALERT line — doctor, the ops pane and watchtower all key off that
    /// line rather than re-deriving the age themselves (sp-6p20x). Default 4 when unset.
    pub hotfix_alert_hours: Option<u32>,
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
    pub liveness_model: Option<String>,
    pub reflect_model: Option<String>,
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
    pub workflow_only_paths: Option<String>,
    pub gh_api: Option<String>,
    pub round_vm_state_dir: Option<String>,
    pub round_vm_provider: Option<String>,
    pub round_vm_ssh_user: Option<String>,
    pub round_vm_ssh_port: Option<String>,
    pub round_vm_host_key: Option<String>,
    pub round_vm_host_pubkey: Option<String>,
    pub round_vm_host_addr: Option<String>,
    pub round_vm_vcpus: Option<String>,
    pub round_vm_maxpar: Option<String>,
    pub round_vm_max_retries: Option<String>,
    pub round_vm_retry_interval: Option<String>,
    pub round_vm_mirror_port: Option<String>,
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
    // `gate` is retired (sp-quu2w, RETIRED_REPO_KEYS): the gate string lives in the tree
    // under test (`gate.steps`), which the gate reads; this key never had a reader.
    #[serde(default)]
    pub lanes: Vec<Lane>,
    /// Overrides the host's default forge script for this one repository. Absent means
    /// "use `[spira]`'s own", which is every repository today.
    pub forge: Option<String>,
    /// What the certification gate runs for this repository (design
    /// gate-unit-round-integration-2026-09-29, item 4, sp-2ghui). Absent means
    /// [`GateMode::Suites`], today's behaviour exactly. Read it with [`RepoSection::gate_mode`].
    pub gate_mode: Option<GateMode>,
}

impl RepoSection {
    /// The gate mode in force: the typed key, else [`GateMode::Suites`].
    pub fn gate_mode(&self) -> GateMode {
        self.gate_mode.unwrap_or_default()
    }
}

/// `[repo.<name>] gate_mode` — how the gate composes a trial (gate/DESIGN.md "Composition").
///
/// * `suites` (the default): the repository's gate string, whole — fences, build, and the
///   budgeted suite selection.
/// * `unit`: the composition follows what the branch touches. A branch that touches any
///   bash (or other script) component still runs the gate string whole; one that touches
///   only Rust crates (plus docs, config, suites) runs the fences with suites off, then
///   `cargo test -p` for each touched crate and its reverse dependents on the host; one
///   that touches nothing buildable runs the fences only.
///
/// One command reverts it: `spira-config set repo.<name>.gate_mode suites <file>`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GateMode {
    Unit,
    #[default]
    Suites,
}

impl GateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GateMode::Unit => "unit",
            GateMode::Suites => "suites",
        }
    }
}

/// `[repo.<name>] gate_mode` of `doc`, the default when the repository or the key is absent.
pub fn repo_gate_mode(doc: &SpiraToml, repo: &str) -> GateMode {
    doc.repo.get(repo).map(RepoSection::gate_mode).unwrap_or_default()
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
///
/// One field is patched after generation: `stack_max_depth`'s hard ceiling of 4 (design
/// stacked-dependents-2026-09-28 §1) has no derive-macro spelling in schemars 0.8 (no
/// `#[schemars(range(...))]`), and a `schema_with` override loses the derive's own
/// `Option<u32>` handling — it names a synthetic wrapper type instead, which schemars then
/// treats as *required* rather than nullable. Patching the generated node in place keeps the
/// `Option` semantics schemars already got right and adds only the one property this crate
/// cannot express through the derive.
pub fn json_schema() -> schemars::schema::RootSchema {
    let mut root = schemars::schema_for!(SpiraToml);
    if let Some(schemars::schema::Schema::Object(spira_section)) = root.definitions.get_mut("SpiraSection") {
        if let Some(schemars::schema::Schema::Object(prop)) = spira_section.object().properties.get_mut("stack_max_depth") {
            prop.number().maximum = Some(4.0);
        }
    }
    root
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
    // Spira works the whole backlog continuously; there is no single goal bead (per Ryan,
    // 2026-09-30, sp-2f9sa). The id prefix it used to imply is its own key now: id_prefix.
    RetiredKey { key: "goal", bead: "sp-k6m1m" },
    RetiredKey { key: "queue_local_gate", bead: "sp-vsob2" },
    RetiredKey { key: "queue_batch_idle_cut", bead: "sp-vsob2" },
    RetiredKey { key: "hook_lines", bead: "sp-o9nkc" },
    RetiredKey { key: "answer_state", bead: "sp-xsl8i" },
    RetiredKey { key: "answer_mark", bead: "sp-xsl8i" },
    RetiredKey { key: "answer_comment_mark", bead: "sp-xsl8i" },
    RetiredKey { key: "self_closed", bead: "sp-xsl8i" },
    RetiredKey { key: "wake_watchers", bead: "sp-xsl8i" },
    RetiredKey { key: "reclaim_skip_label", bead: "sp-i2m7y" },
    // No explicit CPU quotas anywhere (law-isolate-greedy-work-in-vms): the aeon launch and the
    // sentinel's landing dispatch no longer pass CPUQuota, so these keys have no consumer.
    RetiredKey { key: "aeon_cpu_quota", bead: "sp-b4oct" },
    RetiredKey { key: "land_cpu_quota", bead: "sp-b4oct" },
    // Every Spira tool is invoked by bare name on the launcher's PATH (sp-gypjk, design
    // runtime-is-a-release): a key naming a tool's path has no consumer. `batcher_bin` is
    // retired too, but a value that is not the batcher (prod's "/bin/true") is honoured as
    // `batcher_enable = "0"` by [`validate_with_warnings`], so the batcher stays off.
    RetiredKey { key: "lc_bin", bead: "sp-gypjk" },
    RetiredKey { key: "panel", bead: "sp-gypjk" },
    RetiredKey { key: "broker_bin", bead: "sp-gypjk" },
    RetiredKey { key: "czar_pass_bin", bead: "sp-gypjk" },
    RetiredKey { key: "queue_watch_bin", bead: "sp-gypjk" },
    RetiredKey { key: "supervise_bin", bead: "sp-gypjk" },
    RetiredKey { key: "landing_pass_bin", bead: "sp-gypjk" },
    RetiredKey { key: "tsd_bin", bead: "sp-gypjk" },
    RetiredKey { key: "tsd_lifecycle_export_bin", bead: "sp-gypjk" },
    RetiredKey { key: "reconciler_bin", bead: "sp-gypjk" },
    RetiredKey { key: "test_plan_bin", bead: "sp-gypjk" },
    RetiredKey { key: "reconciler_flow_bin", bead: "sp-gypjk" },
    RetiredKey { key: "loom_bin", bead: "sp-gypjk" },
    RetiredKey { key: "batcher_bin", bead: "sp-gypjk" },
];

/// A retired `batcher_bin` value that is not the batcher itself (e.g. "/bin/true", the old
/// way to switch automatic cuts off) — read as `batcher_enable = "0"`.
pub fn batcher_bin_means_off(val: &str) -> bool {
    let v = val.trim();
    !v.is_empty() && std::path::Path::new(v).file_name().and_then(|n| n.to_str()) != Some("batcher")
}

/// A `[repo.<name>]` key dropped from `RepoSection`, accepted from a live config with a
/// warning instead of the hard "unknown field" error, exactly as [`RETIRED_SPIRA_KEYS`] is.
pub const RETIRED_REPO_KEYS: &[RetiredKey] = &[
    // The gate string moved into the tree under test (`gate.steps`, gate/DESIGN.md "The tree
    // owns its gate"): a branch that ports a fence edits it in the same commit. The legacy
    // repo-map column still gates a repository whose landing ref has never carried one.
    RetiredKey { key: "gate", bead: "sp-quu2w" },
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

/// [`validate_with_warnings`] plus [`require_id_prefix`] — the check `spira-config validate`
/// runs (doctor, pre-activate): the document is well-formed AND names this installation's
/// id prefix.
pub fn validate_strict(text: &str) -> Result<(SpiraToml, Vec<String>), String> {
    let (doc, warnings) = validate_with_warnings(text)?;
    require_id_prefix(&doc)?;
    Ok((doc, warnings))
}

/// Whether `p` can be a bead id prefix: non-empty ASCII letters, digits and `_` — never a
/// hyphen, which is the separator between the prefix and the id.
pub fn valid_id_prefix(p: &str) -> bool {
    !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// sp-oppza: the ONE-TIME UPGRADE MIGRATION for a box that last wrote its config before
/// sp-k6m1m retired the goal concept. Such a box's `[spira]` table (or legacy `spira.conf`)
/// names a `goal` shaped like a bead id — `"<prefix>-<rest>"` — the exact prefix this
/// installation's own bead ids carry (`conf.sh` used to derive `SPIRA_ID_PREFIX` from it as
/// `${SPIRA_GOAL%%-*}`). With id_prefix now required and nothing deriving it, that derivation
/// is reproduced here so an existing box migrates instead of failing activation outright.
/// `None` when `goal` is not bead-id-shaped (no hyphen, or a prefix `valid_id_prefix` rejects)
/// — that box gets the ordinary "required" refusal naming the key, same as one with no goal
/// at all.
pub fn id_prefix_from_goal(goal: &str) -> Option<String> {
    let (prefix, _) = goal.split_once('-')?;
    valid_id_prefix(prefix).then(|| prefix.to_string())
}

/// sp-oppza: `spira-config migrate` — the automatic form of `the_migration_is_one_set`
/// (spira-config/tests/validate.rs): read, `set_path spira.id_prefix`, write. Run by the
/// installer (pre-activate, doctor) before either checks `spira.id_prefix` is set, so a box
/// whose config predates sp-k6m1m migrates instead of failing activation on every one of
/// them. `Ok(None)` when there is nothing to do — no `[spira]` table, `id_prefix` ALREADY SET
/// (production's own state, set by hand: a true no-op, checked first), or `goal` is not
/// bead-id-shaped (that box gets the ordinary "required" refusal naming the key, same as one
/// with no goal at all). `Ok(Some((new_text, message)))` otherwise: the migrated document,
/// with `goal` gone (the ordinary retirement already drops it — [`validate`] strips it before
/// this ever reaches `set_path`), and a message for the caller to log.
pub fn migrate_goal_to_id_prefix(text: &str) -> Result<Option<(String, String)>, String> {
    let root: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let prefix = match root.get("spira").and_then(|v| v.as_table()) {
        Some(spira) if !spira.contains_key("id_prefix") => {
            match spira.get("goal").and_then(|v| v.as_str()).and_then(id_prefix_from_goal) {
                Some(p) => p,
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    let doc = validate(text)?;
    let migrated = set_path(&doc, "spira.id_prefix", &prefix)?;
    let out = toml::to_string_pretty(&migrated).map_err(|e| e.to_string())?;
    let msg = format!("migrated: goal implied id_prefix = {prefix:?} (sp-k6m1m/sp-oppza, one-time) — goal removed");
    Ok(Some((out, msg)))
}

/// File wrapper for [`migrate_goal_to_id_prefix`]: reads `file` and, when a migration
/// applies, writes the result back atomically (via [`write_atomic`]) and returns the message
/// to log. A missing or empty file is nothing to migrate, not an error — `spira-config
/// migrate` is meant to run unconditionally, ahead of every `validate`, same as this crate's
/// other idempotent seams (`spira_toml_resolve`'s auto-convert).
pub fn migrate_goal_to_id_prefix_in_file(file: &std::path::Path) -> Result<Option<String>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    match migrate_goal_to_id_prefix(&text)? {
        Some((out, msg)) => {
            write_atomic(file, &out).map_err(|e| format!("{}: {e}", file.display()))?;
            Ok(Some(msg))
        }
        None => Ok(None),
    }
}

/// Whether `[spira]` sets any key at all. An empty table — the shape `convert` writes when
/// it regenerates only `[persona.*]` — says nothing about the host, so it needs no prefix.
fn spira_sets_anything(s: &SpiraSection) -> bool {
    match toml::Value::try_from(s) {
        Ok(toml::Value::Table(t)) => t.values().any(|v| !matches!(v, toml::Value::Array(a) if a.is_empty())),
        _ => true,
    }
}

/// THE ID PREFIX FAILS CLOSED (sp-k6m1m). A `[spira]` table that sets anything must name
/// `id_prefix`, and it must be a usable prefix. It used to be derived in conf.sh from the
/// goal epic's id (`${SPIRA_GOAL%%-*}`); with the goal retired nothing derives it. A
/// document with no `[spira]` table, or an empty one, passes: it configures no installation.
///
/// WHERE IT REFUSES: [`validate_strict`] — `spira-config validate`, which is what `doctor`
/// and the release's `pre-activate` run against the config in force, so a release cannot
/// be activated over a config without one. The typed READERS ([`validate`], [`load`]) do not
/// refuse a whole document for it: a daemon that cannot parse its config falls back to
/// defaults for every key, which is a far wider failure than the one key being absent.
pub fn require_id_prefix(doc: &SpiraToml) -> Result<(), String> {
    let Some(s) = doc.spira.as_ref() else { return Ok(()) };
    match s.id_prefix.as_deref() {
        Some(p) if valid_id_prefix(p) => Ok(()),
        Some(p) => Err(format!(
            "spira.id_prefix: {p:?} is not a bead id prefix (letters, digits and _ only, no hyphen)"
        )),
        None if !spira_sets_anything(s) => Ok(()),
        None => Err(
            "spira.id_prefix: required — the prefix of this installation's own bead ids, without \
             the hyphen (e.g. \"sp\"); it is no longer derived from the retired goal key (sp-k6m1m)"
                .into(),
        ),
    }
}

/// Like [`validate`], but returns one warning per [`RETIRED_SPIRA_KEYS`] member found under
/// `[spira]` — naming the key and the bead that retired it — instead of silently dropping
/// them. A key `SPIRA_CONF_KEYS`/`SpiraSection` never accepted still hard-errors: only
/// listed retirements are stripped before the deserialize that would otherwise refuse them.
pub fn validate_with_warnings(text: &str) -> Result<(SpiraToml, Vec<String>), String> {
    let mut root: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let mut warnings = Vec::new();
    if let Some(spira) = root.get_mut("spira").and_then(|v| v.as_table_mut()) {
        // BEFORE the generic strip: a batcher_bin that is not the batcher switched the cuts
        // off, and dropping it silently would switch them back on (sp-gypjk).
        let off = spira.get("batcher_bin").and_then(|v| v.as_str()).map(batcher_bin_means_off).unwrap_or(false);
        if off && !spira.contains_key("batcher_enable") {
            spira.insert("batcher_enable".into(), toml::Value::String("0".into()));
            warnings.push(
                "batcher_bin is retired (sp-gypjk); its non-batcher value is read as batcher_enable = \"0\" — replace it with that".into(),
            );
        }
        for retired in RETIRED_SPIRA_KEYS {
            if spira.remove(retired.key).is_some() {
                warnings.push(format!(
                    "{} is retired ({}) and ignored — remove it",
                    retired.key, retired.bead
                ));
            }
        }
    }
    if let Some(repos) = root.get_mut("repo").and_then(|v| v.as_table_mut()) {
        for (name, table) in repos.iter_mut() {
            let Some(table) = table.as_table_mut() else { continue };
            for retired in RETIRED_REPO_KEYS {
                if table.remove(retired.key).is_some() {
                    warnings.push(format!(
                        "repo.{name}.{} is retired ({}) and ignored — remove it",
                        retired.key, retired.bead
                    ));
                }
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
///
/// A RETIRED key (`spira.<key>` in [`RETIRED_SPIRA_KEYS`], `repo.<name>.<key>` in
/// [`RETIRED_REPO_KEYS`]) is not a field any more, so it cannot be nulled; `validate` already
/// dropped it from `doc`, and writing `doc` back is what removes it from the file. Unsetting
/// one is therefore the document unchanged, never an "unknown field" refusal — that refusal
/// would leave the retired line in the live file with no CLI way to take it out.
pub fn unset_path(doc: &SpiraToml, path: &str) -> Result<SpiraToml, String> {
    if is_retired_path(path) {
        return Ok(doc.clone());
    }
    set_path_leaf(doc, path, serde_json::Value::Null)
}

/// Whether `path` names a retired key (see [`unset_path`]).
pub fn is_retired_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('.').collect();
    match parts.as_slice() {
        ["spira", key] => RETIRED_SPIRA_KEYS.iter().any(|r| r.key == *key),
        ["repo", _, key] => RETIRED_REPO_KEYS.iter().any(|r| r.key == *key),
        _ => false,
    }
}

/// `spira-config set` for several paths at once, as a library call: read and validate
/// `file`, apply every `(path, value)` with [`set_path`], re-validate the result, and write
/// it with [`write_atomic`] — all or nothing. For a Rust caller that must change two keys
/// together (queue's land-mode transition writes `mode` and `base` as one fact), so the
/// document is never observed with one written and not the other.
pub fn set_paths_in_file(file: &std::path::Path, pairs: &[(&str, &str)]) -> Result<(), String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut doc = if text.trim().is_empty() { SpiraToml::default() } else { validate(&text)? };
    for (path, value) in pairs {
        doc = set_path(&doc, path, value)?;
    }
    let out = toml::to_string_pretty(&doc).map_err(|e| format!("{}: {e}", file.display()))?;
    validate(&out)?;
    write_atomic(file, &out).map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_path_is_the_release_then_the_system_dirs_and_unset_is_refused() {
        assert_eq!(
            release_path("/r/spira-releases/abc"),
            "/r/spira-releases/abc/bin:/r/spira-releases/abc/spira:/usr/local/bin:/usr/bin:/bin"
        );
        assert_eq!(release_path_from_env(Some("/h/")).unwrap(), "/h/bin:/h/spira:/usr/local/bin:/usr/bin:/bin");
        for v in [None, Some(""), Some("  ")] {
            assert!(release_path_from_env(v).unwrap_err().contains("SPIRA_RELEASE is not set"));
        }
    }

    #[test]
    fn release_path_with_tail_appends_after_the_system_dirs() {
        assert_eq!(
            release_path_with_tail("/r/spira-releases/abc", "/h/.local/bin:/h/.cargo/bin").unwrap(),
            "/r/spira-releases/abc/bin:/r/spira-releases/abc/spira:/usr/local/bin:/usr/bin:/bin:/h/.local/bin:/h/.cargo/bin"
        );
        // An empty (or all-whitespace) tail changes nothing — same as before this bead.
        for empty in ["", "   "] {
            assert_eq!(release_path_with_tail("/r/spira-releases/abc", empty).unwrap(), release_path("/r/spira-releases/abc"));
        }
    }

    #[test]
    fn release_path_with_tail_refuses_a_tail_entry_inside_a_release() {
        let e = release_path_with_tail("/r/spira-releases/abc", "/r/spira-releases/def/bin").unwrap_err();
        assert!(e.contains("spira-releases"), "{e}");
        assert!(e.contains("/r/spira-releases/def/bin"), "{e}");
    }

    #[test]
    fn release_path_with_tail_refuses_a_tail_entry_inside_a_checkout() {
        let dir = testkit::TempDir::new("spira-config-tail-checkout");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let e = release_path_with_tail("/r/spira-releases/abc", bin.to_str().unwrap()).unwrap_err();
        assert!(e.contains("checkout"), "{e}");
        assert!(e.contains(".git"), "{e}");
    }

    #[test]
    fn release_path_from_env_with_tail_composes_both_refusals() {
        assert_eq!(
            release_path_from_env_with_tail(Some("/h/"), "/x/y").unwrap(),
            "/h/bin:/h/spira:/usr/local/bin:/usr/bin:/bin:/x/y"
        );
        assert!(release_path_from_env_with_tail(None, "/x/y").unwrap_err().contains("SPIRA_RELEASE is not set"));
        assert!(release_path_from_env_with_tail(Some("/h/"), "/r/spira-releases/def").unwrap_err().contains("spira-releases"));
    }

    #[test]
    fn lifecycle_enforce_resolution_matches_aeon() {
        // The environment wins, both ways; set-but-empty is off.
        assert!(resolve_lifecycle_enforce(Some("1"), Some(false)));
        assert!(resolve_lifecycle_enforce(Some("true"), None));
        assert!(!resolve_lifecycle_enforce(Some("0"), Some(true)));
        assert!(!resolve_lifecycle_enforce(Some(""), Some(true)));
        assert!(!resolve_lifecycle_enforce(Some("yes"), None));
        assert!(!resolve_lifecycle_enforce(Some("TRUE"), None));
        // No environment: the typed key, else off.
        assert!(resolve_lifecycle_enforce(None, Some(true)));
        assert!(!resolve_lifecycle_enforce(None, Some(false)));
        assert!(!resolve_lifecycle_enforce(None, None));
    }

    #[test]
    fn lifecycle_enforce_reads_the_typed_key_from_a_document() {
        // Only meaningful when the process environment does not pin the switch.
        if std::env::var_os(LIFECYCLE_ENFORCE_ENV).is_some() {
            return;
        }
        let dir = testkit::TempDir::new("spira-config-lce");
        let p = dir.join("spira.toml");
        std::fs::write(&p, "[spira]\nid_prefix = \"sp\"\nlifecycle_enforce = true\n").unwrap();
        assert!(lifecycle_enforce(Some(&p)));
        std::fs::write(&p, "[spira]\nid_prefix = \"sp\"\nlifecycle_enforce = false\n").unwrap();
        assert!(!lifecycle_enforce(Some(&p)));
        std::fs::write(&p, "[spira]\nid_prefix = \"sp\"\nnot_a_key = 1\n").unwrap();
        assert!(!lifecycle_enforce(Some(&p)), "an invalid document is off");
        assert!(!lifecycle_enforce(Some(&dir.join("absent.toml"))), "a named but absent document is off");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_minimal_document() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmax_aeons = 4\n").expect("valid");
        assert_eq!(doc.spira.unwrap().max_aeons, Some(4));
    }

    #[test]
    fn unknown_key_names_its_path() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nbogus = 1\n").unwrap_err();
        assert!(err.starts_with("spira.bogus"), "{err}");
    }

    #[test]
    fn wrong_type_names_its_path() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nmax_aeons = \"four\"\n").unwrap_err();
        assert!(err.starts_with("spira.max_aeons"), "{err}");
    }

    #[test]
    fn bad_enum_names_its_path() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nczar_stage_deadlock = \"sometimes\"\n").unwrap_err();
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
        let err = validate("[spira]\nid_prefix = \"sp\"\ndb = /home/x # a trailing comment\n").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn the_inline_comment_scar_is_harmless_quoted() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\ndb = \"/home/x\" # a trailing comment\n").expect("valid");
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
        let doc = validate("[spira]\nid_prefix = \"sp\"\nhome_repo = \"a b\"\nmax_aeons = 4\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("HOME_REPO='a b'\n"), "{out}");
        assert!(out.contains("MAX_AEONS='4'\n"), "{out}");
    }

    #[test]
    fn export_sh_renders_a_toml_bool_as_the_word_true_not_1() {
        // sp-9hwim: a caller reading a bool key off export --sh (mail.sh's SPIRA_MAIL_MUTE)
        // must match on "1|true", the same spelling every other [spira] bool key already
        // uses — never a bare `= "1"`, which a TOML `true` would silently fail.
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmail_mute = true\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("MAIL_MUTE='true'\n"), "{out}");
    }

    #[test]
    fn set_path_on_empty_document_creates_the_table() {
        let doc = set_path(&SpiraToml::default(), "spira.prod", "/srv/x").unwrap();
        assert_eq!(doc.spira.unwrap().prod, Some("/srv/x".to_string()));
    }

    #[test]
    fn set_path_replaces_without_disturbing_siblings() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nprod = \"/old\"\nmax_aeons = 4\n").unwrap();
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
        let (doc, warnings) = validate_with_warnings("[spira]\nid_prefix = \"sp\"\nqueue_local_gate = 1\n")
            .expect("a retired key must not be a hard error");
        assert!(doc.spira.is_some());
        assert!(
            warnings.iter().any(|w| w.contains("queue_local_gate") && w.contains("sp-vsob2")),
            "{warnings:?}"
        );
    }

    #[test]
    fn the_retired_repo_gate_key_warns_and_is_ignored() {
        let (doc, warnings) = validate_with_warnings(
            "[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngate = \"bash spira/x.sh\"\n",
        )
        .expect("a retired repo key must not be a hard error");
        assert!(doc.repo.contains_key("spira"));
        assert!(
            warnings.iter().any(|w| w.contains("repo.spira.gate") && w.contains("sp-quu2w")),
            "{warnings:?}"
        );
        // Positive control: a misspelt repo key is still refused.
        let err = validate("[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngatee = 1\n").unwrap_err();
        assert!(err.contains("repo.spira"), "{err}");
    }

    #[test]
    fn unsetting_a_retired_key_removes_it_instead_of_refusing() {
        let text = "[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngate = \"bash spira/x.sh\"\n";
        let doc = validate(text).unwrap();
        let out = toml::to_string_pretty(&unset_path(&doc, "repo.spira.gate").unwrap()).unwrap();
        assert!(!out.contains("gate"), "{out}");
        assert!(out.contains("path = \"/p\""), "{out}");
        assert!(unset_path(&doc, "spira.aeon_cpu_quota").is_ok());
        // Positive control: an unknown, never-retired key is still refused.
        assert!(unset_path(&doc, "repo.spira.gatee").is_err());
    }

    #[test]
    fn retired_cpu_quota_keys_warn_and_are_ignored() {
        // sp-b4oct: a live spira.toml still carrying the aeon/landing CPU quota validates,
        // with one warning per key naming the retiring bead, and nothing exports them.
        let (doc, warnings) = validate_with_warnings(
            "[spira]\nid_prefix = \"sp\"\naeon_cpu_quota = \"400\"\nland_cpu_quota = \"70\"\n",
        )
        .expect("a retired CPU quota key must not be a hard error");
        for key in ["aeon_cpu_quota", "land_cpu_quota"] {
            assert!(
                warnings.iter().any(|w| w.contains(key) && w.contains("sp-b4oct")),
                "no warning for {key}: {warnings:?}"
            );
        }
        let sh = export_sh(&doc);
        assert!(!sh.contains("CPU_QUOTA"), "a retired key leaked into export --sh: {sh}");
    }

    #[test]
    fn a_misspelt_key_still_errors() {
        // Positive control for a_retired_key_warns_instead_of_erroring: a key that looks
        // like a retired one but isn't must still be refused, not silently accepted.
        let err = validate("[spira]\nid_prefix = \"sp\"\nqueue_local_gatee = 1\n").unwrap_err();
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
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmax_live_aeons = 3\nmax_aeons = 4\n").unwrap();
        let doc = unset_path(&doc, "spira.max_live_aeons").unwrap();
        // omitted entirely from the serialized document, not written as an empty value.
        let out = toml::to_string_pretty(&doc).unwrap();
        assert!(!out.contains("max_live_aeons"), "{out}");
        let spira = doc.spira.unwrap();
        assert_eq!(spira.max_live_aeons, None);
        assert_eq!(spira.max_aeons, Some(4));
    }

    // ENV VARS ARE PROCESS-GLOBAL: every `discover` test holding one of these keys takes
    // `ENV_LOCK` for its whole body, so two tests never observe each other's value mid-run.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static SCRATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn scratch_dir(tag: &str) -> testkit::TempDir {
        let n = SCRATCH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        testkit::TempDir::new(&format!("spira-config-lib-test-{tag}-{n}"))
    }

    #[test]
    fn find_under_locates_the_file() {
        let dir = scratch_dir("find-under-present");
        std::fs::write(dir.join(FILE_NAME), "[spira]\n").unwrap();
        assert_eq!(find_under(&dir), Some(dir.join(FILE_NAME)));
    }

    #[test]
    fn find_under_is_none_when_absent() {
        // POSITIVE CONTROL for find_under_locates_the_file: an empty scratch dir must not
        // report a file that was never written.
        let dir = scratch_dir("find-under-absent");
        assert_eq!(find_under(&dir), None);
    }

    #[test]
    fn discover_prefers_the_explicit_path_over_the_environment() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = std::env::var("SPIRA_TOML").ok();
        std::env::set_var("SPIRA_TOML", "/should-not-be-used/spira.toml");
        let explicit = PathBuf::from("/explicit/spira.toml");
        assert_eq!(discover(Some(explicit.clone())), Some(explicit));
        match saved {
            Some(v) => std::env::set_var("SPIRA_TOML", v),
            None => std::env::remove_var("SPIRA_TOML"),
        }
    }

    #[test]
    fn discover_falls_back_through_spira_toml_then_spira_repo() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved_toml = std::env::var("SPIRA_TOML").ok();
        let saved_repo = std::env::var("SPIRA_REPO").ok();
        std::env::remove_var("SPIRA_TOML");
        let repo_dir = scratch_dir("discover-repo");
        std::fs::write(repo_dir.join(FILE_NAME), "[spira]\n").unwrap();
        std::env::set_var("SPIRA_REPO", &repo_dir);
        assert_eq!(discover(None), Some(repo_dir.join(FILE_NAME)));
        match saved_toml {
            Some(v) => std::env::set_var("SPIRA_TOML", v),
            None => std::env::remove_var("SPIRA_TOML"),
        }
        match saved_repo {
            Some(v) => std::env::set_var("SPIRA_REPO", v),
            None => std::env::remove_var("SPIRA_REPO"),
        }
    }

    #[test]
    fn load_reads_and_validates() {
        let dir = scratch_dir("load-ok");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "[spira]\nid_prefix = \"sp\"\nmax_aeons = 4\n").unwrap();
        let doc = load(&path).expect("valid document");
        assert_eq!(doc.spira.unwrap().max_aeons, Some(4));
    }

    #[test]
    fn load_names_the_path_on_a_parse_error() {
        let dir = scratch_dir("load-bad");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "[spira]\nid_prefix = \"sp\"\nbogus = 1\n").unwrap();
        let err = load(&path).unwrap_err();
        assert!(err.contains(&path.display().to_string()), "{err}");
    }

    #[test]
    fn set_paths_in_file_writes_both_or_neither() {
        let dir = testkit::TempDir::new("spira-config-setpaths");
        let path = dir.join("spira.toml");
        std::fs::write(&path, "[repo.r]\npath = \"/x\"\nmode = \"queue.local\"\nbase = \"local/main\"\n").unwrap();
        set_paths_in_file(&path, &[("repo.r.mode", "queue.forge"), ("repo.r.base", "origin/main")]).unwrap();
        let doc = load(&path).unwrap();
        assert_eq!(get_path(&doc, "repo.r.mode").as_deref(), Some("queue.forge"));
        assert_eq!(get_path(&doc, "repo.r.base").as_deref(), Some("origin/main"));
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(set_paths_in_file(&path, &[("repo.r.base", "local/main"), ("repo.r.mode", "bogus")]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------ gate_mode (sp-2ghui)

    const REPO_R: &str = "[repo.r]\npath = \"/x\"\nmode = \"queue.local\"\n";

    #[test]
    fn gate_mode_defaults_to_suites_when_absent() {
        let doc = validate(REPO_R).unwrap();
        assert_eq!(repo_gate_mode(&doc, "r"), GateMode::Suites);
        assert_eq!(repo_gate_mode(&doc, "no-such-repo"), GateMode::Suites);
        assert_eq!(get_path(&doc, "repo.r.gate_mode"), None);
    }

    #[test]
    fn gate_mode_reads_both_values() {
        for (v, want) in [("unit", GateMode::Unit), ("suites", GateMode::Suites)] {
            let doc = validate(&format!("{REPO_R}gate_mode = \"{v}\"\n")).unwrap();
            assert_eq!(repo_gate_mode(&doc, "r"), want);
            assert_eq!(want.as_str(), v);
            assert_eq!(get_path(&doc, "repo.r.gate_mode").as_deref(), Some(v));
        }
    }

    #[test]
    fn gate_mode_refuses_anything_else() {
        for bad in ["\"Unit\"", "\"fast\"", "\"\"", "true", "1"] {
            let err = validate(&format!("{REPO_R}gate_mode = {bad}\n")).unwrap_err();
            assert!(err.contains("repo.r.gate_mode"), "{bad}: {err}");
        }
    }

    #[test]
    fn gate_mode_is_set_and_reverted_through_set_path() {
        let doc = validate(REPO_R).unwrap();
        let on = set_path(&doc, "repo.r.gate_mode", "unit").unwrap();
        assert_eq!(repo_gate_mode(&on, "r"), GateMode::Unit);
        let text = toml::to_string_pretty(&on).unwrap();
        assert!(text.contains("gate_mode = \"unit\""), "{text}");
        assert!(set_path(&doc, "repo.r.gate_mode", "fast").is_err());
        let off = set_path(&on, "repo.r.gate_mode", "suites").unwrap();
        assert_eq!(repo_gate_mode(&off, "r"), GateMode::Suites);
        let gone = unset_path(&on, "repo.r.gate_mode").unwrap();
        assert_eq!(repo_gate_mode(&gone, "r"), GateMode::Suites);
    }

    // ---- sp-oppza: migrate_goal_to_id_prefix — the automatic one-time upgrade migration ----

    #[test]
    fn id_prefix_from_goal_reads_the_prefix_and_refuses_what_is_not_bead_shaped() {
        assert_eq!(id_prefix_from_goal("sp-spira").as_deref(), Some("sp"));
        assert_eq!(id_prefix_from_goal("tt-own-thing").as_deref(), Some("tt"));
        for bad in ["nohyphen", "-leadinghyphen", "sp with space-x", ""] {
            assert_eq!(id_prefix_from_goal(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn migrate_derives_id_prefix_from_a_bead_shaped_goal_and_drops_goal() {
        let (out, msg) = migrate_goal_to_id_prefix("[spira]\ngoal = \"sp-spira\"\ndb = \"/db\"\n")
            .expect("parses")
            .expect("a migration applies");
        assert!(msg.contains("sp-k6m1m") && msg.contains("sp-oppza"), "{msg}");
        let (doc, _) = validate_strict(&out).expect("the migrated document passes the same check doctor/pre-activate run");
        let s = doc.spira.unwrap();
        assert_eq!(s.id_prefix.as_deref(), Some("sp"));
        assert_eq!(s.db.as_deref(), Some("/db"), "the rest of [spira] survives");
        assert!(!out.contains("goal"), "{out}");
    }

    #[test]
    fn migrate_is_a_true_no_op_once_id_prefix_is_set_by_hand() {
        // Production's own state: id_prefix already set BY HAND. Checked first, so a goal
        // left behind alongside it changes nothing — no rewrite, same as the retired-key
        // warning path already covers.
        assert_eq!(migrate_goal_to_id_prefix("[spira]\nid_prefix = \"sp\"\ngoal = \"sp-spira\"\n").unwrap(), None);
        assert_eq!(migrate_goal_to_id_prefix("[spira]\nid_prefix = \"tt\"\n").unwrap(), None);
    }

    #[test]
    fn migrate_is_a_no_op_with_nothing_to_migrate() {
        assert_eq!(migrate_goal_to_id_prefix("[spira]\ndb = \"/db\"\n").unwrap(), None, "no goal at all");
        assert_eq!(migrate_goal_to_id_prefix("[spira]\ngoal = \"nohyphen\"\n").unwrap(), None, "goal not bead-shaped");
        assert_eq!(migrate_goal_to_id_prefix("[repo.a]\npath = \"/a\"\nmode = \"push\"\n").unwrap(), None, "no [spira] table at all");
        assert_eq!(migrate_goal_to_id_prefix("").unwrap(), None, "empty document");
    }

    #[test]
    fn migrate_in_file_writes_atomically_and_a_second_run_is_silent() {
        let dir = testkit::TempDir::new("spira-config-migrate");
        let path = dir.join("spira.toml");
        std::fs::write(&path, "[spira]\ngoal = \"sp-spira\"\ndb = \"/db\"\n").unwrap();

        let msg = migrate_goal_to_id_prefix_in_file(&path).unwrap().expect("first run migrates");
        assert!(msg.contains("sp-oppza"), "{msg}");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("id_prefix = \"sp\""), "{after}");
        assert!(!after.contains("goal"), "{after}");

        // SECOND RUN IS SILENT: id_prefix is there now, so this is the true no-op path —
        // proof the migration ran exactly once, not on every invocation.
        assert_eq!(migrate_goal_to_id_prefix_in_file(&path).unwrap(), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after, "unchanged on the second run");
    }

    #[test]
    fn migrate_in_file_is_a_no_op_on_a_missing_or_empty_file() {
        let dir = testkit::TempDir::new("spira-config-migrate-missing");
        assert_eq!(migrate_goal_to_id_prefix_in_file(&dir.join("nonexistent.toml")).unwrap(), None);
        let empty = dir.join("empty.toml");
        std::fs::write(&empty, "").unwrap();
        assert_eq!(migrate_goal_to_id_prefix_in_file(&empty).unwrap(), None);
    }
}
