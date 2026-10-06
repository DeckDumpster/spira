//! Configuration (DESIGN.md §2.6). `$SPIRA_TOML` is the one source of config (per Ryan
//! 2026-10-05): every registered key below goes through `spira_config::process::cfg`/
//! `cfg_parse`, resolved once per process, no environment override, no Rust-side default
//! atop a declared value. A handful of fields are NOT registered config at all (test-only
//! knobs, per-invocation seams like `SPIRA_SYSTEMCTL`/`SPIRA_SUMMON`, or process-identity
//! like `BEADS_ACTOR`) — those still come from `Source::env`, exactly as before.

use std::path::PathBuf;

use crate::classify::Vocab;

#[derive(Debug, Clone)]
pub struct Config {
    pub db: Option<String>,
    pub run: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub bd: String,
    pub vocab: Vocab,
    pub ci_label: String,
    pub scope_label: String,
    pub no_loop_label: String,
    /// `SPIRA_MAX_AEONS` — Some(0) is the operator's deliberate "no aeons" (pool-paused).
    pub max_aeons: Option<u32>,
    pub max_live_aeons: u32,
    pub throttle_release_at: String,
    /// The spira-claim program: `spira-claim`, by name on the launcher's PATH (sp-gypjk). A
    /// field only so a unit test can hand in a recorder.
    pub claim_bin: String,
    pub instance: Option<String>,
    pub labels: Option<String>,
    pub exclude_labels: Option<String>,
    pub strand_grace: i64,
    pub event_cooldown: i64,
    pub bd_timeout: String,
    pub list_snapshot: Option<PathBuf>,
    pub systemctl: String,
    pub summon: String,
    pub capacity_pause: Option<PathBuf>,
    pub throttle_stamp: Option<PathBuf>,
    pub beads_actor: String,
    /// `SPIRA_INCIDENT_LABEL` (detect_incident_needs_builder, wave 4.29).
    pub incident_label: String,
    /// `SPIRA_GROOM_ASK_LABEL` (detect_livelocked's unmapped-repo category).
    pub groom_ask_label: String,
    /// `SPIRA_WORK_CLOSE_TYPES` (detect_landed_but_open / detect_closed_unlanded_states).
    pub work_close_types: String,
    /// `SPIRA_ID_PREFIX` (detect_invalid_closed's tracking-reference regex).
    pub id_prefix: String,
    /// `SPIRA_FAYTHS` — `aeons_live_lanes`' override onto `spira_lane_fayths`'s own chamber
    /// scan. Read here, once, so that pure probe logic never reads the environment itself.
    pub fayths_override: Option<String>,
}

/// The remaining, NOT-registered lookups `resolve` still makes: a test-only knob, a seam
/// name (`SPIRA_SYSTEMCTL`/`SPIRA_SUMMON`), or process identity (`BEADS_ACTOR`) — none of
/// these live in `spira/conf.d`, so none of them go through `spira_config::process::cfg`.
pub trait Source {
    fn env(&self, key: &str) -> Option<String>;
}

pub struct Live;

impl Live {
    pub fn load() -> Live {
        Live
    }
}

impl Source for Live {
    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[cfg(test)]
impl Config {
    /// A unit test's recorder in place of the spira-claim on PATH.
    pub fn with_claim(mut self, claim: &str) -> Config {
        self.claim_bin = claim.into();
        self
    }

    /// A fully-populated baseline `Config` for a test that needs *some* config to drive
    /// OTHER logic (detectors, probe) — never a route for exercising `resolve`'s own config
    /// read, which `spira_config::process::fixture_toml` + `SPIRA_TOML` covers instead (see
    /// `resolve_reads_every_registered_key_from_the_one_toml` below). Field-update syntax
    /// (`Config { field: x, ..Config::test_fixture() }`) tweaks just what a test cares
    /// about, the same way `Config`'s fields were varied through a fake `Source` before (per
    /// Ryan 2026-10-05: one source of config — a fake env/toml can no longer stand in for a
    /// registered key once `resolve` reads it through the process-wide `cfg` door).
    pub fn test_fixture() -> Config {
        Config {
            db: None,
            run: None,
            home: None,
            bd: "bd".into(),
            vocab: Vocab {
                ask: "needs-operator".into(),
                submitted: "spira-submitted".into(),
                queue_wait: "spira-queue-waiting".into(),
                open_children: "spira-open-children".into(),
                poison: "spira-poison".into(),
            },
            ci_label: "awaiting-ci".into(),
            scope_label: String::new(),
            no_loop_label: "no-loop".into(),
            max_aeons: Some(4),
            max_live_aeons: 0,
            throttle_release_at: "8".into(),
            claim_bin: "spira-claim".into(),
            instance: Some("prod".into()),
            labels: None,
            exclude_labels: None,
            strand_grace: 900,
            event_cooldown: 3600,
            bd_timeout: "180".into(),
            list_snapshot: None,
            systemctl: "systemctl".into(),
            summon: "systemd-run".into(),
            capacity_pause: None,
            throttle_stamp: None,
            beads_actor: "harness".into(),
            incident_label: "incident".into(),
            groom_ask_label: "groom-asked".into(),
            work_close_types: "task bug feature".into(),
            id_prefix: "sp".into(),
            fayths_override: None,
        }
    }
}

impl Config {
    /// Every REGISTERED key (`spira/conf.d`) goes through `spira_config::process::cfg`/
    /// `cfg_parse` — the declared value in `$SPIRA_TOML`, resolved once per process, no
    /// environment override, no literal Rust-side default standing in for an empty
    /// declaration (per Ryan 2026-10-05: one source of config). `Err` here means the config
    /// itself could not be resolved (or named a key `conf.d` does not register) — a named
    /// refusal, propagated to the caller, never a guessed value.
    ///
    /// `SPIRA_BD` is the one exception worth flagging: `spira/conf.d/SPIRA_BD` carries no
    /// default anywhere in `conf.sh` — it resolves empty unless some `spira.toml` sets it
    /// explicitly. Before this, strand's own Rust-side default of the literal `"bd"` was the
    /// ONLY place in the whole harness that ever supplied one. Deleting it means an install
    /// that never set `spira.bd` now refuses here instead of silently finding `bd` on PATH.
    pub fn resolve(src: &dyn Source) -> Result<Config, String> {
        use spira_config::process::cfg;

        let nonempty = |v: String| if v.is_empty() { None } else { Some(v) };
        let path = |env: &str| src.env(env).filter(|v| !v.is_empty()).map(PathBuf::from);
        let num_env = |env: &str, def: i64| src.env(env).and_then(|v| v.trim().parse().ok()).unwrap_or(def);
        let str_env = |env: &str, def: &str| src.env(env).filter(|v| !v.is_empty()).unwrap_or_else(|| def.to_string());

        let max_aeons_raw = cfg("SPIRA_MAX_AEONS")?;
        let max_aeons = if max_aeons_raw.trim().is_empty() {
            None
        } else {
            Some(
                max_aeons_raw
                    .trim()
                    .parse::<u32>()
                    .map_err(|e| format!("SPIRA_MAX_AEONS = {max_aeons_raw:?} in spira.toml does not parse: {e}"))?,
            )
        };
        let max_live_raw = cfg("SPIRA_MAX_LIVE_AEONS")?;
        let max_live_aeons = if max_live_raw.trim().is_empty() {
            0
        } else {
            max_live_raw
                .trim()
                .parse::<u32>()
                .map_err(|e| format!("SPIRA_MAX_LIVE_AEONS = {max_live_raw:?} in spira.toml does not parse: {e}"))?
        };

        let bd = cfg("SPIRA_BD")?;
        if bd.is_empty() {
            return Err(
                "SPIRA_BD is empty in spira.toml — set spira.bd to the bd binary this install uses (no default exists for this key)"
                    .to_string(),
            );
        }

        Ok(Config {
            db: nonempty(cfg("SPIRA_DB")?),
            run: nonempty(cfg("SPIRA_RUN")?).map(PathBuf::from),
            home: path("SPIRA_HOME"),
            bd,
            vocab: Vocab {
                ask: cfg("SPIRA_ASK_LABEL")?,
                submitted: cfg("SPIRA_SUBMITTED_LABEL")?,
                queue_wait: cfg("SPIRA_QUEUE_WAIT_LABEL")?,
                open_children: cfg("SPIRA_OPEN_CHILDREN_LABEL")?,
                poison: "spira-poison".into(),
            },
            ci_label: cfg("SPIRA_CI_LABEL")?,
            scope_label: cfg("SPIRA_SCOPE_LABEL")?,
            no_loop_label: cfg("SPIRA_NO_LOOP_LABEL")?,
            max_aeons,
            max_live_aeons,
            throttle_release_at: cfg("SPIRA_QUEUE_THROTTLE_RELEASE_AT")?,
            claim_bin: "spira-claim".into(),
            instance: nonempty(cfg("SPIRA_INSTANCE")?),
            labels: src.env("SPIRA_LABELS").filter(|v| !v.is_empty()),
            exclude_labels: src.env("SPIRA_EXCLUDE_LABELS").filter(|v| !v.is_empty()),
            strand_grace: num_env("SPIRA_STRAND_GRACE", 900),
            event_cooldown: num_env("SPIRA_EVENT_COOLDOWN", 3600),
            bd_timeout: str_env("BD_TIMEOUT", "180"),
            list_snapshot: path("SPIRA_LIST_SNAPSHOT"),
            systemctl: str_env("SPIRA_SYSTEMCTL", "systemctl"),
            summon: str_env("SPIRA_SUMMON", "systemd-run"),
            capacity_pause: path("SPIRA_CAPACITY_PAUSE"),
            throttle_stamp: path("SPIRA_THROTTLE_STAMP"),
            beads_actor: str_env("BEADS_ACTOR", "harness"),
            incident_label: cfg("SPIRA_INCIDENT_LABEL")?,
            groom_ask_label: cfg("SPIRA_GROOM_ASK_LABEL")?,
            work_close_types: cfg("SPIRA_WORK_CLOSE_TYPES")?,
            id_prefix: cfg("SPIRA_ID_PREFIX")?,
            fayths_override: nonempty(cfg("SPIRA_FAYTHS")?),
        })
    }

    /// The exclusions a partition whose fayth declares none falls back to.
    pub fn exclude_default(&self) -> String {
        format!("spira-poison,{},{}", self.vocab.ask, self.ci_label)
    }

    /// Labels every "is this claimable" predicate excludes (lib.sh ready_shared_exclude).
    pub fn shared_exclude(&self) -> Vec<String> {
        [&self.vocab.queue_wait, &self.vocab.submitted, &self.vocab.open_children]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect()
    }

    pub fn state_path(&self) -> Option<PathBuf> {
        self.run.as_ref().map(|r| r.join("strands.json"))
    }
    pub fn sentinel_log(&self) -> Option<PathBuf> {
        self.run.as_ref().map(|r| r.join("sentinel.log"))
    }
    pub fn capacity_pause_path(&self) -> Option<PathBuf> {
        self.capacity_pause.clone().or_else(|| self.run.as_ref().map(|r| r.join("capacity-pause")))
    }
    pub fn throttle_stamp_path(&self) -> Option<PathBuf> {
        self.throttle_stamp.clone().or_else(|| self.run.as_ref().map(|r| r.join("queue-throttled")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoEnv;
    impl Source for NoEnv {
        fn env(&self, _: &str) -> Option<String> {
            None
        }
    }

    /// The one test in this crate allowed to exercise `resolve`'s own top-level config read
    /// (per Ryan 2026-10-05: one source of config — every other test in strand drives its
    /// logic off `Config::test_fixture()` instead, never off a fake env/toml, because
    /// `spira_config::process::cfg`'s resolution is cached once per TEST BINARY PROCESS: a
    /// second fixture here would not see its own `SPIRA_TOML`, it would see whichever ran
    /// first).
    #[test]
    fn resolve_reads_every_registered_key_from_the_one_toml() {
        let dir = testkit::TempDir::new("strand-config-resolve");
        let toml = spira_config::process::fixture_toml(
            &dir,
            &[
                ("SPIRA_ASK_LABEL", "ask-x"),
                ("SPIRA_DB", "/db"),
                ("SPIRA_RUN", "/r"),
                ("SPIRA_BD", "bd"),
                ("SPIRA_MAX_AEONS", "0"),
                ("SPIRA_SCOPE_LABEL", ""),
                ("SPIRA_QUEUE_THROTTLE_RELEASE_AT", "12"),
            ],
        );
        // `cfg`'s registry check needs the REAL `spira/conf.d` (every key this test reads
        // must actually be registered there) — unlike a fixture home with an empty conf.d,
        // this has to be the checkout's own, so it is found relative to this crate's own
        // manifest, never by trusting an ancestor search from the test binary's path.
        let real_home = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let c = {
            let _env = testkit::env(&[("SPIRA_HOME", Some(real_home.to_str().unwrap())), ("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
            Config::resolve(&NoEnv).unwrap()
        };

        assert_eq!(c.incident_label, "incident", "the complete fixture's own declared default");
        assert_eq!(c.groom_ask_label, "groom-asked");
        assert_eq!(c.work_close_types, "task bug feature");
        assert_eq!(c.id_prefix, "sp");
        assert_eq!(c.vocab.ask, "ask-x");
        assert_eq!(c.run, Some(PathBuf::from("/r")));
        assert_eq!(c.db.as_deref(), Some("/db"));
        assert_eq!(c.max_aeons, Some(0));
        assert_eq!(c.scope_label, "", "an explicitly empty scope disables it — still Ok, not a refusal");
        assert_eq!(c.throttle_release_at, "12");
        assert_eq!(c.vocab.submitted, "spira-submitted");
        assert_eq!(c.exclude_default(), format!("spira-poison,ask-x,{}", c.ci_label));
    }
}
