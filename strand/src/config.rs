//! Configuration (DESIGN.md §2.6): process environment → spira-config's `[spira]` table →
//! the conf.sh default. strand never reads spira.toml itself; spira-config does.

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
    /// The spira-lc program: `spira-lc`, by name on the launcher's PATH (sp-gypjk). A field
    /// only so a unit test can hand in a recorder.
    pub lc_bin: String,
    /// THE lifecycle switch (DESIGN.md §9): `SPIRA_LIFECYCLE_ENFORCE`, else
    /// `spira.lifecycle_enforce`, else off. Off, strand never runs `lc_bin`; `lc_bin`'s
    /// presence or absence is never consulted to decide this.
    pub lifecycle_enforce: bool,
    pub instance: Option<String>,
    pub labels: Option<String>,
    pub exclude_labels: Option<String>,
    pub ghost_grace: i64,
    pub strand_grace: i64,
    pub reclaim_at: i64,
    pub event_cooldown: i64,
    pub bd_timeout: String,
    pub list_snapshot: Option<PathBuf>,
    pub ready_snapshot: Option<PathBuf>,
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
}

/// A lookup source: `env(key)` and `toml(field)`. Split out so tests can supply both.
pub trait Source {
    fn env(&self, key: &str) -> Option<String>;
    fn toml(&self, field: &str) -> Option<String>;
}

pub struct Live {
    doc: Option<spira_config::SpiraToml>,
}

impl Live {
    pub fn load() -> Live {
        let doc = spira_config::discover(None).and_then(|p| spira_config::load(&p).ok());
        Live { doc }
    }
}

impl Source for Live {
    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
    fn toml(&self, field: &str) -> Option<String> {
        spira_config::get_path(self.doc.as_ref()?, &format!("spira.{field}"))
    }
}

#[cfg(test)]
impl Config {
    /// A unit test's recorder in place of the spira-lc on PATH.
    pub fn with_lc(mut self, lc: &str) -> Config {
        self.lc_bin = lc.into();
        self
    }
}

impl Config {
    pub fn resolve(src: &dyn Source) -> Config {
        // env set (even to "") wins; else toml; else default. An explicitly empty env value
        // is a declaration (conf.sh: SPIRA_SCOPE_LABEL="" disables scope restriction).
        let get = |env: &str, field: &str| src.env(env).or_else(|| if field.is_empty() { None } else { src.toml(field) });
        let nonempty = |v: Option<String>| v.filter(|s| !s.is_empty());
        let or = |env: &str, field: &str, def: &str| get(env, field).unwrap_or_else(|| def.to_string());
        let num = |env: &str, def: i64| src.env(env).and_then(|v| v.trim().parse().ok()).unwrap_or(def);
        let path = |env: &str| nonempty(src.env(env)).map(PathBuf::from);

        let scope_label = match src.env("SPIRA_SCOPE_LABEL") {
            Some(v) => v,
            None => src.toml("scope_label").or_else(|| src.toml("home_repo")).unwrap_or_default(),
        };
        Config {
            db: nonempty(get("SPIRA_DB", "db")),
            run: nonempty(get("SPIRA_RUN", "run")).map(PathBuf::from),
            home: nonempty(get("SPIRA_HOME", "prod")).map(PathBuf::from),
            bd: nonempty(get("SPIRA_BD", "bd")).unwrap_or_else(|| "bd".into()),
            vocab: Vocab {
                // literal-ok: conf.sh's own default; this binary cannot source schema.sh
                ask: or("SPIRA_ASK_LABEL", "ask_label", "needs-operator"),
                submitted: or("SPIRA_SUBMITTED_LABEL", "submitted_label", "spira-submitted"),
                queue_wait: or("SPIRA_QUEUE_WAIT_LABEL", "queue_wait_label", "spira-queue-waiting"),
                open_children: or("SPIRA_OPEN_CHILDREN_LABEL", "open_children_label", "spira-open-children"),
                poison: "spira-poison".into(),
            },
            // literal-ok: conf.sh's own default; this binary cannot source schema.sh
            ci_label: or("SPIRA_CI_LABEL", "ci_label", "awaiting-ci"),
            scope_label,
            // literal-ok: conf.sh's own default; this binary cannot source schema.sh
            no_loop_label: or("SPIRA_NO_LOOP_LABEL", "no_loop_label", "no-loop"),
            max_aeons: get("SPIRA_MAX_AEONS", "max_aeons").and_then(|v| v.trim().parse().ok()),
            max_live_aeons: get("SPIRA_MAX_LIVE_AEONS", "max_live_aeons")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0),
            throttle_release_at: nonempty(get("SPIRA_QUEUE_THROTTLE_RELEASE_AT", "queue_throttle_release_at"))
                .unwrap_or_else(|| "8".into()),
            lc_bin: "spira-lc".into(),
            lifecycle_enforce: spira_config::resolve_lifecycle_enforce(
                src.env(spira_config::LIFECYCLE_ENFORCE_ENV).as_deref(),
                src.toml("lifecycle_enforce").map(|v| v == "true"),
            ),
            // conf.sh: an unset SPIRA_INSTANCE is identical to SPIRA_INSTANCE=prod.
            instance: Some(nonempty(get("SPIRA_INSTANCE", "instance")).unwrap_or_else(|| "prod".into())),
            labels: nonempty(src.env("SPIRA_LABELS")),
            exclude_labels: nonempty(src.env("SPIRA_EXCLUDE_LABELS")),
            ghost_grace: num("SPIRA_GHOST_GRACE", 300),
            strand_grace: num("SPIRA_STRAND_GRACE", 900),
            reclaim_at: num("SPIRA_RECLAIM_AT", 5),
            event_cooldown: num("SPIRA_EVENT_COOLDOWN", 3600),
            bd_timeout: nonempty(src.env("BD_TIMEOUT")).unwrap_or_else(|| "180".into()),
            list_snapshot: path("SPIRA_LIST_SNAPSHOT"),
            ready_snapshot: path("SPIRA_READY_SNAPSHOT"),
            systemctl: nonempty(src.env("SPIRA_SYSTEMCTL")).unwrap_or_else(|| "systemctl".into()),
            summon: nonempty(src.env("SPIRA_SUMMON")).unwrap_or_else(|| "systemd-run".into()),
            capacity_pause: path("SPIRA_CAPACITY_PAUSE"),
            throttle_stamp: path("SPIRA_THROTTLE_STAMP"),
            beads_actor: nonempty(src.env("BEADS_ACTOR")).unwrap_or_else(|| "harness".into()),
            // literal-ok: conf.sh's own derived default; this binary cannot source schema.sh
            incident_label: or("SPIRA_INCIDENT_LABEL", "incident_label", "incident"),
            groom_ask_label: or("SPIRA_GROOM_ASK_LABEL", "groom_ask_label", "groom-asked"),
            work_close_types: or("SPIRA_WORK_CLOSE_TYPES", "work_close_types", "task bug feature"),
            id_prefix: or("SPIRA_ID_PREFIX", "id_prefix", "sp"),
        }
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
    use std::collections::HashMap;

    struct Fake {
        env: HashMap<&'static str, &'static str>,
        toml: HashMap<&'static str, &'static str>,
    }
    impl Source for Fake {
        fn env(&self, k: &str) -> Option<String> {
            self.env.get(k).map(|s| s.to_string())
        }
        fn toml(&self, f: &str) -> Option<String> {
            self.toml.get(f).map(|s| s.to_string())
        }
    }

    #[test]
    fn env_then_toml_then_default() {
        let f = Fake {
            env: HashMap::from([("SPIRA_ASK_LABEL", "ask-x"), ("SPIRA_SCOPE_LABEL", "")]),
            toml: HashMap::from([
                ("ask_label", "ignored"),
                ("run", "/r"),
                ("db", "/db"),
                ("max_aeons", "0"),
                ("home_repo", "spira"),
                ("queue_throttle_release_at", "12"),
            ]),
        };
        let c = Config::resolve(&f);
        assert_eq!(c.incident_label, "incident", "default when neither env nor toml sets it");
        assert_eq!(c.groom_ask_label, "groom-asked");
        assert_eq!(c.work_close_types, "task bug feature");
        assert_eq!(c.id_prefix, "sp");
        assert_eq!(c.vocab.ask, "ask-x");
        assert_eq!(c.run, Some(PathBuf::from("/r")));
        assert_eq!(c.db.as_deref(), Some("/db"));
        assert_eq!(c.max_aeons, Some(0));
        assert_eq!(c.scope_label, "", "an explicitly empty scope disables it");
        assert_eq!(c.throttle_release_at, "12");
        assert_eq!(c.vocab.submitted, "spira-submitted");
        assert_eq!(c.exclude_default(), format!("spira-poison,ask-x,{}", c.ci_label));
        let f = Fake { env: HashMap::new(), toml: HashMap::from([("home_repo", "spira")]) };
        assert_eq!(Config::resolve(&f).scope_label, "spira");
        assert_eq!(Config::resolve(&f).max_aeons, None);
    }

    #[test]
    fn lifecycle_switch_env_then_toml_then_off() {
        let r = |env: &[(&'static str, &'static str)], toml: &[(&'static str, &'static str)]| {
            Config::resolve(&Fake { env: env.iter().copied().collect(), toml: toml.iter().copied().collect() }).lifecycle_enforce
        };
        assert!(!r(&[], &[]), "default off");
        assert!(r(&[], &[("lifecycle_enforce", "true")]));
        assert!(!r(&[("SPIRA_LIFECYCLE_ENFORCE", "0")], &[("lifecycle_enforce", "true")]), "the environment wins");
        assert!(!r(&[("SPIRA_LIFECYCLE_ENFORCE", "")], &[("lifecycle_enforce", "true")]), "set-but-empty is off");
        assert!(r(&[("SPIRA_LIFECYCLE_ENFORCE", "1")], &[]));
        assert!(r(&[("SPIRA_LIFECYCLE_ENFORCE", "true")], &[("lifecycle_enforce", "false")]));
    }
}
