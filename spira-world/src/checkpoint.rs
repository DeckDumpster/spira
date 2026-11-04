//! Checkpoint holds across a `world.sh stop`: a dwarf slain by the stop leaves its bead under
//! a `wait` hold with a `checkpoint-until:` reason, and `world.sh start` lifts them.

use serde_json::Value;
use spira_config::lc_call::LcClient;

/// The beads in `spira-lc list --hold wait` output whose hold is a checkpoint.
pub fn checkpointed(list_json: &str) -> Vec<String> {
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(list_json.trim()) else { return Vec::new() };
    rows.iter()
        .filter(|r| r.get("reason").and_then(Value::as_str).is_some_and(spira_config::lc_state::is_checkpoint))
        .filter_map(|r| r.get("bead_id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Lifts every checkpoint hold; returns the beads lifted and the beads that refused.
pub fn lift_all() -> (Vec<String>, Vec<String>) {
    match LcClient::configured() {
        Ok(client) => lift_all_over(&client, &|bead| unhold_wait(&client, bead)),
        Err(_) => (Vec::new(), vec!["(spira-lc list could not run)".into()]),
    }
}

/// [`lift_all`] reading the holds through `client` and lifting each with `unhold`.
pub fn lift_all_over(client: &LcClient, unhold: &dyn Fn(&str) -> bool) -> (Vec<String>, Vec<String>) {
    let list = match client.call(&["list", "--hold", "wait"]) {
        Err(_) => return (Vec::new(), vec!["(spira-lc list could not run)".into()]),
        Ok(r) if r.code != 0 => return (Vec::new(), vec!["(spira-lc list refused)".into()]),
        Ok(r) => r.out,
    };
    let (mut lifted, mut failed) = (Vec::new(), Vec::new());
    for bead in checkpointed(&list) {
        if unhold(&bead) { lifted.push(bead) } else { failed.push(bead) }
    }
    (lifted, failed)
}

/// `unhold <bead> wait world` through the store's socket: `true` only when it applied.
pub fn unhold_wait(client: &LcClient, bead: &str) -> bool {
    client.call(&["unhold", bead, "wait", "world"]).is_ok_and(|r| r.code == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_config::lc_call::{Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Scripted {
        answer: Result<Reply, String>,
        asked: Mutex<Vec<Vec<String>>>,
    }

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _deadline: Duration) -> Result<Reply, String> {
            self.asked.lock().unwrap().push(argv.to_vec());
            self.answer.clone()
        }
    }

    fn client(answer: Result<Reply, String>) -> (Arc<Scripted>, LcClient) {
        let t = Arc::new(Scripted { answer, asked: Mutex::new(Vec::new()) });
        (Arc::clone(&t), LcClient::over(t, true))
    }

    #[test]
    fn the_holds_are_read_over_the_client_and_each_checkpoint_is_lifted() {
        let rows = r#"[{"bead_id":"sp-a","reason":"checkpoint-until: 2026-11-04"},{"bead_id":"sp-b","reason":"waiting on ci"},{"bead_id":"sp-c","reason":"checkpoint-until: x"}]"#;
        let (t, c) = client(Ok(Reply { code: 0, out: rows.into(), err: String::new() }));
        let (lifted, failed) = lift_all_over(&c, &|b| b == "sp-a");
        assert_eq!(*t.asked.lock().unwrap(), vec![vec!["list".to_string(), "--hold".into(), "wait".into()]]);
        assert_eq!((lifted, failed), (vec!["sp-a".to_string()], vec!["sp-c".to_string()]));
    }

    #[test]
    fn a_refused_or_unanswered_list_lifts_nothing_and_says_which() {
        let (_, refused) = client(Ok(Reply { code: 3, out: "no".into(), err: String::new() }));
        assert_eq!(lift_all_over(&refused, &|_| true), (vec![], vec!["(spira-lc list refused)".to_string()]));
        let (_, down) = client(Err("cannot tell: down".into()));
        assert_eq!(lift_all_over(&down, &|_| true), (vec![], vec!["(spira-lc list could not run)".to_string()]));
    }

    #[test]
    fn the_unhold_goes_over_the_client_and_only_an_applied_one_counts() {
        let (t, ok) = client(Ok(Reply { code: 0, out: String::new(), err: String::new() }));
        assert!(unhold_wait(&ok, "sp-a"));
        assert_eq!(*t.asked.lock().unwrap(), vec![["unhold", "sp-a", "wait", "world"].map(String::from).to_vec()]);
        let (_, refused) = client(Ok(Reply { code: 3, out: String::new(), err: "lc: refused".into() }));
        assert!(!unhold_wait(&refused, "sp-a"));
        let (_, down) = client(Err("cannot tell: down".into()));
        assert!(!unhold_wait(&down, "sp-a"));
    }
}
