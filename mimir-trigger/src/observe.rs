//! The reads a scheduler trigger task's observe probe makes (`trigger.mimir`, `trigger.groom`,
//! `trigger.warden`): the open-trigger count over the store's door, the lane check over the
//! registry, the persona's own epoch files, and landings counted from `git log`. Every value
//! is handed in by the caller (its pinned config), never read from the environment here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use spira_store::lc_call::LcClient;
use spira_config::repos::{Column, Registry};

use crate::engine;
use crate::lanes::{self, LaneLabels};

/// The ids in `content list --json`'s array; `None` for anything that is not an array.
pub fn json_ids(input: &str) -> Option<Vec<String>> {
    match serde_json::from_str::<serde_json::Value>(input) {
        Ok(serde_json::Value::Array(a)) => Some(a.iter().filter_map(|r| r.get("id").and_then(|i| i.as_str()).map(str::to_string)).collect()),
        _ => None,
    }
}

/// `spira_open_trigger_count`'s own counter: the trigger beads still open — their lifecycle
/// row READY, WORKING or REWORK (what bd's `open,in_progress` meant). A bead with no row can
/// never be worked, and a poisoned one is not claimable again: neither is open, or one dead
/// trigger would suppress every later one.
pub fn unfinished_count(ids: &[String], lc: &std::collections::HashMap<String, spira_store::lc_state::Row>) -> u64 {
    ids.iter().filter(|id| lc.get(*id).is_some_and(|r| !r.past_builder() && !r.held("poison"))).count() as u64
}

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
    let rows = spira_store::lc_state::list_live_on(lc).map_err(|e| format!("lifecycle state unreadable ({e})"))?;
    Ok(unfinished_count(&ids, &spira_store::lc_state::index(rows)))
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

/// The subjects of `base_ref`'s commits dated `since_ts` or later, as `git log --format=%s --after=@<since>`
/// prints them. The rows read are held under the ref's sha, so a landing ref that has not moved costs no git.
// FORK-EDGE: git stays a child (the contract keeps `git log` out of process: there is no git
// library in the tree, and a read of the commit graph is not a Spira binary).
pub fn git_log_subjects(repo: &Path, since_ts: i64, base_ref: &str) -> String {
    let read = |from: i64| -> Result<(i64, Vec<(i64, String)>), String> {
        let out = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(repo)
            .args(["log", "--format=%ct %s", &format!("--after=@{from}"), base_ref])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout);
        Ok((from, text.lines().filter_map(|l| l.split_once(' ')).filter_map(|(at, subject)| Some((at.parse().ok()?, subject.to_string()))).collect()))
    };
    let (from, rows) = match spira_config::git_refs::sha_of(repo, base_ref) {
        Some(tip) => spira_config::git_refs::held(repo, &format!("landing subjects {base_ref}"), &tip, || read(since_ts)),
        None => read(since_ts),
    }
    .unwrap_or_default();
    let rows = if from > since_ts { read(since_ts).map(|(_, r)| r).unwrap_or_default() } else { rows };
    rows.iter().filter(|(at, _)| *at >= since_ts).map(|(_, subject)| format!("{subject}\n")).collect()
}

/// `detect_invalid_closed`'s raw lines, in-process through `strand`.
pub fn invalid_closed_raw() -> Result<String, String> {
    let cfg = strand::config::Config::resolve(&strand::config::Live::load())?;
    Ok(strand::detectors::detect_invalid_closed(&cfg, &spira_store::lc_call::LcClient::configured()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn git_at(repo: &Path, at: u64, args: &[&str]) {
        let date = format!("@{at} +0000");
        let out = std::process::Command::new("git").arg("-C").arg(repo).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).env("GIT_AUTHOR_DATE", &date).env("GIT_COMMITTER_DATE", &date).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test_id(tsdmrul)]
    #[test]
    fn landing_subjects_are_held_per_tip_and_cut_by_the_requested_time_in_process() {
        let d = testkit::TempDir::new("mimir-subjects");
        git_at(d.path(), 1700000100, &["init", "-q", "-b", "main"]);
        git_at(d.path(), 1700000100, &["commit", "-q", "--allow-empty", "-m", "old"]);
        git_at(d.path(), 1700000200, &["commit", "-q", "--allow-empty", "-m", "sp-aaaaaa: mid"]);
        git_at(d.path(), 1700000300, &["commit", "-q", "--allow-empty", "-m", "sp-bbbbbb: new"]);
        assert_eq!(git_log_subjects(d.path(), 1700000150, "main"), "sp-bbbbbb: new\nsp-aaaaaa: mid\n");
        let hidden = d.path().join(".git/hidden");
        std::fs::rename(d.path().join(".git/objects"), &hidden).unwrap();
        assert_eq!(git_log_subjects(d.path(), 1700000250, "main"), "sp-bbbbbb: new\n", "the tip has not moved, so no git is asked and the later cutoff filters the held rows");
        std::fs::rename(&hidden, d.path().join(".git/objects")).unwrap();
        git_at(d.path(), 1700000400, &["commit", "-q", "--allow-empty", "-m", "sp-cccccc: newer"]);
        assert_eq!(git_log_subjects(d.path(), 1700000250, "main"), "sp-cccccc: newer\nsp-bbbbbb: new\n", "a moved tip is read again");
        assert_eq!(git_log_subjects(d.path(), 1700000050, "main").lines().count(), 4, "an earlier cutoff than the held read is read again");
    }

    use spira_store::lc_state::Row;

    /// sp-mve9i: an open trigger is one whose lifecycle row is still before its builder's
    /// hand-off; bd's status is not read.
    #[test_id(tg04f41)]
    #[test]
    fn an_open_trigger_is_one_the_machine_has_not_seen_handed_on() {
        let ids = json_ids(r#"[{"id":"a","status":"closed"},{"id":"b","status":"open"},{"id":"c"},{"id":"d"},{"id":"e"}]"#).unwrap();
        let lc = [("a", "READY"), ("b", "SUBMITTED"), ("c", "WORKING"), ("d", "DONE")]
            .iter()
            .map(|(i, st)| (i.to_string(), Row { bead_id: i.to_string(), state: st.to_string(), ..Default::default() }))
            .collect();
        assert_eq!(unfinished_count(&ids, &lc), 2);
        let mut lc: std::collections::HashMap<String, Row> = lc;
        lc.get_mut("a").unwrap().holds = vec!["poison".to_string()];
        assert_eq!(unfinished_count(&ids, &lc), 1, "a poisoned trigger is dead and must not suppress the next");
        assert!(json_ids("not json").is_none());
    }

    // covers: UC-ops-detection-remediation-16
    #[test_id(tshqhsu)]
    #[test]
    fn a_trigger_still_working_suppresses_a_second_and_one_handed_on_does_not() {
        let ids = json_ids(r#"[{"id":"w"},{"id":"s"}]"#).unwrap();
        let row = |st: &str, id: &str| (id.to_string(), Row { bead_id: id.to_string(), state: st.to_string(), ..Default::default() });
        let lc: std::collections::HashMap<_, _> = [row("WORKING", "w"), row("SUBMITTED", "s")].into_iter().collect();
        assert_eq!(unfinished_count(&ids, &lc), 1);
        assert_eq!(unfinished_count(&ids[..1], &lc), 1, "positive control: the working one alone counts");
        assert_eq!(unfinished_count(&ids[1..], &lc), 0);
    }

    // covers: UC-ops-detection-remediation-16
    #[test_id(txqmn0y)]
    #[test]
    fn landings_on_a_repo_whose_only_remote_is_not_origin_are_read_from_its_own_default_branch_since_the_mark() {
        let ws = testkit::TempDir::new("mimir-trigger-gitea");
        let remote = ws.path().join("remote.git");
        let repo = ws.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        testkit::git(ws.path(), &["init", "-q", "--bare", "-b", "trunk", remote.to_str().unwrap()]);
        testkit::git(&repo, &["init", "-q", "-b", "trunk"]);
        let at = |when: &str, msg: &str| {
            let out = std::process::Command::new("git")
                .arg("-C").arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid", "commit", "-q", "--allow-empty", "-m", msg])
                .env("GIT_AUTHOR_DATE", when).env("GIT_COMMITTER_DATE", when)
                .output().unwrap();
            assert!(out.status.success());
        };
        at("2020-01-01T00:00:00Z", "spira: land sp-old1");
        at("2030-01-01T00:00:00Z", "spira: land sp-new1");
        at("2030-01-02T00:00:00Z", "tidy readme");
        testkit::git(&repo, &["remote", "add", "gitea", remote.to_str().unwrap()]);
        testkit::git(&repo, &["push", "-q", "gitea", "trunk"]);

        let reg = spira_config::repos::Registry::new(None, &BTreeMap::new(), ws.path());
        let base = spira_config::repos::landref(&reg, repo.to_str().unwrap()).expect("a lone remote that is not origin still resolves");
        assert_eq!(base, "gitea/trunk");
        let mark = 1_893_000_000; // 2029-12-26, after the old commit and before the new
        let ids = crate::engine::parse_landing_ids(&git_log_subjects(&repo, mark, &base));
        assert_eq!(ids, ["sp-new1"]);
    }

    use spira_store::lc_call::{Reply, Transport};
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
            Ok(Reply { code: 0, out: out.into(), err: String::new(), read_ended: false })
        }
    }

    #[test_id(t8b95xh)]
    #[test]
    fn the_open_count_reads_the_content_door_and_the_lifecycle_list() {
        let t = Arc::new(Store(Mutex::new(vec![])));
        let lc = LcClient::over(t.clone(), true);
        assert_eq!(open_trigger_count_on(&lc, "spira,mimir"), Ok(1));
        assert_eq!(t.0.lock().unwrap()[0], ["content", "list", "--all", "--limit", "0", "--label", "spira,mimir", "--json"]);
    }
}
