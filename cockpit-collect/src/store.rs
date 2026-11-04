//! The store's answers in-process: lifecycle verbs over [`LcClient`] (the socket in production,
//! no process), and the round/batch views of [`Projection`] where the probes used to run
//! `queue batch show` and `queue round status`. `None` is CANNOT TELL, as every probe read is.

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

use spira_config::lc_call::{LcClient, LcError};
use store_view::{Projection, Read};

/// The configured client, bounded at `SPIRA_LC_TIMEOUT` for a program route; `None` when the
/// key is unset or the route does not resolve.
pub fn client() -> Option<LcClient> {
    let secs: u64 = spira_config::process::cfg("SPIRA_LC_TIMEOUT").ok().filter(|s| !s.is_empty())?.trim().parse().ok()?;
    Some(LcClient::configured().ok()?.within(Duration::from_secs(secs)))
}

/// `args`'s stdout when the store applied it (exit 0).
pub fn answer_on(client: &LcClient, args: &[&str]) -> Option<String> {
    client.answer(args).ok()
}

static VIEWS: OnceLock<Option<Projection>> = OnceLock::new();

/// This process's views, loaded once on first use.
fn views() -> Option<&'static Projection> {
    VIEWS
        .get_or_init(|| {
            let p = Projection::configured(LcClient::configured().ok()?).ok()?;
            p.load().ok()?;
            Some(p)
        })
        .as_ref()
}

/// `read` on `p`; a read refused as stale polls once and reads again.
pub fn fresh<T>(p: &Projection, read: impl Fn(&Projection) -> Result<Read<T>, LcError>) -> Option<T> {
    match read(p) {
        Ok(r) => Some(r.value),
        Err(LcError::Refused(_)) => {
            p.poll().ok()?;
            read(p).ok().map(|r| r.value)
        }
        Err(LcError::Fault(_)) => None,
    }
}

/// What `queue batch show --repo <repo>` printed: the open forge batch's `key=value` lines,
/// nothing when none is open.
pub fn batch_show_in(p: &Projection, repo: &str) -> Option<String> {
    fresh(p, |p| p.batch_view(repo)).map(|v| v.shown())
}

pub fn batch_show(repo: &str) -> Option<String> {
    batch_show_in(views()?, repo)
}

/// The bead ids of every member of the repo's open rounds (`members=id:tip ...`).
pub fn round_members_in(p: &Projection, repo: &str) -> Option<HashSet<String>> {
    let view = fresh(p, |p| p.round_view(repo))?;
    Some(
        view.rounds
            .iter()
            .filter_map(|r| r.record.get("members"))
            .flat_map(|m| m.split_whitespace().map(|t| t.split(':').next().unwrap_or(t).to_string()).collect::<Vec<_>>())
            .collect(),
    )
}

pub fn round_members(repo: &str) -> Option<HashSet<String>> {
    round_members_in(views()?, repo)
}

#[cfg(test)]
pub mod scripted {
    use spira_config::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A socket double: answers by verb, and records every argv it was sent.
    pub struct Scripted {
        pub answers: Vec<(String, i32, String)>,
        pub sent: Mutex<Vec<Vec<String>>>,
    }

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.sent.lock().unwrap().push(argv.to_vec());
            let key = argv.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
            self.answers
                .iter()
                .find(|(k, _, _)| key == *k || argv.first() == Some(k))
                .map(|(_, code, out)| Reply { code: *code, out: out.clone(), err: String::new() })
                .ok_or_else(|| format!("cannot tell: unscripted {argv:?}"))
        }
    }

    pub fn client(answers: &[(&str, i32, &str)]) -> (LcClient, Arc<Scripted>) {
        let s = Arc::new(Scripted { answers: answers.iter().map(|(k, c, o)| (k.to_string(), *c, o.to_string())).collect(), sent: Mutex::new(Vec::new()) });
        (LcClient::over(Arc::clone(&s) as Arc<dyn Transport>, true), s)
    }

    /// One repo's store: an open forge batch `b1`, a certifying round `r2`, a staged `s3`, a landed `old`.
    pub fn rounds_store(repo: &str) -> String {
        serde_json::json!({"ok": {"list_rev": 7, "items": [
            {"key": format!("batches/{repo}/b1"), "create_rev": {"main": 3}, "value": {"data": {
                "batch_id": "b1", "state": "OPEN", "repo": repo,
                "round": {"head": "h1", "members": "sp-a:t1", "forge.phase": "pr-open", "pr": "41"}}}},
            {"key": format!("batches/{repo}/r2"), "create_rev": {"main": 5}, "value": {"data": {
                "batch_id": "r2", "state": "CI_RUNNING", "repo": repo,
                "round": {"head": "h2", "members": "sp-b:t2 sp-c:t3"}}}},
            {"key": format!("batches/{repo}/s3"), "create_rev": {"main": 6}, "value": {"data": {
                "batch_id": "s3", "state": "STAGED", "repo": repo,
                "round": {"head": "h3", "members": "sp-d:t4"}}}},
            {"key": format!("batches/{repo}/old"), "create_rev": {"main": 1}, "value": {"data": {
                "batch_id": "old", "state": "LANDED", "repo": repo,
                "round": {"head": "h0", "members": "sp-z:t0"}}}},
        ]}})
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::scripted::{client, rounds_store};
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    fn projection(repo: &str) -> Projection {
        let store = rounds_store(repo);
        let (c, _) = client(&[("rc list", 0, &store), ("list", 0, "[]")]);
        let p = Projection::new(c, Duration::from_secs(60), Arc::new(Instant::now));
        p.load().unwrap();
        p
    }

    #[test]
    fn batch_show_reads_the_open_forge_batch_from_the_views_with_no_process() {
        let p = projection("spira");
        let shown = batch_show_in(&p, "spira").unwrap();
        assert!(shown.contains("batch_id=b1\n"), "{shown}");
        assert!(shown.contains("forge.phase=pr-open\n") && shown.contains("pr=41\n"), "{shown}");
        assert_eq!(batch_show_in(&p, "other").as_deref(), Some(""), "no open batch prints nothing");
    }

    #[test]
    fn every_open_and_staged_round_member_is_in_flight_and_an_ended_one_is_not() {
        let p = projection("spira");
        let m = round_members_in(&p, "spira").unwrap();
        let want: HashSet<String> = ["sp-a", "sp-b", "sp-c", "sp-d"].iter().map(|s| s.to_string()).collect();
        assert_eq!(m, want);
    }

    #[test]
    fn an_unloaded_view_cannot_tell() {
        let (c, _) = client(&[]);
        let p = Projection::new(c, Duration::from_secs(60), Arc::new(Instant::now));
        assert_eq!(batch_show_in(&p, "spira"), None);
    }

    #[test]
    fn a_verb_goes_over_the_socket_and_a_refusal_is_none() {
        let (c, sent) = client(&[("list", 0, "[{\"bead_id\":\"sp-a\"}]"), ("pane-facets", 3, "refused")]);
        assert_eq!(answer_on(&c, &["list", "--batches"]).as_deref(), Some("[{\"bead_id\":\"sp-a\"}]"));
        assert_eq!(answer_on(&c, &["pane-facets", "inflow"]), None);
        assert_eq!(sent.sent.lock().unwrap()[0], vec!["list".to_string(), "--batches".to_string()]);
    }
}
