//! Read once at start, then never again: a running service whose config file changes keeps the
//! values it started with until it is restarted.

use std::path::PathBuf;

use spira_config::process::cfg;

use crate::watchers::round_duty::RoundDuty;
use crate::world::World;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub heartbeat_url: String,
    pub heartbeat_secs: u64,
    pub stall_limit: u32,
    pub run_dir: PathBuf,
    pub lc_socket: PathBuf,
    pub round_duty: RoundDuty,
    pub world: World,
}

fn num<T: std::str::FromStr + PartialOrd + Default>(key: &str, raw: &str) -> Result<T, String> {
    raw.trim().parse::<T>().ok().filter(|v| *v > T::default()).ok_or_else(|| format!("{key} must be a positive integer, got {raw:?}"))
}

impl Settings {
    pub fn load() -> Result<Settings, String> {
        Self::from_lookup(&cfg)
    }

    pub fn from_lookup(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Settings, String> {
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
        let world = World {
            queue_dir: PathBuf::from(get("SPIRA_QUEUE_DIR")?),
            repo_map: PathBuf::from(get("SPIRA_REPO_MAP")?),
            home_repo: PathBuf::from(get("SPIRA_REPO")?),
            inbox: PathBuf::from(get("SPIRA_CONCIERGE_INBOX")?),
            lc_socket: PathBuf::from(&socket),
            run_dir: run_dir.clone(),
        };
        Ok(Settings {
            round_duty,
            world,
            heartbeat_url: get("SPIRA_RUNTIME_HEARTBEAT_URL")?,
            heartbeat_secs: num("SPIRA_RUNTIME_HEARTBEAT_SECS", &get("SPIRA_RUNTIME_HEARTBEAT_SECS")?)?,
            stall_limit: num("SPIRA_RUNTIME_STALL_LIMIT", &get("SPIRA_RUNTIME_STALL_LIMIT")?)?,
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
        let text = format!("{}\n{}\n{}\n{}\n{}\n{:?}\n{:?}", self.heartbeat_url, self.heartbeat_secs, self.stall_limit, self.run_dir.display(), self.lc_socket.display(), self.round_duty, self.world);
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
                "SPIRA_RUN" => "/r",
                "SPIRA_LC_SOCKET" => "/d/sock",
                "SPIRA_RUNTIME_ROUND_DUTY_SECS" | "SPIRA_ROUND_MIN" | "SPIRA_ROUND_MAX_OPEN" | "SPIRA_ROUND_DUE_REMIND" | "SPIRA_ROUND_IDLE_CUT" => "5",
                "SPIRA_QUEUE_DIR" | "SPIRA_REPO_MAP" | "SPIRA_REPO" | "SPIRA_CONCIERGE_INBOX" => "/q",
                _ => return Err(format!("{k} unknown")),
            }
            .to_string())
        }
    }

    #[test]
    fn every_value_comes_from_the_lookup_and_a_bad_number_is_refused() {
        let s = Settings::from_lookup(&lookup("7")).unwrap();
        assert_eq!((s.heartbeat_secs, s.stall_limit, s.heartbeat_url.as_str()), (7, 3, "http://127.0.0.1:9/ping"));
        assert_eq!(s.lc_socket, PathBuf::from("/d/sock"));
        assert!(Settings::from_lookup(&lookup("0")).unwrap_err().contains("positive"), "zero seconds would spin the schedule");
        assert_ne!(s.hash(), Settings::from_lookup(&lookup("8")).unwrap().hash());
    }
}
