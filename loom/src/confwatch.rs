//! Loom's own config watch: a metadata check per tick, a re-resolve only when the file moved.

use crate::Config;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Why Loom should exit for restart, or `None` to stay: `now` is the pinned text re-rendered, or why it cannot be.
pub fn restart_reason(old: &str, now: &Result<String, String>) -> Option<String> {
    let now = match now {
        Err(e) => return Some(format!("config no longer resolves: {e}")),
        Ok(n) if n == old => return None,
        Ok(n) => n,
    };
    let only = |a: &str, b: &str| a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect::<Vec<_>>().join(" ");
    Some(format!("config changed ({} → {})", only(old, now), only(now, old)))
}

/// The pinned config as the files declare it now.
pub fn render_fresh() -> Result<String, String> {
    spira_config::process::with_fresh(Config::from_env)?.map(|c| c.render_pinned())
}

type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &PathBuf) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// The file Loom's config is edited in: the last layer of `SPIRA_TOML`.
pub fn conf_file() -> Option<PathBuf> {
    let spec = spira_config::process::spec().ok()?;
    spec.rsplit(':').next().map(PathBuf::from)
}

/// Resolves when the pinned config differs from `pinned0` (or stops resolving), with the reason.
pub async fn changed(path: PathBuf, tick: Duration, pinned0: String) -> String {
    changed_with(path, tick, pinned0, render_fresh).await
}

pub async fn changed_with(path: PathBuf, tick: Duration, pinned0: String, render: impl Fn() -> Result<String, String>) -> String {
    let mut seen = stamp(&path);
    loop {
        tokio::time::sleep(tick).await;
        let now = stamp(&path);
        if now == seen {
            continue;
        }
        seen = now;
        if let Some(why) = restart_reason(&pinned0, &render()) {
            return why;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test_id(th63gqv)]
    #[test]
    fn a_changed_key_restarts_with_the_old_and_new_lines() {
        let why = restart_reason("A=1\nB=2\n", &Ok("A=1\nB=3\n".into())).unwrap();
        assert_eq!(why, "config changed (B=2 → B=3)");
    }

    #[test_id(tw5w1lf)]
    #[test]
    fn an_unresolvable_config_restarts_with_the_refusal() {
        let why = restart_reason("A=1\n", &Err("boom".into())).unwrap();
        assert_eq!(why, "config no longer resolves: boom");
    }

    #[test_id(tj1wzfp)]
    #[test]
    fn identical_pinned_text_stays() {
        assert_eq!(restart_reason("A=1\n", &Ok("A=1\n".into())), None);
    }

    // covers: UC-cockpit-observability-07
    #[test_id(twg6xzg)]
    #[tokio::test]
    async fn touching_the_file_without_a_pinned_change_stays_and_a_pinned_change_restarts() {
        let dir = testkit::TempDir::new("loom-confwatch");
        let file = dir.join("conf.toml");
        std::fs::write(&file, "k = 1\n").unwrap();
        let renders = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let render = {
            let (file, renders) = (file.clone(), renders.clone());
            move || {
                renders.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let t = std::fs::read_to_string(&file).unwrap();
                Ok(format!("PINNED={}\n", t.lines().find(|l| l.starts_with("pinned")).unwrap_or("")))
            }
        };
        std::fs::write(&file, "pinned = 1\n").unwrap();
        let w = tokio::spawn(changed_with(file.clone(), Duration::from_millis(5), "PINNED=pinned = 1\n".into(), render));
        tokio::task::yield_now().await;
        std::fs::write(&file, "pinned = 1\nother = 2\n").unwrap();
        while renders.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await; // clock-safe: caps a wait on the render counter
        }
        assert!(!w.is_finished(), "an unpinned edit must not restart");
        std::fs::write(&file, "pinned = 22\n").unwrap();
        let why = tokio::time::timeout(Duration::from_secs(5), w).await.expect("restart").unwrap();
        assert!(why.starts_with("config changed (PINNED=pinned = 1 → PINNED=pinned = 22"), "{why}");
    }
}
