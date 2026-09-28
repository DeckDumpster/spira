//! The `spira.conf` / `repo-map` / `chamber/*.fayth` → `spira.toml` converter.
//!
//! `spira.conf` is explicitly not shell (conf.sh's own header: "IT IS NOT SOURCED"), so its
//! reader here is a direct port of `spira_conf_read`'s semantics — same trimming, same
//! one-layer quote strip, same `~`/`$HOME` expansion, same "unknown key is reported, not
//! obeyed." `repo-map` is pipe-delimited data; its reader is a port of `lib.sh`'s
//! `repo_field` column heuristic, kept identical so a row this converter reads the same way
//! the shell reader already does.
//!
//! `chamber/*.fayth` files ARE shell — sourced, with values built from `${VAR:+text}`
//! parameter expansion referencing `[spira]` keys like `SPIRA_SCOPE_LABEL`. Reimplementing
//! bash is out of scope; a small, bounded expander for exactly the forms these files use
//! (`${NAME:+repl}` and bare `$NAME`) is not.

use std::collections::BTreeMap;

use crate::{LandMode, Lane, Lease, OnOff, PersonaSection, RepoSection, SpiraSection, SpiraToml};

/// Everything that came from the inputs but had nowhere to go in the schema: an unknown
/// `spira.conf` key, a row with an unknown land mode, a fayth field this schema does not
/// carry. Never fatal — matching `spira_conf_read`'s own "report, don't refuse" — but
/// returned so a caller can show what a widened schema would still need to capture. An
/// unrecognized lane token is not among these: see [`convert`]'s `Err`.
#[derive(Debug, Default, Clone)]
pub struct ConvertWarnings(pub Vec<String>);

impl ConvertWarnings {
    fn push(&mut self, w: impl Into<String>) {
        self.0.push(w.into());
    }
}

/// Parses `spira.conf` text into raw `KEY -> value` pairs, applying exactly
/// `spira_conf_read`'s trimming, quote-stripping and `~`/`$HOME` expansion. Does not filter
/// by the allowlist — that happens in [`spira_section`], which is where "unknown key" is
/// reported instead of just "read."
pub fn read_conf(text: &str, home: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        let mut val = val.trim().to_string();
        if (val.starts_with('"') && val.ends_with('"') && val.len() >= 2)
            || (val.starts_with('\'') && val.ends_with('\'') && val.len() >= 2)
        {
            val = val[1..val.len() - 1].to_string();
        }
        if val == "~" || val.starts_with("~/") {
            val = format!("{home}{}", &val[1..]);
        }
        let val = val.replace("$HOME", home).replace("${HOME}", home);
        out.insert(key, val);
    }
    out
}

fn parse_u32(w: &mut ConvertWarnings, key: &str, val: &str) -> Option<u32> {
    val.parse()
        .map_err(|_| w.push(format!("spira.{key}: not an integer: {val:?}")))
        .ok()
}

fn parse_u64(w: &mut ConvertWarnings, key: &str, val: &str) -> Option<u64> {
    val.parse()
        .map_err(|_| w.push(format!("spira.{key}: not an integer: {val:?}")))
        .ok()
}

fn parse_bool01(w: &mut ConvertWarnings, key: &str, val: &str) -> Option<bool> {
    match val {
        "1" => Some(true),
        "0" => Some(false),
        other => {
            w.push(format!("spira.{key}: not 0/1: {other:?}"));
            None
        }
    }
}

/// Builds `[spira]` from the raw key/value pairs [`read_conf`] produced. Every key
/// `conf.sh`'s own `SPIRA_CONF_KEYS` accepts has a match arm below; a key that reaches the
/// catch-all is refused rather than dropped, because a key neither `conf.sh` nor this schema
/// recognises being silently ignored is exactly the class of bug that let ~200 accepted keys
/// vanish through this converter (sp-9nljd) — the round-trip check `configure.sh` runs
/// (`297c84742`) only greps for the warning text, so a real install's `spira.conf`, never
/// routed through `configure.sh`, had no such backstop.
pub fn spira_section(
    raw: &BTreeMap<String, String>,
    warnings: &mut ConvertWarnings,
) -> Result<SpiraSection, Vec<String>> {
    let mut s = SpiraSection::default();
    let mut errors = Vec::new();
    for (key, val) in raw {
        match key.as_str() {
            "SPIRA_HOME_REPO" => s.home_repo = Some(val.clone()),
            "SPIRA_DB" => s.db = Some(val.clone()),
            "SPIRA_RUN" => s.run = Some(val.clone()),
            "SPIRA_GOAL" => s.goal = Some(val.clone()),
            "SPIRA_PATH" => s.path = Some(val.clone()),
            "SPIRA_WORKSPACES" => s.workspaces = Some(val.clone()),
            "SPIRA_PROD" => s.prod = Some(val.clone()),
            "SPIRA_DOLT_DATA" => s.dolt_data = Some(val.clone()),
            "SPIRA_WIKI" => s.wiki = Some(val.clone()),
            "SPIRA_WIKI_HOOK" => s.wiki_hook = Some(val.clone()),
            "SPIRA_VIEW" => s.view = Some(val.clone()),
            "SPIRA_TZ" => s.tz = Some(val.clone()),
            "SPIRA_OPERATOR" => s.operator = Some(val.clone()),
            "SPIRA_OPERATOR_ACTOR" => s.operator_actor = Some(val.clone()),
            "SPIRA_ASK_LABEL" => s.ask_label = Some(val.clone()),
            "SPIRA_CI_LABEL" => s.ci_label = Some(val.clone()),
            "SPIRA_SCOPE_LABEL" => s.scope_label = Some(val.clone()),
            "SPIRA_ACTIONABLE" => s.actionable = Some(val.clone()),
            "SPIRA_NOTIFY_AGE" => s.notify_age = parse_u64(warnings, "notify_age", val),
            "SPIRA_VERDICT_TTL" => s.verdict_ttl = parse_u64(warnings, "verdict_ttl", val),
            "SPIRA_MAX_AEONS" => s.max_aeons = parse_u32(warnings, "max_aeons", val),
            "SPIRA_MAX_LIVE_AEONS" => s.max_live_aeons = parse_u32(warnings, "max_live_aeons", val),
            "SPIRA_STACK_MAX_DEPTH" => s.stack_max_depth = parse_u32(warnings, "stack_max_depth", val),
            "SPIRA_FAYTHS" => s.fayths = val.split_whitespace().map(str::to_string).collect(),
            "SPIRA_BATCH_MAXPAR" => s.batch_maxpar = parse_u32(warnings, "batch_maxpar", val),
            "SPIRA_CERTIFY_PAR" => s.certify_par = parse_u32(warnings, "certify_par", val),
            "SPIRA_CERTIFY_SUITES" => {
                s.certify_suites = match val.as_str() {
                    "on" => Some(OnOff::On),
                    "off" => Some(OnOff::Off),
                    other => {
                        warnings.push(format!("spira.certify_suites: not on/off: {other:?}"));
                        None
                    }
                }
            }
            "SPIRA_CERT_IDLE_SKIP" => {
                s.cert_idle_skip = parse_bool01(warnings, "cert_idle_skip", val)
            }
            "SPIRA_QUEUE_BATCH_MAX" => {
                s.queue_batch_max = parse_u32(warnings, "queue_batch_max", val)
            }
            "SPIRA_QUEUE_BATCH_WAIT" => {
                s.queue_batch_wait = parse_u64(warnings, "queue_batch_wait", val)
            }
            "SPIRA_QUEUE_CI_MAXSEC" => {
                s.queue_ci_maxsec = parse_u64(warnings, "queue_ci_maxsec", val)
            }
            "SPIRA_QUEUE_THROTTLE_RELEASE_AT" => {
                s.queue_throttle_release_at = parse_u32(warnings, "queue_throttle_release_at", val)
            }
            "SPIRA_SUITES_BUDGET" => s.suites_budget = parse_u64(warnings, "suites_budget", val),
            "SPIRA_LOOM_ADDR" => s.loom_addr = Some(val.clone()),
            "SPIRA_LOOM_BUDGET_MS" => s.loom_budget_ms = parse_u64(warnings, "loom_budget_ms", val),
            "SPIRA_MAIL_READERS" => s.mail_readers = Some(val.clone()),
            "SPIRA_GH_INTAKE_REPO" => s.gh_intake_repo = Some(val.clone()),
            "COCKPIT_BOTTOM_PCT" => {
                s.cockpit_bottom_pct = parse_u32(warnings, "cockpit_bottom_pct", val)
            }
            "COCKPIT_RIGHT_PCT" => {
                s.cockpit_right_pct = parse_u32(warnings, "cockpit_right_pct", val)
            }
            "SPIRA_CZAR_STAGE_DEADLOCK" => {
                s.czar_stage_deadlock = match val.as_str() {
                    "shadow" => Some(crate::CzarStage::Shadow),
                    "act" => Some(crate::CzarStage::Act),
                    other => {
                        warnings.push(format!(
                            "spira.czar_stage_deadlock: not shadow/act: {other:?}"
                        ));
                        None
                    }
                }
            }
            "SPIRA_BD" => s.bd = Some(val.clone()),
            "SPIRA_WATCHERS" => s.watchers = Some(val.clone()),
            "SPIRA_WATCHERS_OVERLAY" => s.watchers_overlay = Some(val.clone()),
            "SPIRA_COCKPIT" => s.cockpit = Some(val.clone()),
            "SPIRA_BATCH_MEM_PER_SUITE_MIB" => {
                s.batch_mem_per_suite_mib = parse_u32(warnings, "batch_mem_per_suite_mib", val)
            }
            "SPIRA_LANES_MAX_LIVE" => s.lanes_max_live = parse_u32(warnings, "lanes_max_live", val),
            "SPIRA_THRASH_MINUTES" => s.thrash_minutes = parse_u32(warnings, "thrash_minutes", val),
            "SPIRA_QUEUE_TRANSITION_POLLSEC" => s.queue_transition_pollsec = parse_u32(warnings, "queue_transition_pollsec", val),
            "SPIRA_QUEUE_TRANSITION_MAXSEC" => s.queue_transition_maxsec = parse_u32(warnings, "queue_transition_maxsec", val),
            "SPIRA_LOCAL_BACKLOG_COUNT" => s.local_backlog_count = parse_u32(warnings, "local_backlog_count", val),
            "SPIRA_LOCAL_BACKLOG_AGE" => s.local_backlog_age = parse_u32(warnings, "local_backlog_age", val),
            "SPIRA_SUMMON_LOCK_WAIT" => s.summon_lock_wait = parse_u32(warnings, "summon_lock_wait", val),
            "SPIRA_CONCIERGE_INBOX" => s.concierge_inbox = Some(val.clone()),
            "SPIRA_CONCIERGE_INBOX_DEDUP" => {
                s.concierge_inbox_dedup = parse_u32(warnings, "concierge_inbox_dedup", val)
            }
            "SPIRA_CONCIERGE_INBOX_STALL" => {
                s.concierge_inbox_stall = parse_u32(warnings, "concierge_inbox_stall", val)
            }
            "SPIRA_CONCIERGE_INBOX_BACKOFF" => {
                s.concierge_inbox_backoff = parse_u32(warnings, "concierge_inbox_backoff", val)
            }
            "SPIRA_MAIL_SETTLE" => s.mail_settle = parse_u64(warnings, "mail_settle", val),
            "COCKPIT_CLIPBOARD" => s.cockpit_clipboard = Some(val.clone()),
            "SPIRA_LC_BIN" => s.lc_bin = Some(val.clone()),
            "SPIRA_MAIL_SETTLE_EVENT" => s.mail_settle_event = Some(val.clone()),
            "SPIRA_QUEUE_THROTTLE_OVERRIDE" => s.queue_throttle_override = Some(val.clone()),
            "SPIRA_CLIENT_SETTINGS" => s.client_settings = Some(val.clone()),
            "SPIRA_MAIL" => s.mail = Some(val.clone()),
            "SPIRA_MAIL_SESSION_MAILBOX" => s.mail_session_mailbox = Some(val.clone()),
            "SPIRA_REPO_MAP" => s.repo_map = Some(val.clone()),
            "SPIRA_PREFIX_MAP" => s.prefix_map = Some(val.clone()),
            "SPIRA_CHAMBER" => s.chamber = Some(val.clone()),
            "SPIRA_CHAMBER_OVERLAY" => s.chamber_overlay = Some(val.clone()),
            "SPIRA_OVERRIDES" => s.overrides = Some(val.clone()),
            "SPIRA_ID_PREFIX" => s.id_prefix = Some(val.clone()),
            "SPIRA_HEALTH_TIMEOUT" => s.health_timeout = Some(val.clone()),
            "SPIRA_WAKE" => s.wake = Some(val.clone()),
            "SPIRA_CTRL" => s.ctrl = Some(val.clone()),
            "SPIRA_MAIL_KINDS" => s.mail_kinds = Some(val.clone()),
            "SPIRA_MAIL_UNREAD_AGE" => s.mail_unread_age = Some(val.clone()),
            "SPIRA_MAIL_REPEAT_WINDOW" => s.mail_repeat_window = Some(val.clone()),
            "SPIRA_MAIL_TIDY_FRESH" => s.mail_tidy_fresh = Some(val.clone()),
            "SPIRA_MAIL_WAKE_BACKOFF" => s.mail_wake_backoff = Some(val.clone()),
            "SPIRA_MAIL_INDEX" => s.mail_index = Some(val.clone()),
            "SPIRA_COCKPIT_TRACE_LINES" => s.cockpit_trace_lines = Some(val.clone()),
            "SPIRA_SNAP_STALE_S" => s.snap_stale_s = Some(val.clone()),
            "SPIRA_NOTIFY" => s.notify = Some(val.clone()),
            "SPIRA_PANEL" => s.panel = Some(val.clone()),
            "SPIRA_VERIFY_TIMEOUT" => s.verify_timeout = Some(val.clone()),
            "SPIRA_RECLAIM_GRACE_SECS" => s.reclaim_grace_secs = Some(val.clone()),
            "SPIRA_OPERATED" => s.operated = Some(val.clone()),
            "SPIRA_CI_PARK_MAX" => s.ci_park_max = Some(val.clone()),
            "SPIRA_WORLD_STOP_LABEL" => s.world_stop_label = Some(val.clone()),
            "SPIRA_LAND_MAXSEC" => s.land_maxsec = Some(val.clone()),
            "SPIRA_LAND_GATE_RESERVE" => s.land_gate_reserve = Some(val.clone()),
            "SPIRA_CERTIFY_ALWAYS_COVERS" => s.certify_always_covers = Some(val.clone()),
            "SPIRA_REBASE_ESCALATE_AT" => s.rebase_escalate_at = Some(val.clone()),
            "SPIRA_EVICTION_ESCALATE_AT" => s.eviction_escalate_at = Some(val.clone()),
            "SPIRA_VERDICT_WINDOW" => s.verdict_window = Some(val.clone()),
            "SPIRA_CHECK5_MAX_FILE" => s.check5_max_file = Some(val.clone()),
            "SPIRA_CHECK5_MAX_RESOLVE" => s.check5_max_resolve = Some(val.clone()),
            "SPIRA_REMEDY_WINDOW" => s.remedy_window = Some(val.clone()),
            "SPIRA_PR_STALL_MINS" => s.pr_stall_mins = Some(val.clone()),
            "SPIRA_DEFERRAL_ESCALATE_AT" => s.deferral_escalate_at = Some(val.clone()),
            "SPIRA_BROKER_BIN" => s.broker_bin = Some(val.clone()),
            "SPIRA_BROKER_ENABLE" => s.broker_enable = Some(val.clone()),
            "SPIRA_BROKER_GH_CONFIG_DIR" => s.broker_gh_config_dir = Some(val.clone()),
            "SPIRA_BROKER_GH_TOKEN" => s.broker_gh_token = Some(val.clone()),
            "SPIRA_CZAR_PASS_BIN" => s.czar_pass_bin = Some(val.clone()),
            "SPIRA_QUEUE_WATCH_BIN" => s.queue_watch_bin = Some(val.clone()),
            "SPIRA_SUPERVISE_BIN" => s.supervise_bin = Some(val.clone()),
            "SPIRA_LANDING_PASS_BIN" => s.landing_pass_bin = Some(val.clone()),
            "SPIRA_TSD_BIN" => s.tsd_bin = Some(val.clone()),
            "SPIRA_TSD_LIFECYCLE_EXPORT_BIN" => s.tsd_lifecycle_export_bin = Some(val.clone()),
            "SPIRA_RECONCILER_BIN" => s.reconciler_bin = Some(val.clone()),
            "SPIRA_TEST_PLAN_BIN" => s.test_plan_bin = Some(val.clone()),
            "SPIRA_RECONCILER_FLOW_BIN" => s.reconciler_flow_bin = Some(val.clone()),
            "SPIRA_LOOM_CACHE_S" => s.loom_cache_s = Some(val.clone()),
            "SPIRA_LOOM_BIN" => s.loom_bin = Some(val.clone()),
            "SPIRA_LOOM_READY_GRACE" => s.loom_ready_grace = Some(val.clone()),
            "SPIRA_FLOW_WINDOW_HOURS" => s.flow_window_hours = Some(val.clone()),
            "SPIRA_FLOW_BASELINE_HOURS" => s.flow_baseline_hours = Some(val.clone()),
            "SPIRA_FLOW_GRACE_SECS" => s.flow_grace_secs = Some(val.clone()),
            "SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS" => s.flow_unobservable_grace_secs = Some(val.clone()),
            "SPIRA_DESIRED_DIR" => s.desired_dir = Some(val.clone()),
            "SPIRA_GH" => s.gh = Some(val.clone()),
            "SPIRA_GH_APP_CONFIG" => s.gh_app_config = Some(val.clone()),
            "SPIRA_GH_ASK_GRACE_SECS" => s.gh_ask_grace_secs = Some(val.clone()),
            "SPIRA_SPIKE_LABEL" => s.spike_label = Some(val.clone()),
            "SPIRA_SPIKE_DIR" => s.spike_dir = Some(val.clone()),
            "SPIRA_SPIKE_PATHS" => s.spike_paths = Some(val.clone()),
            "SPIRA_GROOMER_LABEL" => s.groomer_label = Some(val.clone()),
            "SPIRA_GROOM_THRESHOLD" => s.groom_threshold = Some(val.clone()),
            "SPIRA_GROOM_ASK_LABEL" => s.groom_ask_label = Some(val.clone()),
            "SPIRA_PLAN_LABEL" => s.plan_label = Some(val.clone()),
            "SPIRA_INCIDENT_LABEL" => s.incident_label = Some(val.clone()),
            "SPIRA_CZAR_LABEL" => s.czar_label = Some(val.clone()),
            "SPIRA_NO_LOOP_LABEL" => s.no_loop_label = Some(val.clone()),
            "SPIRA_EXPRESS_LABEL" => s.express_label = Some(val.clone()),
            "SPIRA_RECONCILER_LABEL" => s.reconciler_label = Some(val.clone()),
            "SPIRA_RECONCILER_GRACE_SECS" => s.reconciler_grace_secs = Some(val.clone()),
            "SPIRA_DISK_FLOOR_PCT" => s.disk_floor_pct = Some(val.clone()),
            "SPIRA_CZAR_STAGE_ATTRIBUTION_FAILED" => s.czar_stage_attribution_failed = Some(val.clone()),
            "SPIRA_CZAR_STAGE_SORT_FAILED" => s.czar_stage_sort_failed = Some(val.clone()),
            "SPIRA_CZAR_STAGE_LOOP_STALLED" => s.czar_stage_loop_stalled = Some(val.clone()),
            "SPIRA_CZAR_STAGE_CI_STALLED" => s.czar_stage_ci_stalled = Some(val.clone()),
            "SPIRA_CZAR_STAGE_STARVED" => s.czar_stage_starved = Some(val.clone()),
            "SPIRA_CZAR_STAGE_CI_RED" => s.czar_stage_ci_red = Some(val.clone()),
            "SPIRA_CZAR_STAGE_BASE_RED" => s.czar_stage_base_red = Some(val.clone()),
            "SPIRA_MAECHEN_LABEL" => s.maechen_label = Some(val.clone()),
            "SPIRA_MAECHEN_LANDING_INTERVAL" => s.maechen_landing_interval = Some(val.clone()),
            "SPIRA_MAECHEN_MAX_GAP_SECONDS" => s.maechen_max_gap_seconds = Some(val.clone()),
            "SPIRA_MAECHEN_MAX_BEADS" => s.maechen_max_beads = Some(val.clone()),
            "SPIRA_MAECHEN_REMEDY_LABEL" => s.maechen_remedy_label = Some(val.clone()),
            "SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S" => s.census_clock_skew_tolerance_s = Some(val.clone()),
            "COCKPIT_DB" => s.cockpit_db = Some(val.clone()),
            "COCKPIT_MAIL" => s.cockpit_mail = Some(val.clone()),
            "COCKPIT_MOUSE" => s.cockpit_mouse = Some(val.clone()),
            "COCKPIT_CWD" => s.cockpit_cwd = Some(val.clone()),
            "COCKPIT_SESSIONS" => s.cockpit_sessions = Some(val.clone()),
            "COCKPIT_HOST" => s.cockpit_host = Some(val.clone()),
            "SPIRA_TOWN" => s.town = Some(val.clone()),
            "SPIRA_MIRROR" => s.mirror = Some(val.clone()),
            "SPIRA_EXPORTER" => s.exporter = Some(val.clone()),
            "SPIRA_DESIGN" => s.design = Some(val.clone()),
            "SPIRA_VIEW_SESSION" => s.view_session = Some(val.clone()),
            "SPIRA_ALERT_GLOB" => s.alert_glob = Some(val.clone()),
            "SPIRA_BD_PIN" => s.bd_pin = Some(val.clone()),
            "SPIRA_BD_TAG" => s.bd_tag = Some(val.clone()),
            "SPIRA_LANES" => s.lanes = Some(val.clone()),
            "SPIRA_QA_DEPTH" => s.qa_depth = Some(val.clone()),
            "SPIRA_THRASH_STREAK_CAP" => s.thrash_streak_cap = Some(val.clone()),
            "SPIRA_RAPID_RECUR_THRESHOLD" => s.rapid_recur_threshold = Some(val.clone()),
            "SPIRA_BRIEF_KEEP_RECURRENCES" => s.brief_keep_recurrences = Some(val.clone()),
            "SPIRA_BRIEF_NOTES_MAX_CHARS" => s.brief_notes_max_chars = Some(val.clone()),
            "SPIRA_CLAIM_RETRIES" => s.claim_retries = Some(val.clone()),
            "SPIRA_CLAIM_RETRY_DELAY_S" => s.claim_retry_delay_s = Some(val.clone()),
            "SPIRA_TOKEN_WINDOW_H" => s.token_window_h = Some(val.clone()),
            "SPIRA_TOKEN_PROJECTS" => s.token_projects = Some(val.clone()),
            "SPIRA_CTX_WARN" => s.ctx_warn = Some(val.clone()),
            "SPIRA_CTX_HIGH" => s.ctx_high = Some(val.clone()),
            "SPIRA_CTX_LIMIT" => s.ctx_limit = Some(val.clone()),
            "SPIRA_ARCHIVE" => s.archive = Some(val.clone()),
            "SPIRA_ARCHIVIST_EVERY" => s.archivist_every = Some(val.clone()),
            "SPIRA_ARCHIVIST_IDLE" => s.archivist_idle = Some(val.clone()),
            "SPIRA_ARCHIVIST_MODEL" => s.archivist_model = Some(val.clone()),
            "SPIRA_ARCHIVIST_TIMEOUT" => s.archivist_timeout = Some(val.clone()),
            "SPIRA_ARCHIVIST_PER_PASS" => s.archivist_per_pass = Some(val.clone()),
            "SPIRA_ARCHIVIST_TIMEOUT_RETRIES" => s.archivist_timeout_retries = Some(val.clone()),
            "SPIRA_TESTDB_LIB" => s.testdb_lib = Some(val.clone()),
            "SPIRA_TESTDB_BD" => s.testdb_bd = Some(val.clone()),
            "SPIRA_TESTDB_DATA" => s.testdb_data = Some(val.clone()),
            "SPIRA_TESTDB_PORT" => s.testdb_port = Some(val.clone()),
            "SPIRA_TESTENV_REGISTRY" => s.testenv_registry = Some(val.clone()),
            "SPIRA_TESTENV_MAX_CONCURRENT" => s.testenv_max_concurrent = Some(val.clone()),
            "SPIRA_TESTENV_QUEUE_TIMEOUT" => s.testenv_queue_timeout = Some(val.clone()),
            "SPIRA_TESTENV_QUEUE_POLL" => s.testenv_queue_poll = Some(val.clone()),
            "SPIRA_GH_INTAKE_PRIORITY" => s.gh_intake_priority = Some(val.clone()),
            "SPIRA_GH_INTAKE_BEAD_REPO" => s.gh_intake_bead_repo = Some(val.clone()),
            "SPIRA_FLAKY_GH_REPO" => s.flaky_gh_repo = Some(val.clone()),
            "SPIRA_RELEASE_REPO" => s.release_repo = Some(val.clone()),
            "SPIRA_RELEASE_RUST_TOOLCHAIN" => s.release_rust_toolchain = Some(val.clone()),
            "SPIRA_GATE_TIMEOUT" => s.gate_timeout = Some(val.clone()),
            "SPIRA_GATE_BUDGET" => s.gate_budget = Some(val.clone()),
            "SPIRA_GATE_SELECT_CAP" => s.gate_select_cap = Some(val.clone()),
            "SPIRA_AEON_CPU_QUOTA" => s.aeon_cpu_quota = Some(val.clone()),
            "SPIRA_CUTOVER_ROUND_LABEL" => s.cutover_round_label = Some(val.clone()),
            "SPIRA_GATE_LOCK_WAIT" => s.gate_lock_wait = Some(val.clone()),
            "SPIRA_LAND_CPU_QUOTA" => s.land_cpu_quota = Some(val.clone()),
            "SPIRA_GATE_SUITES" => s.gate_suites = Some(val.clone()),
            "SPIRA_SUITE_STATE_FILE" => s.suite_state_file = Some(val.clone()),
            "SPIRA_SUITES_STATE" => s.suites_state = Some(val.clone()),
            "SPIRA_SUITE_TIMEOUT" => s.suite_timeout = Some(val.clone()),
            "SPIRA_TIER_BUDGET_T0_MS" => s.tier_budget_t0_ms = Some(val.clone()),
            "SPIRA_TIER_BUDGET_T1_MS" => s.tier_budget_t1_ms = Some(val.clone()),
            "SPIRA_TIER_BUDGET_T2_MS" => s.tier_budget_t2_ms = Some(val.clone()),
            "SPIRA_TIER_BUDGET_T3_MS" => s.tier_budget_t3_ms = Some(val.clone()),
            "SPIRA_TIER_BUDGET_WINDOW" => s.tier_budget_window = Some(val.clone()),
            "SPIRA_TIER_ALLOWLIST_MARGIN_PCT" => s.tier_allowlist_margin_pct = Some(val.clone()),
            "SPIRA_TIER_ALLOWLIST" => s.tier_allowlist = Some(val.clone()),
            "SPIRA_TIER_AREA_ALLOWLIST" => s.tier_area_allowlist = Some(val.clone()),
            "SPIRA_BATCH_MAXPAR_CEILING" => s.batch_maxpar_ceiling = Some(val.clone()),
            "SPIRA_BATCH_MEM_RESERVE_MIB" => s.batch_mem_reserve_mib = Some(val.clone()),
            "SPIRA_BATCH_MEM_AVAIL_MIB" => s.batch_mem_avail_mib = Some(val.clone()),
            "SPIRA_BATCH_PSI_THRESHOLD" => s.batch_psi_threshold = Some(val.clone()),
            "SPIRA_BATCH_ORPHAN_MIN_AGE" => s.batch_orphan_min_age = Some(val.clone()),
            "SPIRA_BATCH_BINS_TTL" => s.batch_bins_ttl = Some(val.clone()),
            "SPIRA_ATTRIBUTE_MAXPAR" => s.attribute_maxpar = Some(val.clone()),
            "SPIRA_BATCH_PEAK_WARN_FRAC" => s.batch_peak_warn_frac = Some(val.clone()),
            "SPIRA_BATCH_ARTIFACT_DAYS" => s.batch_artifact_days = Some(val.clone()),
            "SPIRA_BATCH_TAIL_LINES" => s.batch_tail_lines = Some(val.clone()),
            "SPIRA_SUITE_TIMES_LOG" => s.suite_times_log = Some(val.clone()),
            "SPIRA_BATCH_LEDGER" => s.batch_ledger = Some(val.clone()),
            "SPIRA_SUITES_PRIORITY" => s.suites_priority = Some(val.clone()),
            "SPIRA_SUITES_STALE" => s.suites_stale = Some(val.clone()),
            "SPIRA_SELF_TEST" => s.self_test = Some(val.clone()),
            "SPIRA_INCIDENT_PRIORITY" => s.incident_priority = Some(val.clone()),
            "SPIRA_WATCHER_INTERVAL_S" => s.watcher_interval_s = Some(val.clone()),
            "SPIRA_FLAKE_QUARANTINE_AT" => s.flake_quarantine_at = Some(val.clone()),
            "SPIRA_FLAKE_WINDOW" => s.flake_window = Some(val.clone()),
            "SPIRA_QUARANTINE_CLEAN_RUNS" => s.quarantine_clean_runs = Some(val.clone()),
            "SPIRA_QUARANTINE_MAX_AGE" => s.quarantine_max_age = Some(val.clone()),
            "SPIRA_QUEUE_CI_IDLE_SEC" => s.queue_ci_idle_sec = Some(val.clone()),
            "SPIRA_CI_QUEUED_MAX_SECS" => s.ci_queued_max_secs = Some(val.clone()),
            "SPIRA_LOOP_STALL_SECS" => s.loop_stall_secs = Some(val.clone()),
            "SPIRA_CI_RED_MAX_SECS" => s.ci_red_max_secs = Some(val.clone()),
            "SPIRA_BASE_CI_UNREADABLE_GRACE_SECS" => s.base_ci_unreadable_grace_secs = Some(val.clone()),
            "SPIRA_PREFLIGHT_WALL_SECS" => s.preflight_wall_secs = Some(val.clone()),
            "SPIRA_BATCHER_WALL_SECS" => s.batcher_wall_secs = Some(val.clone()),
            "SPIRA_PREFLIGHT_SUITE_MAX_SECS" => s.preflight_suite_max_secs = Some(val.clone()),
            "SPIRA_QUEUE_INFRA_RETRIES" => s.queue_infra_retries = Some(val.clone()),
            "SPIRA_QUEUE_STUCK_AGE" => s.queue_stuck_age = Some(val.clone()),
            "SPIRA_QUEUE_DIR" => s.queue_dir = Some(val.clone()),
            "SPIRA_FORGE" => s.forge = Some(val.clone()),
            "SPIRA_FORGE_REPO" => s.forge_repo = Some(val.clone()),
            "SPIRA_QUEUE_WAIT_LABEL" => s.queue_wait_label = Some(val.clone()),
            "SPIRA_QUEUE_ACTIONS_APP_ID" => s.queue_actions_app_id = Some(val.clone()),
            "SPIRA_QUEUE_BATCHER" => s.queue_batcher = Some(val.clone()),
            "SPIRA_BATCHER_BIN" => s.batcher_bin = Some(val.clone()),
            "SPIRA_BATCH_JUDGEMENT_LABEL" => s.batch_judgement_label = Some(val.clone()),
            "SPIRA_QUEUE_LOCK_WAIT" => s.queue_lock_wait = Some(val.clone()),
            "SPIRA_QUEUE_LOCK_STARVE_MAX" => s.queue_lock_starve_max = Some(val.clone()),
            "SPIRA_QUEUE_REPRO_CI_POLLSEC" => s.queue_repro_ci_pollsec = Some(val.clone()),
            "SPIRA_QUEUE_REPRO_CI_MAXSEC" => s.queue_repro_ci_maxsec = Some(val.clone()),
            "SPIRA_SUBMITTED_LABEL" => s.submitted_label = Some(val.clone()),
            "SPIRA_PUBLISH_REMOTE" => s.publish_remote = Some(val.clone()),
            "SPIRA_OPEN_CHILDREN_LABEL" => s.open_children_label = Some(val.clone()),
            "SPIRA_WORK_CLOSE_TYPES" => s.work_close_types = Some(val.clone()),
            "SPIRA_QUEUE_THROTTLE_DEPTH_AT" => s.queue_throttle_depth_at = Some(val.clone()),
            "SPIRA_QUEUE_THROTTLE_STALL_MINS" => s.queue_throttle_stall_mins = Some(val.clone()),
            "SPIRA_AURON_RESTARTS" => s.auron_restarts = Some(val.clone()),
            "SPIRA_AURON_RESTART_WINDOW" => s.auron_restart_window = Some(val.clone()),
            "SPIRA_INSTANCE" => s.instance = Some(val.clone()),
            "SPIRA_RELEASES" => s.releases = Some(val.clone()),
            "SPIRA_RELEASES_KEEP" => s.releases_keep = Some(val.clone()),
            "SPIRA_GH_REPO" => s.gh_repo = Some(val.clone()),
            "SPIRA_REVIEWER_MODEL" => s.reviewer_model = Some(val.clone()),
            "SPIRA_REVIEWER_VERDICTS" => s.reviewer_verdicts = Some(val.clone()),
            "SPIRA_REVIEWER_TIMEOUT" => s.reviewer_timeout = Some(val.clone()),
            "SPIRA_REVIEWER_DIFF_LIMIT" => s.reviewer_diff_limit = Some(val.clone()),
            "SPIRA_REVIEW_LABEL" => s.review_label = Some(val.clone()),
            "SPIRA_CAPACITY_PROBE_MODEL" => s.capacity_probe_model = Some(val.clone()),
            "SPIRA_CAPACITY_PROBE_INTERVAL" => s.capacity_probe_interval = Some(val.clone()),
            "SPIRA_CAPACITY_PROBE_WINDOW" => s.capacity_probe_window = Some(val.clone()),
            "SPIRA_CAPACITY_PROBE_TIMEOUT" => s.capacity_probe_timeout = Some(val.clone()),
            "SPIRA_SELF_WINDOW" => s.self_window = Some(val.clone()),
            "SPIRA_DELIVERS_CHECK_TIMEOUT" => s.delivers_check_timeout = Some(val.clone()),
            "SPIRA_CERT_WINDOW_MINS" => s.cert_window_mins = Some(val.clone()),
            "SPIRA_AGENT" => s.agent = Some(val.clone()),
            "SPIRA_STATUTE_CORE" => s.statute_core = Some(val.clone()),
            "SPIRA_GIT_NAME" => s.git_name = Some(val.clone()),
            "SPIRA_GIT_EMAIL" => s.git_email = Some(val.clone()),
            "SPIRA_GH_APP_ID" => s.gh_app_id = Some(val.clone()),
            "SPIRA_GH_APP_INSTALLATION_ID" => s.gh_app_installation_id = Some(val.clone()),
            "SPIRA_GH_APP_KEY" => s.gh_app_key = Some(val.clone()),
            "SPIRA_GH_APP_PRIVATE_KEY" => s.gh_app_private_key = Some(val.clone()),
            "SPIRA_PVE_ENV" => s.pve_env = Some(val.clone()),
            "SPIRA_WORKFLOW_ONLY_PATHS" => s.workflow_only_paths = Some(val.clone()),
            "SPIRA_GH_API" => s.gh_api = Some(val.clone()),
            other => errors.push(format!(
                "spira.conf: unknown key {other}, refused (not in conf.sh's SPIRA_CONF_KEYS \
                 or a typo — widen the schema in spira-config/src/convert.rs if this key is real)"
            )),
        }
    }
    if errors.is_empty() {
        Ok(s)
    } else {
        Err(errors)
    }
}

fn is_lane_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == ',' || c == '_' || c == '-')
}

fn expand_lane_mode(token: &str) -> Vec<Lane> {
    use Lane::*;
    match token {
        "consume" => vec![Plan],
        "develop" => vec![Plan, Incident, Groom, Spike],
        "self" => vec![Plan, Incident, Groom, Spike, MaechenSweep, CzarTrigger],
        _ => Vec::new(),
    }
}

fn parse_lane_token(tok: &str) -> Option<Lane> {
    Some(match tok {
        "plan" => Lane::Plan,
        "incident" => Lane::Incident,
        "groom" => Lane::Groom,
        // literal-ok: matches the repo-map lane token this old format actually writes
        "maechen-sweep" => Lane::MaechenSweep,
        "spike" => Lane::Spike,
        "czar-trigger" => Lane::CzarTrigger,
        _ => return None,
    })
}

/// One parsed `repo-map` row, before its `lanes` shorthand is expanded.
struct RepoRow {
    name: String,
    path: String,
    land: String,
    base: String,
    format: String,
    gate: String,
    lanes_raw: String,
}

/// Splits one non-comment `repo-map` line into its columns, using the same "is the last
/// field a lane list or the tail of the gate command" heuristic as `lib.sh`'s `repo_field`:
/// a lanes column is empty or matches `^[A-Za-z][A-Za-z0-9,_-]*$`; anything else (spaces,
/// `$`, `/`, `&&`, a bare `|` from a shell pipe) is gate, however many `|` splits it cost.
fn parse_repo_row(line: &str) -> Option<RepoRow> {
    let fields: Vec<&str> = line.split('|').collect();
    let nf = fields.len();
    if nf < 2 {
        return None;
    }
    let name = fields[0].trim();
    if name.is_empty() {
        return None;
    }
    let lanes_idx = if nf >= 7 {
        let t = fields[nf - 1].trim();
        if t.is_empty() || is_lane_ident(t) {
            Some(nf)
        } else {
            None
        }
    } else {
        None
    };
    let base = if nf >= 6 { fields[3].trim() } else { "" };
    let format = if nf >= 6 {
        fields[4].trim()
    } else if nf == 5 {
        fields[3].trim()
    } else {
        ""
    };
    let gate_start = if nf >= 6 {
        6
    } else if nf == 5 {
        5
    } else {
        4
    };
    let gate_end = lanes_idx.map(|li| li - 1).unwrap_or(nf);
    let gate = if gate_start <= gate_end && gate_start <= nf {
        fields[gate_start - 1..gate_end.min(nf)]
            .join("|")
            .trim()
            .to_string()
    } else {
        String::new()
    };
    let lanes_raw = lanes_idx
        .map(|li| fields[li - 1].trim().to_string())
        .unwrap_or_default();
    Some(RepoRow {
        name: name.to_string(),
        path: fields
            .get(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        land: fields
            .get(2)
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        base: base.to_string(),
        format: format.to_string(),
        gate,
        lanes_raw,
    })
}

/// Expands one row's raw `lanes` column, refusing an unrecognized mode word or lane label
/// instead of dropping it — the value's own source is where an agent that hallucinated a
/// token gets backpressure, not lib.sh's now-narrowed reading of an already-expanded list
/// (law-fail-closed-at-the-source). Empty means "no restriction": every lane.
fn parse_lanes(row_name: &str, raw: &str) -> Result<Vec<Lane>, String> {
    if raw.is_empty() {
        return Ok(vec![Lane::Plan]);
    }
    let expanded = expand_lane_mode(raw);
    if !expanded.is_empty() {
        return Ok(expanded);
    }
    let mut lanes = Vec::new();
    for tok in raw.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        match parse_lane_token(tok) {
            Some(l) => lanes.push(l),
            None => return Err(format!("repo-map: {row_name}: unknown lane {tok:?}")),
        }
    }
    Ok(lanes)
}

/// Parses `repo-map` text into `[repo.<name>]` tables. `Err` names every row whose `lanes`
/// column held an unrecognized mode word or lane label — collected across all rows, not just
/// the first, so a sweep of a bad map fixes every offender in one pass.
pub fn repo_sections(
    text: &str,
    warnings: &mut ConvertWarnings,
) -> Result<BTreeMap<String, RepoSection>, Vec<String>> {
    let mut out = BTreeMap::new();
    let mut errors = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some(row) = parse_repo_row(line) else {
            continue;
        };
        let mode = match row.land.as_str() {
            "push" => LandMode::Push,
            "pr" => LandMode::Pr,
            "hold" => LandMode::Hold,
            "queue" => LandMode::Queue,
            "queue.forge" => LandMode::QueueForge,
            "queue.local" => LandMode::QueueLocal,
            other => {
                warnings.push(format!(
                    "repo-map: {}: unknown land mode {other:?}",
                    row.name
                ));
                continue;
            }
        };
        let lanes = match parse_lanes(&row.name, &row.lanes_raw) {
            Ok(lanes) => lanes,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        out.insert(
            row.name.clone(),
            RepoSection {
                path: row.path,
                mode,
                base: if row.base.is_empty() {
                    None
                } else {
                    Some(row.base)
                },
                format: if row.format.is_empty() {
                    None
                } else {
                    Some(row.format)
                },
                gate: if row.gate.is_empty() {
                    None
                } else {
                    Some(row.gate)
                },
                lanes,
                forge: None,
            },
        );
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// Expands the small subset of shell parameter expansion the fayth files actually use:
/// `${NAME:+repl}` (repl, itself expanded, if NAME is set and non-empty; empty otherwise)
/// and bare `$NAME`. Not a shell interpreter — anything else passes through unexpanded.
fn shell_expand(input: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() && chars[i + 1] == '{' {
            if let Some(close) = chars[i..].iter().position(|&c| c == '}') {
                let inner: String = chars[i + 2..i + close].iter().collect();
                if let Some((name, repl)) = inner.split_once(":+") {
                    let set = vars.get(name).map(|v| !v.is_empty()).unwrap_or(false);
                    if set {
                        out.push_str(&shell_expand(repl, vars));
                    }
                } else {
                    out.push_str(vars.get(inner.as_str()).map(String::as_str).unwrap_or(""));
                }
                i += close + 1;
                continue;
            }
        }
        if chars[i] == '$' {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j > i + 1 {
                let name: String = chars[i + 1..j].iter().collect();
                out.push_str(vars.get(name.as_str()).map(String::as_str).unwrap_or(""));
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The `SPIRA_*_LABEL` defaults `conf.sh` derives when `[spira]` does not set them —
/// needed to resolve a fayth's `FAYTH_LABELS` expression even when the schema (deliberately
/// narrower than `conf.sh`'s full allowlist) carries only some of the label keys.
fn label_defaults() -> BTreeMap<String, String> {
    [
        // literal-ok: mirrors conf.sh's own derived default (this binary cannot source schema.sh)
        ("SPIRA_ASK_LABEL", "needs-operator"),
        // literal-ok: mirrors conf.sh's own derived default; see above
        ("SPIRA_CI_LABEL", "awaiting-ci"),
        ("SPIRA_SPIKE_LABEL", "spike"),
        ("SPIRA_GROOMER_LABEL", "groom"),
        // literal-ok: mirrors conf.sh's own derived defaults; see above
        ("SPIRA_MAECHEN_LABEL", "maechen-sweep"),
        ("SPIRA_PLAN_LABEL", "plan"),
        ("SPIRA_INCIDENT_LABEL", "incident"),
        ("SPIRA_CZAR_LABEL", "czar-trigger"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Parses one `chamber/*.fayth` file's `FAYTH_KEY=value` lines into `[persona.<name>]`.
/// `spira_vars` is `[spira]`'s own resolved values (e.g. `SPIRA_SCOPE_LABEL`), consulted by
/// [`shell_expand`] before falling back to [`label_defaults`].
pub fn persona_section(
    text: &str,
    spira: &SpiraSection,
    warnings: &mut ConvertWarnings,
) -> (String, PersonaSection) {
    let mut vars = label_defaults();
    if let Some(v) = &spira.scope_label {
        vars.insert("SPIRA_SCOPE_LABEL".to_string(), v.clone());
    }
    if let Some(v) = &spira.ask_label {
        vars.insert("SPIRA_ASK_LABEL".to_string(), v.clone());
    }
    if let Some(v) = &spira.ci_label {
        vars.insert("SPIRA_CI_LABEL".to_string(), v.clone());
    }

    let mut name = String::new();
    let mut model = String::new();
    let mut tools = Vec::new();
    let mut labels_raw = String::new();
    let mut lane = None;
    let mut lease_minutes = None;
    let mut heartbeat_seconds = None;
    let mut system_prompt = None;

    for raw_line in text.lines() {
        let line = raw_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.starts_with("FAYTH_") {
            continue;
        }
        let mut val = val.trim().to_string();
        if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
            val = val[1..val.len() - 1].to_string();
        }
        let val = shell_expand(&val, &vars);
        match key {
            "FAYTH_NAME" => name = val,
            "FAYTH_MODEL" => model = val,
            "FAYTH_TOOLS" => {
                tools = val
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "FAYTH_LABELS" => labels_raw = val,
            "FAYTH_LANE" => lane = Some(val),
            "FAYTH_LEASE_MINUTES" => {
                lease_minutes = val.parse().ok().or_else(|| {
                    warnings.push(format!(
                        "fayth: FAYTH_LEASE_MINUTES not an integer: {val:?}"
                    ));
                    None
                })
            }
            "FAYTH_HEARTBEAT_SECONDS" => {
                heartbeat_seconds = val.parse().ok().or_else(|| {
                    warnings.push(format!(
                        "fayth: FAYTH_HEARTBEAT_SECONDS not an integer: {val:?}"
                    ));
                    None
                })
            }
            "FAYTH_SYSTEM_PROMPT" => {
                system_prompt = match val.as_str() {
                    "append" => Some(crate::SystemPromptMode::Append),
                    "replace" => Some(crate::SystemPromptMode::Replace),
                    other => {
                        warnings.push(format!(
                            "fayth: FAYTH_SYSTEM_PROMPT not append/replace: {other:?}"
                        ));
                        None
                    }
                }
            }
            _ => {}
        }
    }

    let labels = labels_raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let lease = if lease_minutes.is_some() || heartbeat_seconds.is_some() {
        Some(Lease {
            minutes: lease_minutes,
            heartbeat_seconds,
        })
    } else {
        None
    };

    (
        name,
        PersonaSection {
            model,
            tools,
            labels,
            lane,
            lease,
            system_prompt,
        },
    )
}

/// Converts a full set of legacy inputs into one `SpiraToml`, plus every warning collected
/// along the way (an unknown `spira.conf` key, an unparseable repo-map row, ...). `Err` means
/// at least one repo-map row's `lanes` column named an unrecognized mode word or lane label —
/// refused rather than silently narrowed, on this path and every other caller of `convert`.
pub fn convert(
    conf_text: &str,
    home: &str,
    repo_map_text: &str,
    fayth_texts: &[(&str, &str)],
) -> Result<(SpiraToml, ConvertWarnings), Vec<String>> {
    let mut warnings = ConvertWarnings::default();
    let raw = read_conf(conf_text, home);
    let spira = spira_section(&raw, &mut warnings)?;
    let repo = repo_sections(repo_map_text, &mut warnings)?;
    let mut persona = BTreeMap::new();
    for (_path, text) in fayth_texts {
        let (name, section) = persona_section(text, &spira, &mut warnings);
        if name.is_empty() {
            warnings.push("fayth: no FAYTH_NAME, skipped".to_string());
            continue;
        }
        persona.insert(name, section);
    }
    Ok((
        SpiraToml {
            spira: Some(spira),
            repo,
            persona,
        },
        warnings,
    ))
}
