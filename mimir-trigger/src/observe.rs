//! The reads a scheduler trigger task's observe probe makes (`trigger.mimir`, `trigger.groom`,
//! `trigger.warden`): the open-trigger count over the store's door, the lane check over the
//! registry, the persona's own epoch files, and landings counted from `git log`. Every value
//! is handed in by the caller (its pinned config), never read from the environment here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use spira_config::lc_call::LcClient;
use spira_config::repos::{Column, Registry};

use crate::engine;
use crate::lanes::{self, LaneLabels};
use crate::real::{json_ids, unfinished_count};

/// `spira_open_trigger_count <labels>` over the store's door: the beads carrying every label in
/// `labels` (comma-separated) whose lifecycle row is still before its builder's hand-off and not
/// poisoned. `Err` when either read gives no answer; an unreadable count is never zero.
pub fn open_trigger_count_on(lc: &LcClient, labels: &str) -> Result<u64, String> {
    let listed = lc.call(&["content", "list", "--all", "--limit", "0", "--label", labels, "--json"]).map_err(|e| format!("content list unreadable ({e})"))?;
    if listed.code != 0 {
        return Err(format!("content list exited {}: {}", listed.code, listed.why()));
    }
    let text = listed.out.find('[').map_or("", |i| &listed.out[i..]);
    let ids = json_ids(text).ok_or("content list output is not a JSON array")?;
    if ids.is_empty() {
        return Ok(0);
    }
    let rows = spira_config::lc_state::list_on(lc).map_err(|e| format!("lifecycle state unreadable ({e})"))?;
    Ok(unfinished_count(&ids, &spira_config::lc_state::index(rows)))
}

/// The value of `key` in the pinned `config`, or a refusal naming it.
pub fn pinned<'a>(config: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, String> {
    config.get(key).map(String::as_str).ok_or_else(|| format!("{key} is not in the task's pinned config"))
}

/// The six lane labels, from the pinned config.
pub fn lane_labels(config: &BTreeMap<String, String>) -> Result<LaneLabels, String> {
    Ok(LaneLabels {
        plan: pinned(config, "SPIRA_PLAN_LABEL")?.into(),
        incident: pinned(config, "SPIRA_INCIDENT_LABEL")?.into(),
        groomer: pinned(config, "SPIRA_GROOMER_LABEL")?.into(),
        mimir: pinned(config, "SPIRA_MIMIR_LABEL")?.into(),
        spike: pinned(config, "SPIRA_SPIKE_LABEL")?.into(),
        czar: pinned(config, "SPIRA_CZAR_LABEL")?.into(),
    })
}

/// The repo registry from the pinned config (`SPIRA_REPO_MAP`, `SPIRA_HOME_REPO` when pinned),
/// through the registry's one production door.
pub fn registry(config: &BTreeMap<String, String>) -> Result<Registry, String> {
    let home = spira_config::resolve::locate_home_for_process()?;
    let env: BTreeMap<String, String> = config.iter().filter(|(k, v)| matches!(k.as_str(), "SPIRA_REPO_MAP" | "SPIRA_HOME_REPO") && !v.is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect();
    Ok(Registry::from_env(env, &home))
}

/// `spira_lane_admitted <lane>` over `reg`.
pub fn lane_admitted(reg: &Registry, lane: &str, labels: &LaneLabels) -> bool {
    lanes::lane_admitted(lane, reg.home_repo(), &reg.names(), |n| reg.field(n, Column::Lanes), labels)
}

/// An epoch-seconds file a persona writes (`mimir.lastpass`, `groom.lastpass`): 0 when missing
/// or not all digits.
pub fn read_epoch(path: &Path) -> i64 {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Distinct bead ids landed on one repository (by name or path) since `since_ts`; 0 when its
/// base ref or checkout cannot be resolved.
pub fn landings_since(reg: &Registry, repo_name_or_path: &str, since_ts: i64) -> u64 {
    let Some(base_ref) = spira_config::repos::landref(reg, repo_name_or_path) else { return 0 };
    let path = if repo_name_or_path.contains('/') { Some(PathBuf::from(repo_name_or_path)) } else { reg.root(repo_name_or_path).map(PathBuf::from) };
    match path.filter(|p| p.is_dir()) {
        Some(p) => engine::parse_landing_ids(&git_log_subjects(&p, since_ts, &base_ref)).len() as u64,
        None => 0,
    }
}

/// Landings since `since_ts` on the home repo plus every other repo the map names, as the
/// Mimir sweep counts them.
pub fn landings_everywhere(reg: &Registry, map_text: &str, since_ts: i64) -> u64 {
    let home = reg.home_repo().to_string();
    let mut n = landings_since(reg, &home, since_ts);
    for (name, path) in engine::parse_repo_map(map_text) {
        if name != home && Path::new(&path).is_dir() {
            n += landings_since(reg, &path, since_ts);
        }
    }
    n
}

/// `git -C <repo> log --format=%s --after=@<since> <base_ref>`.
// FORK-EDGE: git stays a child (the contract keeps `git log` out of process: there is no git
// library in the tree, and a read of the commit graph is not a Spira binary).
pub fn git_log_subjects(repo: &Path, since_ts: i64, base_ref: &str) -> String {
    spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(repo)
        .args(["log", "--format=%s", &format!("--after=@{since_ts}"), base_ref])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// `detect_invalid_closed`'s raw lines, in-process through `strand`.
pub fn invalid_closed_raw() -> Result<String, String> {
    let cfg = strand::config::Config::resolve(&strand::config::Live::load())?;
    Ok(strand::detectors::detect_invalid_closed(&cfg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_config::lc_call::{Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Store(Mutex<Vec<Vec<String>>>);

    impl Transport for Store {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            let out = match argv.first().map(String::as_str) {
                Some("content") => r#"[{"id":"a"},{"id":"b"},{"id":"c"}]"#,
                _ => r#"[{"bead_id":"a","state":"READY"},{"bead_id":"b","state":"DONE"},{"bead_id":"c","state":"WORKING","holds":["poison"]}]"#,
            };
            Ok(Reply { code: 0, out: out.into(), err: String::new() })
        }
    }

    #[test]
    fn the_open_count_reads_the_content_door_and_the_lifecycle_list() {
        let t = Arc::new(Store(Mutex::new(vec![])));
        let lc = LcClient::over(t.clone(), true);
        assert_eq!(open_trigger_count_on(&lc, "spira,mimir"), Ok(1));
        assert_eq!(t.0.lock().unwrap()[0], ["content", "list", "--all", "--limit", "0", "--label", "spira,mimir", "--json"]);
    }
}
