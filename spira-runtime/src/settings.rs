//! Read once at start, then never again: a running service whose config file changes keeps the
//! values it started with until it is restarted.

use std::path::PathBuf;

use spira_config::process::cfg;

use crate::watchers::inbox_keeper::InboxKeeper;
use crate::forge::Forge;
use crate::watchers::pr_notify::PrNotify;
use crate::watchers::asks::Asks;
use crate::watchers::design_asks::DesignAsks;
use crate::watchers::brain_dirty::BrainDirty;
use crate::watchers::supersede_duty::SupersedeDuty;
use crate::watchers::bead_sweep::BeadSweep;
use crate::watchers::publish_backlog::PublishBacklog;
use crate::watchers::repo_events::RepoEvents;
use crate::watchers::round_duty::RoundDuty;
use crate::watchers::view::View;
use crate::world::World;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub heartbeat_url: String,
    pub heartbeat_secs: u64,
    pub stall_limit: u32,
    pub metrics_endpoint: Option<String>,
    pub run_dir: PathBuf,
    pub lc_socket: PathBuf,
    pub round_duty: RoundDuty,
    pub inbox_keeper: InboxKeeper,
    pub pr_notify: PrNotify,
    pub forge: Forge,
    pub asks: Asks,
    pub design_asks: DesignAsks,
    pub brain_dirty: BrainDirty,
    pub supersede_duty: SupersedeDuty,
    pub bead_sweep: BeadSweep,
    pub publish_backlog: PublishBacklog,
    pub repo_events: RepoEvents,
    pub view: View,
    pub world: World,
}

const DEFAULT_FORGET_SECS: u64 = 86_400;

fn num<T: std::str::FromStr + PartialOrd + Default>(key: &str, raw: &str) -> Result<T, String> {
    raw.trim().parse::<T>().ok().filter(|v| *v > T::default()).ok_or_else(|| format!("{key} must be a positive integer, got {raw:?}"))
}

impl Settings {
    /// Every key `from_lookup` reads; each must be registered in spira/conf.d.
    pub const KEYS: &'static [&'static str] = &[
        "SPIRA_LC_SOCKET",
        "SPIRA_RUN",
        "SPIRA_RUNTIME_ROUND_DUTY_SECS",
        "SPIRA_ROUND_MIN",
        "SPIRA_ROUND_MAX_OPEN",
        "SPIRA_ROUND_DUE_REMIND",
        "SPIRA_ROUND_IDLE_CUT",
        "SPIRA_QUEUE_DIR",
        "SPIRA_REPO_MAP",
        "SPIRA_HOME_REPO",
        "SPIRA_CONCIERGE_INBOX",
        "SPIRA_RUNTIME_HEARTBEAT_URL",
        "SPIRA_RUNTIME_HEARTBEAT_SECS",
        "SPIRA_RUNTIME_STALL_LIMIT",
        "SPIRA_METRICS_ENDPOINT",
        "SPIRA_RUNTIME_DESIGN_ASKS_SECS",
        "SPIRA_DESIGN_ASKS_REPEAT",
        "SPIRA_RUNTIME_BRAIN_DIRTY_SECS",
        "SPIRA_BRAIN_DIRTY_AGE",
        "SPIRA_WIKI",
        "SPIRA_RUNTIME_SUPERSEDE_DUTY_SECS",
        "SPIRA_CONCIERGE_INBOX_STALL",
        "SPIRA_CONCIERGE_INBOX_BACKOFF",
        "SPIRA_RUNTIME_PR_NOTIFY_SECS",
        "SPIRA_PR_DEPLOY_GIVEUP",
        "SPIRA_GH_API",
        "SPIRA_BROKER_GH_TOKEN",
        "SPIRA_RUNTIME_ASKS_SECS",
        "SPIRA_RUNTIME_BEAD_SWEEP_SECS",
        "SPIRA_BEAD_SWEEP_HOLD_MIN",
        "SPIRA_BEAD_SWEEP_REWORK_MIN",
        "SPIRA_BEAD_SWEEP_CERT_MIN",
        "SPIRA_BEAD_SWEEP_WAIT_MIN",
        "SPIRA_BEAD_SWEEP_BATCH_MIN",
        "SPIRA_SCOPED_STOP_REVIEW_SECS",
        "SPIRA_MAIL",
        "SPIRA_ASK_LABEL",
        "SPIRA_RUNTIME_PUBLISH_BACKLOG_SECS",
        "SPIRA_LOCAL_BACKLOG_COUNT",
        "SPIRA_LOCAL_BACKLOG_AGE",
        "SPIRA_RUNTIME_REPO_EVENTS_SECS",
        "SPIRA_VIEW",
        "SPIRA_VIEW_SESSION",
        "SPIRA_RUNTIME_VIEW_SECS",
    ];

    pub fn load() -> Result<Settings, String> {
        Self::from_lookup(&cfg)?.with_environment(&|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    /// The cockpit's own state and hunk's runtime directory are where those programs put them,
    /// which only the service's environment says.
    pub fn with_environment(mut self, env: &dyn Fn(&str) -> Option<String>) -> Result<Settings, String> {
        let path = |k: &str| env(k).map(PathBuf::from);
        self.world.hunk_dir = path("XDG_RUNTIME_DIR").map(|d| d.join("hunk-mcp")).unwrap_or_default();
        self.world.cockpit_state = path("COCKPIT_STATE").or_else(|| path("HOME").map(|h| h.join(".local/state/cockpit"))).unwrap_or_default();
        if let Some(secs) = env("COCKPIT_FORGET_SECS") {
            self.view.forget_secs = num("COCKPIT_FORGET_SECS", &secs)?;
        }
        Ok(self)
    }

    pub fn from_lookup(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Settings, String> {
        let unresolved: Vec<&str> = Self::KEYS.iter().copied().filter(|k| get(k).is_err()).collect();
        if !unresolved.is_empty() {
            return Err(format!("unregistered or unresolved config keys: {} — refusing", unresolved.join(", ")));
        }
        let socket = get("SPIRA_LC_SOCKET")?;
        if socket.is_empty() {
            return Err("SPIRA_LC_SOCKET is empty — the journal has no door to the store".into());
        }
        let run_dir = PathBuf::from(get("SPIRA_RUN")?);
        let secs = |key: &str| -> Result<u64, String> { num(key, &get(key)?) };
        let round_duty = RoundDuty {
            interval_ms: secs("SPIRA_RUNTIME_ROUND_DUTY_SECS")? * 1000,
            min: secs("SPIRA_ROUND_MIN")?,
            max_open: secs("SPIRA_ROUND_MAX_OPEN")?,
            remind_secs: secs("SPIRA_ROUND_DUE_REMIND")?,
            idle_cut_secs: secs("SPIRA_ROUND_IDLE_CUT")?,
        };
        let home_repo = get("SPIRA_HOME_REPO")?;
        if home_repo.is_empty() {
            return Err("SPIRA_HOME_REPO is empty — round-duty cannot tell which repo's rounds are its own".into());
        }
        let inbox_keeper = InboxKeeper {
            interval_ms: secs("SPIRA_RUNTIME_INBOX_KEEPER_SECS")? * 1000,
            stall_secs: secs("SPIRA_CONCIERGE_INBOX_STALL")?,
            backoff_secs: secs("SPIRA_CONCIERGE_INBOX_BACKOFF")?,
        };
        let pr_notify = PrNotify { interval_ms: secs("SPIRA_RUNTIME_PR_NOTIFY_SECS")? * 1000, deploy_giveup_secs: secs("SPIRA_PR_DEPLOY_GIVEUP")? };
        let forge = Forge { api: get("SPIRA_GH_API")?, token: get("SPIRA_BROKER_GH_TOKEN")? };
        let asks = Asks { interval_ms: secs("SPIRA_RUNTIME_ASKS_SECS")? * 1000 };
        let design_asks = DesignAsks { interval_ms: secs("SPIRA_RUNTIME_DESIGN_ASKS_SECS")? * 1000, repeat_secs: secs("SPIRA_DESIGN_ASKS_REPEAT")? };
        let brain_dirty = BrainDirty { interval_ms: secs("SPIRA_RUNTIME_BRAIN_DIRTY_SECS")? * 1000, age_secs: secs("SPIRA_BRAIN_DIRTY_AGE")? };
        let supersede_duty = SupersedeDuty { interval_ms: secs("SPIRA_RUNTIME_SUPERSEDE_DUTY_SECS")? * 1000 };
        let bead_sweep = BeadSweep {
            interval_ms: secs("SPIRA_RUNTIME_BEAD_SWEEP_SECS")? * 1000,
            ask_label: get("SPIRA_ASK_LABEL")?,
            home_repo: home_repo.clone(),
            hold_min: secs("SPIRA_BEAD_SWEEP_HOLD_MIN")?,
            rework_min: secs("SPIRA_BEAD_SWEEP_REWORK_MIN")?,
            cert_min: secs("SPIRA_BEAD_SWEEP_CERT_MIN")?,
            wait_min: secs("SPIRA_BEAD_SWEEP_WAIT_MIN")?,
            batch_min: secs("SPIRA_BEAD_SWEEP_BATCH_MIN")?,
            stop_review_secs: secs("SPIRA_SCOPED_STOP_REVIEW_SECS")?,
        };
        let publish_backlog = PublishBacklog {
            interval_ms: secs("SPIRA_RUNTIME_PUBLISH_BACKLOG_SECS")? * 1000,
            max_count: get("SPIRA_LOCAL_BACKLOG_COUNT")?.trim().parse().map_err(|_| "SPIRA_LOCAL_BACKLOG_COUNT must be a non-negative integer".to_string())?,
            max_age_secs: get("SPIRA_LOCAL_BACKLOG_AGE")?.trim().parse().map_err(|_| "SPIRA_LOCAL_BACKLOG_AGE must be a non-negative integer".to_string())?,
        };
        let repo_events = RepoEvents { interval_ms: secs("SPIRA_RUNTIME_REPO_EVENTS_SECS")? * 1000 };
        let view_program = get("SPIRA_VIEW")?;
        let view_session = get("SPIRA_VIEW_SESSION")?;
        if !view_program.trim().is_empty() && view_session.trim().is_empty() {
            return Err("SPIRA_VIEW_SESSION is empty — the view follower has no session to steer".into());
        }
        let view = View { interval_ms: secs("SPIRA_RUNTIME_VIEW_SECS")? * 1000, forget_secs: DEFAULT_FORGET_SECS, enabled: !view_program.trim().is_empty() };
        let world = World {
            cockpit_session: view_session,
            tmux: PathBuf::from("tmux"),
            proc_dir: PathBuf::from("/proc"),
            queue_dir: PathBuf::from(get("SPIRA_QUEUE_DIR")?),
            repo_map: PathBuf::from(get("SPIRA_REPO_MAP")?),
            home_repo,
            inbox: PathBuf::from(get("SPIRA_CONCIERGE_INBOX")?),
            lc_socket: PathBuf::from(&socket),
            mail: PathBuf::from(get("SPIRA_MAIL")?),
            ask_label: get("SPIRA_ASK_LABEL")?,
            wiki: PathBuf::from(get("SPIRA_WIKI")?),
            run_dir: run_dir.clone(),
            ..World::default()
        };
        Ok(Settings {
            round_duty,
            inbox_keeper,
            pr_notify,
            forge,
            asks,
            design_asks,
            brain_dirty,
            supersede_duty,
            bead_sweep,
            publish_backlog,
            repo_events,
            view,
            world,
            heartbeat_url: get("SPIRA_RUNTIME_HEARTBEAT_URL")?,
            heartbeat_secs: num("SPIRA_RUNTIME_HEARTBEAT_SECS", &get("SPIRA_RUNTIME_HEARTBEAT_SECS")?)?,
            stall_limit: num("SPIRA_RUNTIME_STALL_LIMIT", &get("SPIRA_RUNTIME_STALL_LIMIT")?)?,
            metrics_endpoint: Some(get("SPIRA_METRICS_ENDPOINT")?).filter(|e| !e.trim().is_empty()),
            run_dir,
            lc_socket: PathBuf::from(socket),
        })
    }

    /// A ping must finish well inside one beat.
    pub fn ping_deadline_ms(&self) -> u64 {
        (self.heartbeat_secs * 1000 / 2).clamp(500, 10_000)
    }

    pub fn marker_path(&self) -> PathBuf {
        self.run_dir.join("runtime-inflight.json")
    }

    pub fn otel_dir(&self) -> PathBuf {
        spira_otel::dir_of(&self.run_dir)
    }

    /// FNV-1a over the pinned values, so two records with one hash ran on one configuration.
    pub fn hash(&self) -> String {
        let text = format!("{}\n{}\n{}\n{:?}\n{}\n{}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}", self.heartbeat_url, self.heartbeat_secs, self.stall_limit, self.metrics_endpoint, self.run_dir.display(), self.lc_socket.display(), self.round_duty, self.inbox_keeper, self.pr_notify, self.forge, self.asks, self.design_asks, self.brain_dirty, self.supersede_duty, self.bead_sweep, self.publish_backlog, self.repo_events, self.view, self.world);
        let h = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
        format!("{h:016x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(secs: &'static str) -> impl Fn(&str) -> Result<String, String> {
        move |k| {
            Ok(match k {
                "SPIRA_RUNTIME_HEARTBEAT_URL" => "http://127.0.0.1:9/ping",
                "SPIRA_RUNTIME_HEARTBEAT_SECS" => secs,
                "SPIRA_RUNTIME_STALL_LIMIT" => "3",
                "SPIRA_METRICS_ENDPOINT" => "127.0.0.1:9",
                "SPIRA_RUN" => "/r",
                "SPIRA_LC_SOCKET" => "/d/sock",
                "SPIRA_GH_API" => "http://forge",
                "SPIRA_BROKER_GH_TOKEN" => "",
                "SPIRA_RUNTIME_INBOX_KEEPER_SECS" | "SPIRA_RUNTIME_PR_NOTIFY_SECS" | "SPIRA_RUNTIME_ASKS_SECS" | "SPIRA_RUNTIME_SUPERSEDE_DUTY_SECS" | "SPIRA_RUNTIME_DESIGN_ASKS_SECS" | "SPIRA_RUNTIME_BRAIN_DIRTY_SECS" | "SPIRA_BRAIN_DIRTY_AGE" | "SPIRA_DESIGN_ASKS_REPEAT" | "SPIRA_RUNTIME_PUBLISH_BACKLOG_SECS" | "SPIRA_RUNTIME_REPO_EVENTS_SECS" | "SPIRA_RUNTIME_VIEW_SECS" | "SPIRA_LOCAL_BACKLOG_COUNT" | "SPIRA_LOCAL_BACKLOG_AGE" | "SPIRA_RUNTIME_BEAD_SWEEP_SECS" | "SPIRA_BEAD_SWEEP_HOLD_MIN" | "SPIRA_BEAD_SWEEP_REWORK_MIN" | "SPIRA_BEAD_SWEEP_CERT_MIN" | "SPIRA_BEAD_SWEEP_WAIT_MIN" | "SPIRA_BEAD_SWEEP_BATCH_MIN" | "SPIRA_SCOPED_STOP_REVIEW_SECS" | "SPIRA_PR_DEPLOY_GIVEUP" | "SPIRA_CONCIERGE_INBOX_STALL" | "SPIRA_CONCIERGE_INBOX_BACKOFF" | "SPIRA_RUNTIME_ROUND_DUTY_SECS" | "SPIRA_ROUND_MIN" | "SPIRA_ROUND_MAX_OPEN" | "SPIRA_ROUND_DUE_REMIND" | "SPIRA_ROUND_IDLE_CUT" => "5",
                "SPIRA_WIKI" | "SPIRA_MAIL" | "SPIRA_ASK_LABEL" | "SPIRA_QUEUE_DIR" | "SPIRA_REPO_MAP" | "SPIRA_CONCIERGE_INBOX" => "/q",
                "SPIRA_HOME_REPO" => "spira",
                "SPIRA_VIEW" => "/bin/view",
                "SPIRA_VIEW_SESSION" => "pit",
                _ => return Err(format!("{k} unknown")),
            }
            .to_string())
        }
    }

    #[test]
    fn every_key_read_is_registered_in_conf_d() {
        let conf_d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d");
        let missing: Vec<&str> = Settings::KEYS.iter().copied().filter(|k| !conf_d.join(k).is_file()).collect();
        assert!(missing.is_empty(), "keys the runtime reads but spira/conf.d does not register: {missing:?}");
    }

    #[test]
    fn each_bead_sweep_key_reaches_its_own_field() {
        let base = lookup("7");
        let distinct = move |k: &str| {
            let n = ["SPIRA_RUNTIME_BEAD_SWEEP_SECS", "SPIRA_BEAD_SWEEP_HOLD_MIN", "SPIRA_BEAD_SWEEP_REWORK_MIN", "SPIRA_BEAD_SWEEP_CERT_MIN", "SPIRA_BEAD_SWEEP_WAIT_MIN", "SPIRA_BEAD_SWEEP_BATCH_MIN", "SPIRA_SCOPED_STOP_REVIEW_SECS"].iter().position(|x| *x == k);
            n.map_or_else(|| base(k), |i| Ok((11 + i).to_string()))
        };
        let b = Settings::from_lookup(&distinct).unwrap().bead_sweep;
        assert_eq!((b.interval_ms, b.hold_min, b.rework_min, b.cert_min, b.wait_min, b.batch_min, b.stop_review_secs), (11_000, 12, 13, 14, 15, 16, 17));
        assert_eq!((b.ask_label.as_str(), b.home_repo.as_str()), ("/q", "spira"));
    }

    #[test]
    fn the_view_follower_finds_hunk_and_the_cockpit_state_where_the_environment_puts_them() {
        let s = Settings::from_lookup(&lookup("7")).unwrap();
        assert_eq!((s.view.enabled, s.view.interval_ms, s.view.forget_secs, s.world.cockpit_session.as_str()), (true, 5000, 86_400, "pit"));
        let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        let full = s.clone().with_environment(&env(&[("XDG_RUNTIME_DIR", "/run/u1"), ("COCKPIT_STATE", "/st"), ("HOME", "/h"), ("COCKPIT_FORGET_SECS", "77")])).unwrap();
        assert_eq!((full.world.hunk_dir.as_path(), full.world.cockpit_state.as_path(), full.view.forget_secs), (std::path::Path::new("/run/u1/hunk-mcp"), std::path::Path::new("/st"), 77));
        let home = s.clone().with_environment(&env(&[("HOME", "/h")])).unwrap();
        assert_eq!(home.world.cockpit_state, PathBuf::from("/h/.local/state/cockpit"));
        assert!(home.world.hunk_dir.as_os_str().is_empty(), "no runtime dir is no daemon, not a guessed one");
        assert!(s.with_environment(&env(&[("COCKPIT_FORGET_SECS", "0")])).unwrap_err().contains("positive"));
    }

    #[test]
    fn a_view_follower_that_is_on_needs_a_session_and_an_empty_program_turns_it_off() {
        let with = |key: &'static str, val: &'static str| {
            let base = lookup("7");
            move |k: &str| if k == key { Ok(val.to_string()) } else { base(k) }
        };
        assert!(Settings::from_lookup(&with("SPIRA_VIEW_SESSION", " ")).unwrap_err().contains("SPIRA_VIEW_SESSION"));
        assert!(!Settings::from_lookup(&with("SPIRA_VIEW", "")).unwrap().view.enabled);
    }

    #[test]
    fn each_publish_backlog_key_reaches_its_own_field() {
        let base = lookup("7");
        let distinct = move |k: &str| match k {
            "SPIRA_RUNTIME_PUBLISH_BACKLOG_SECS" => Ok("21".to_string()),
            "SPIRA_LOCAL_BACKLOG_COUNT" => Ok("22".to_string()),
            "SPIRA_LOCAL_BACKLOG_AGE" => Ok("23".to_string()),
            _ => base(k),
        };
        let s = Settings::from_lookup(&distinct).unwrap();
        assert_eq!((s.publish_backlog.interval_ms, s.publish_backlog.max_count, s.publish_backlog.max_age_secs), (21_000, 22, 23));
    }

    #[test]
    fn every_unresolved_key_is_named_at_once() {
        let base = lookup("7");
        let refusing = move |k: &str| if k == "SPIRA_HOME_REPO" || k == "SPIRA_QUEUE_DIR" { Err(format!("{k} refused")) } else { base(k) };
        let e = Settings::from_lookup(&refusing).unwrap_err();
        assert!(e.contains("SPIRA_HOME_REPO") && e.contains("SPIRA_QUEUE_DIR"), "{e}");
    }

    #[test]
    fn every_value_comes_from_the_lookup_and_a_bad_number_is_refused() {
        let s = Settings::from_lookup(&lookup("7")).unwrap();
        assert_eq!((s.heartbeat_secs, s.stall_limit, s.heartbeat_url.as_str()), (7, 3, "http://127.0.0.1:9/ping"));
        assert_eq!(s.lc_socket, PathBuf::from("/d/sock"));
        assert_eq!(s.metrics_endpoint.as_deref(), Some("127.0.0.1:9"));
        let unset = |k: &str| if k == "SPIRA_METRICS_ENDPOINT" { Ok("  ".to_string()) } else { lookup("7")(k) };
        assert_eq!(Settings::from_lookup(&unset).unwrap().metrics_endpoint, None, "a blank endpoint disables the push");
        assert!(Settings::from_lookup(&lookup("0")).unwrap_err().contains("positive"), "zero seconds would spin the schedule");
        assert_ne!(s.hash(), Settings::from_lookup(&lookup("8")).unwrap().hash());
    }
}
