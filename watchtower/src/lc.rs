//! Delivery and bead state as `spira-lc` answers it. A caller that cannot reach spira-lc gets
//! `None`, never an empty list: "cannot tell" must not read as "nothing queued".

use serde_json::Value;
use spira_config::lc_call::LcClient;

const HARNESS_ACTOR: &str = "harness";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeadRow {
    pub id: String,
    pub tip: String,
    pub since: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryRow {
    pub id: String,
    pub mode: String,
    pub entered_at: Option<i64>,
    pub version: String,
}

fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn epoch(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn run(args: &[&str]) -> Option<String> {
    run_on(&client()?, args)
}

/// The answer when it applied (exit 0); a refusal or no answer is `None`, never empty.
fn run_on(client: &LcClient, args: &[&str]) -> Option<String> {
    client.answer(args).ok()
}

fn rows(args: &[&str]) -> Option<Vec<Value>> {
    rows_on(&client()?, args)
}

/// The declared route: the service's socket, or `$SPIRA_LC_BIN`'s stand-in in a fixture.
fn client() -> Option<LcClient> {
    LcClient::configured().ok()
}

fn rows_on(client: &LcClient, args: &[&str]) -> Option<Vec<Value>> {
    match serde_json::from_str::<Value>(&run_on(client, args)?).ok()? {
        Value::Array(a) => Some(a),
        _ => None,
    }
}

pub fn beads_in(state: &str) -> Option<Vec<BeadRow>> {
    Some(
        rows(&["list", "--state", state, "--snapshot"])?
            .iter()
            .map(|r| BeadRow { id: text(r, "bead_id"), tip: text(r, "tip"), since: epoch(r, "since") })
            .collect(),
    )
}

/// The newest `LANDED` bead and when it entered that state.
pub fn last_landed() -> Option<(String, i64)> {
    beads_in("LANDED")?.into_iter().filter_map(|b| Some((b.id, b.since?))).max_by_key(|(_, at)| *at)
}

pub fn pr_open() -> Option<Vec<DeliveryRow>> {
    Some(
        rows(&["list", "--delivery", "--state", "PR_OPEN"])?
            .iter()
            .map(|r| DeliveryRow {
                id: text(r, "bead_id"),
                mode: text(r, "mode"),
                entered_at: epoch(r, "entered_at"),
                version: text(r, "version"),
            })
            .collect(),
    )
}

/// The PrOpen `Requeued` event, as the harness: the exit that sends the bead back through
/// the landing pass for a rebase.
pub fn requeue_pr_open(id: &str, version: &str) -> bool {
    let Some(shown) = run(&["show", id]).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return false;
    };
    let tip = shown.get("bead").map(|b| text(b, "tip")).unwrap_or_default();
    let kind = serde_json::json!({"Requeued": {"tip": tip}}).to_string();
    run(&["event", "delivery", id, "--expect", "PR_OPEN", "--version", version, "--actor", HARNESS_ACTOR, "--kind", &kind]).is_some()
}

/// A scripted store for tests: records every argv, answers each with one fixed reply.
#[cfg(test)]
pub(crate) mod scripted {
    use spira_config::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};

    pub struct Scripted {
        pub seen: Mutex<Vec<Vec<String>>>,
        answer: (i32, String),
    }

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<Reply, String> {
            self.seen.lock().unwrap().push(argv.to_vec());
            Ok(Reply { code: self.answer.0, out: self.answer.1.clone(), err: String::new() })
        }
    }

    /// A socket client over the script.
    pub fn scripted(code: i32, out: &str) -> (Arc<Scripted>, LcClient) {
        let t = Arc::new(Scripted { seen: Mutex::new(Vec::new()), answer: (code, out.into()) });
        (t.clone(), LcClient::over(t, true))
    }
}

#[cfg(test)]
mod tests {
    use super::scripted::scripted;
    use super::*;

    #[test]
    fn rows_are_read_through_the_client_not_a_program() {
        let (t, c) = scripted(0, r#"[{"bead_id":"sp-a","tip":"abc","since":7}]"#);
        let rows = rows_on(&c, &["list", "--state", "CERTIFIED", "--snapshot"]).expect("the client's answer");
        assert_eq!(rows.len(), 1);
        assert_eq!(t.seen.lock().unwrap().as_slice(), &[vec!["list".to_string(), "--state".into(), "CERTIFIED".into(), "--snapshot".into()]]);
    }

    #[test]
    fn a_refused_or_failed_answer_is_cannot_tell_never_empty() {
        assert!(rows_on(&scripted(3, "refused").1, &["list"]).is_none());
        assert!(rows_on(&scripted(2, "cannot tell").1, &["list"]).is_none());
    }
}
