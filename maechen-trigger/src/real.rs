//! The production [`crate::ports::World`].

use crate::lanes::LaneLabels;
use crate::ports::World;
use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::process::Stdio;

pub struct Real {
    pub home: PathBuf,
    pub run: PathBuf,
    pub db: String,
    pub bd: String,
    pub repo_map: Option<PathBuf>,
    pub lane_labels: LaneLabels,
    registry: OnceCell<spira_config::repos::Registry>,
    strand_cfg: OnceCell<strand::config::Config>,
}

impl Real {
    pub fn new(home: PathBuf, run: PathBuf, db: String, bd: String, repo_map: Option<PathBuf>, lane_labels: LaneLabels) -> Real {
        Real { home, run, db, bd, repo_map, lane_labels, registry: OnceCell::new(), strand_cfg: OnceCell::new() }
    }

    /// `strand::config::Config`, resolved once per process the same way `strand`'s own
    /// binary resolves it (env, then the resolved toml config, then the conf.sh default) —
    /// the detectors moved there (wave 4.29, sp-8ofmt) and are reached in-process instead
    /// of through the `lib.sh` seam.
    fn strand_cfg(&self) -> &strand::config::Config {
        self.strand_cfg.get_or_init(|| strand::config::Config::resolve(&strand::config::Live::load()).unwrap_or_else(|e| {
            eprintln!("maechen-trigger: {e}");
            std::process::exit(1)
        }))
    }

    /// The repo registry (`spira_config::repos::Registry::from_env`, sp-k6lku "wave
    /// 4.13"): `spira_home_repo`/`repo_root`/`spira_landref` were already a bash
    /// subprocess sourcing lib.sh THEN shelling to the `spira-config` binary a second time
    /// (lib.sh's own shim, sp-37rmg) — two processes per lookup. `from_env` resolves
    /// `SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED`/`SPIRA_REPO_MAP` in-process
    /// instead, the same way conf.sh does, when this (bare, unit-launched) process's own
    /// environment lacks them (sp-z3eyk) — reading the bare three and the map text by hand,
    /// as this used to, found nothing in production, since conf.sh exports none of them.
    fn registry(&self) -> &spira_config::repos::Registry {
        self.registry.get_or_init(|| spira_config::repos::Registry::from_env(std::env::vars().collect(), &self.home))
    }

    fn repo_map_text_inner(&self) -> String {
        match &self.repo_map {
            Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
            None => String::new(),
        }
    }

    fn read_epoch_file(&self, name: &str) -> i64 {
        std::fs::read_to_string(self.run.join(name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
    }
}

impl World for Real {
    fn log(&self, msg: &str) {
        eprintln!(
            "{} maechen-trigger: {}",
            humantime_utc_now(),
            msg
        );
    }

    fn read_watermark(&self) -> i64 {
        self.read_epoch_file("maechen.watermark")
    }

    fn read_lastpass(&self) -> i64 {
        self.read_epoch_file("maechen.lastpass")
    }

    fn now(&self) -> i64 {
        spira_config::vtime::now_epoch() as i64
    }

    fn open_trigger_count(&self, labels: &str) -> Result<u64, String> {
        // bd says which beads carry the labels; whether each is still open is the lifecycle
        // machine's answer (sp-mve9i), never bd's status.
        let out = spira_config::bounded::bounded(&self.bd)
            .arg("-C")
            .arg(&self.db)
            .args(["list", "--all", "--label", labels, "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run bd list: {e}"))?;
        if !out.status.success() {
            return Err(format!("bd list exited {}", out.status));
        }
        let ids = json_ids(&String::from_utf8_lossy(&out.stdout))
            .ok_or_else(|| "bd list output is not a JSON array".to_string())?;
        let rows = spira_config::lc_state::list().map_err(|e| format!("lifecycle state unreadable ({e})"))?;
        Ok(unfinished_count(&ids, &spira_config::lc_state::index(rows)))
    }

    fn lane_admitted(&self, lane: &str) -> bool {
        let home = self.registry().home_repo().to_string();
        let names = self.registry().names();
        crate::lanes::lane_admitted(lane, &home, &names, |n| self.registry().field(n, spira_config::repos::Column::Lanes), &self.lane_labels)
    }

    fn repo_lanes(&self, name: &str) -> Result<String, String> {
        let raw = self.registry().field(name, spira_config::repos::Column::Lanes);
        crate::lanes::repo_lanes(name, raw.as_deref(), &self.lane_labels)
    }

    fn home_repo(&self) -> String {
        self.registry().home_repo().to_string()
    }

    fn repo_root(&self, name: &str) -> Option<PathBuf> {
        self.registry().root(name).map(PathBuf::from)
    }

    fn landref(&self, repo_path_or_name: &str) -> Option<String> {
        spira_config::repos::landref(self.registry(), repo_path_or_name)
    }

    fn repo_map_text(&self) -> String {
        self.repo_map_text_inner()
    }

    fn git_log_subjects(&self, repo_path: &Path, since_ts: i64, base_ref: &str) -> String {
        spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(repo_path)
            .arg("log")
            .arg("--format=%s")
            .arg(format!("--after=@{since_ts}"))
            .arg(base_ref)
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    fn detect_invalid_closed(&self) -> String {
        strand::detectors::detect_invalid_closed(self.strand_cfg())
    }

    fn create_bead(&self, title: &str, labels: &str, description: &str) -> Result<(), String> {
        let mut child = spira_config::bounded::bounded(&self.bd)
            .arg("-C")
            .arg(&self.db)
            .arg("create")
            .arg(title)
            .arg("--type")
            .arg("task")
            .arg("--label")
            .arg(labels)
            .arg("--priority")
            .arg("3")
            .arg("--description")
            .arg(description)
            .arg("--silent")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot spawn bd create: {e}"))?;
        let stderr = child.stderr.take();
        let mut created = String::new();
        if let Some(mut o) = child.stdout.take() {
            use std::io::Read;
            let _ = o.read_to_string(&mut created);
        }
        let status = child.wait().map_err(|e| format!("bd create: {e}"))?;
        if status.success() {
            if let Err(e) = spira_config::lifecycle_row::after_create("maechen-trigger", &created) {
            eprintln!("maechen-trigger: LIFECYCLE: row not written after create: {e}; the new bead is rowless and cannot be claimed");
        }
            Ok(())
        } else {
            let mut msg = String::new();
            if let Some(mut s) = stderr {
                use std::io::Read;
                let _ = s.read_to_string(&mut msg);
            }
            Err(format!("bd create exited {status}: {msg}"))
        }
    }
}

/// The ids in `bd list --json`'s array; `None` for anything that is not an array.
fn json_ids(input: &str) -> Option<Vec<String>> {
    match serde_json::from_str::<serde_json::Value>(input) {
        Ok(serde_json::Value::Array(a)) => Some(a.iter().filter_map(|r| r.get("id").and_then(|i| i.as_str()).map(str::to_string)).collect()),
        _ => None,
    }
}

/// `spira_open_trigger_count`'s own counter: the trigger beads still open — their lifecycle
/// row READY, WORKING or REWORK (what bd's `open,in_progress` meant). A bead with no row can
/// never be worked, so it is not open.
fn unfinished_count(ids: &[String], lc: &std::collections::HashMap<String, spira_config::lc_state::Row>) -> u64 {
    ids.iter().filter(|id| lc.get(*id).is_some_and(|r| !r.past_builder())).count() as u64
}

fn humantime_utc_now() -> String {
    // Matches the bash's `date -u +%Y-%m-%dT%H:%M:%SZ`. No chrono dependency: shell to
    // `date`, exactly as every other already-rewritten crate's log stamp does when it needs
    // one (czar-pass, reconciler) — a fixed-format UTC stamp is not worth a crate.
    spira_config::bounded::bounded("date")
        .arg("-u")
        .arg("+%Y-%m-%dT%H:%M:%SZ")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_config::lc_state::Row;

    /// sp-mve9i: an open trigger is one whose lifecycle row is still before its builder's
    /// hand-off; bd's status is not read.
    #[test]
    fn an_open_trigger_is_one_the_machine_has_not_seen_handed_on() {
        let ids = json_ids(r#"[{"id":"a","status":"closed"},{"id":"b","status":"open"},{"id":"c"},{"id":"d"},{"id":"e"}]"#).unwrap();
        let lc = [("a", "READY"), ("b", "SUBMITTED"), ("c", "WORKING"), ("d", "DONE")]
            .iter()
            .map(|(i, st)| (i.to_string(), Row { bead_id: i.to_string(), state: st.to_string(), ..Default::default() }))
            .collect();
        assert_eq!(unfinished_count(&ids, &lc), 2);
        assert!(json_ids("not json").is_none());
    }
}
