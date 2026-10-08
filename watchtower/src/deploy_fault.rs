//! `--deploy-fault-check` — land-local leaves `<queue dir>/<repo>/deploy-fault`
//! (`<head> <why>`) when the release deploy of a landing failed, and removes it on a
//! successful activation. While it stands the landed fixes are not in force: alarm, and
//! retry the build at most once per interval.

use crate::incident::{self, Finding};
use crate::log::log;
use std::path::Path;
use std::process::Command;

pub struct Cfg {
    pub release_bin: String,
    pub repo: String,
    pub retry_secs: i64,
    pub build_timeout_secs: u64,
}

pub fn parse(marker: &str) -> Option<(&str, &str)> {
    let line = marker.lines().next()?.trim();
    let (head, why) = line.split_once(' ').unwrap_or((line, ""));
    (!head.is_empty() && head.bytes().all(|b| b.is_ascii_hexdigit())).then_some((head, why.trim()))
}

pub fn retry_due(last: Option<i64>, now: i64, retry_secs: i64) -> bool {
    last.map_or(true, |t| now - t >= retry_secs)
}

fn retry_build(cfg: &Cfg, head: &str) -> String {
    let out = Command::new("timeout")
        .arg(cfg.build_timeout_secs.to_string())
        .arg(&cfg.release_bin)
        .args(["build", head, "--repo", &cfg.repo])
        .output();
    match out {
        Ok(o) if o.status.success() => format!("retry `release build {head}` succeeded"),
        Ok(o) => format!(
            "retry `release build {head}` failed ({}): {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("")
        ),
        Err(e) => format!("retry could not run {}: {e}", cfg.release_bin),
    }
}

pub fn run(now: i64, queue_dir: &Path, run_dir: &Path, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) {
    let Ok(entries) = std::fs::read_dir(queue_dir) else { return };
    for e in entries.flatten() {
        let marker = e.path().join("deploy-fault");
        let Ok(text) = std::fs::read_to_string(&marker) else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        let Some((head, why)) = parse(&text) else {
            log(&format!("watchtower: deploy-fault-check: unreadable marker {}", marker.display()));
            continue;
        };
        let stamp = run_dir.join(format!("deploy-fault-retry.{name}"));
        let last = std::fs::read_to_string(&stamp).ok().and_then(|s| s.trim().parse().ok());
        let retried = if retry_due(last, now, cfg.retry_secs) {
            let _ = std::fs::write(&stamp, now.to_string());
            retry_build(cfg, head)
        } else {
            "retry not yet due".to_string()
        };
        let body = format!(
            "{head} landed on {name} but its release was not activated: {why}\n\nCurrent is untouched, so the landed work is not in force. Last attempt: {retried}.\n\nThe marker {} clears when a release of a later landing activates; to retry by hand: `release build {head} --repo {}` then `release verify {head}` and `release activate {head}`.\n",
            marker.display(),
            cfg.repo
        );
        let f = Finding::new(db, home_repo, &format!("LAND DEPLOY FAULT: {head} landed but was not released"), &body)
            .priority(1)
            .reference(&format!("incident:deploy-fault:{name}:{head}"))
            .cause("deploy-fault");
        if incident::is_usable(incident_sh) {
            incident::alarm(incident_sh, &f);
        }
        log(&format!("watchtower: deploy-fault-check: {name} {head} — {retried}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_head_and_reason_and_rejects_garbage() {
        assert_eq!(parse("abc123 release build exited 1\n"), Some(("abc123", "release build exited 1")));
        assert_eq!(parse("abc123\n"), Some(("abc123", "")));
        assert_eq!(parse("not-a-sha why"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn retry_waits_out_the_interval() {
        assert!(retry_due(None, 100, 1800));
        assert!(!retry_due(Some(100), 1899, 1800));
        assert!(retry_due(Some(100), 1900, 1800));
    }

    #[test]
    fn run_with_a_marker_stamps_the_retry_and_without_one_does_nothing() {
        let dir = testkit::TempDir::new("deploy-fault");
        let q = dir.join("queue");
        let r = dir.join("run");
        std::fs::create_dir_all(q.join("harness")).unwrap();
        std::fs::create_dir_all(&r).unwrap();
        let cfg = Cfg { release_bin: "/nonexistent/release".into(), repo: "/x".into(), retry_secs: 1800, build_timeout_secs: 5 };
        run(1000, &q, &r, "", "harness", "/nonexistent/incident.sh", &cfg);
        assert!(!r.join("deploy-fault-retry.harness").exists());
        std::fs::write(q.join("harness/deploy-fault"), "abc123 boom\n").unwrap();
        run(1000, &q, &r, "", "harness", "/nonexistent/incident.sh", &cfg);
        assert_eq!(std::fs::read_to_string(r.join("deploy-fault-retry.harness")).unwrap(), "1000");
        run(1100, &q, &r, "", "harness", "/nonexistent/incident.sh", &cfg);
        assert_eq!(std::fs::read_to_string(r.join("deploy-fault-retry.harness")).unwrap(), "1000");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
