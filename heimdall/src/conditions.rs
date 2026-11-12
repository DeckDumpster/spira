//! The control-plane conditions heimdall writes its alerts as: `spira-lc condition
//! set|clear|list`, one `conditions/heimdall/<key>` per cause. A trait so the reconciliation
//! logic (`alerts.rs`) can be driven by an in-memory fake in tests.


use serde::Deserialize;
use serde_json::Value;

use crate::util;
use spira_store::lc_call::LcClient;

/// The condition type every alert carries; the pane lists this type.
pub const ALERT: &str = "Alert";
/// The write probe's condition: written False on every pass, so it never reads as firing.
pub const PROBE_KEY: &str = "heimdall-write-probe";
pub const PROBE: &str = "WriteProbe";

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Condition {
    #[serde(default)]
    pub key: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub flaps: u64,
}

impl Condition {
    pub fn firing(&self) -> bool {
        self.status == "True"
    }
}

/// Whether a failed call timed out (contention — "saturated", not "down") or failed outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Saturated,
    Other,
}

pub type R<T> = Result<T, Failure>;

pub trait Conditions: Send + Sync {
    fn list(&self) -> R<Vec<Condition>>;
    /// Writes `key` as `status` (True: firing).
    fn set(&self, key: &str, kind: &str, reason: &str, message: &str, firing: bool) -> R<()>;
    /// Moves a firing `key` to False; nothing to clear is success.
    fn clear(&self, key: &str, reason: &str, message: &str) -> R<()>;
}

/// The conditions through the release's own `spira-lc`, bounded by `timeout`.
/// The store's `condition` verb, in-process over the lifecycle socket.
pub struct LcConditions {
    pub client: LcClient,
}

fn fail_of(code: i32) -> Failure {
    if code == 124 {
        Failure::Saturated
    } else {
        Failure::Other
    }
}

/// A call that got no answer in time is saturation: the socket's own deadline refusal, or a
/// reply lost past it. A store that is not listening at all is not.
fn fail_of_text(code: i32, text: &str) -> Failure {
    if fail_of(code) == Failure::Saturated || text.contains("deadline") || text.contains("timed out") || text.contains("reply was lost") {
        Failure::Saturated
    } else {
        Failure::Other
    }
}

impl LcConditions {
    fn call(&self, args: &[&str]) -> R<String> {
        let mut argv = vec!["condition".to_string()];
        argv.extend(args.iter().map(|s| s.to_string()));
        argv.extend(["--owner".into(), "heimdall".into()]);
        match self.client.call(&argv) {
            Ok(r) if r.code == 0 => Ok(r.out),
            Ok(r) => Err(fail_of_text(r.code, &r.text())),
            Err(e) => Err(fail_of_text(-1, &e.to_string())),
        }
    }
}

/// The rows of a `condition list` answer.
pub fn parse_list(text: &str) -> Option<Vec<Condition>> {
    let v: Value = serde_json::from_str(util::json_only(text).trim()).ok()?;
    let rows = v.get("conditions")?.as_array()?;
    Some(rows.iter().filter_map(|r| serde_json::from_value(r.clone()).ok()).collect())
}

impl Conditions for LcConditions {
    fn list(&self) -> R<Vec<Condition>> {
        parse_list(&self.call(&["list"])?).ok_or(Failure::Other)
    }

    fn set(&self, key: &str, kind: &str, reason: &str, message: &str, firing: bool) -> R<()> {
        let status = if firing { "True" } else { "False" };
        self.call(&["set", key, "--type", kind, "--reason", reason, "--message", message, "--status", status]).map(|_| ())
    }

    fn clear(&self, key: &str, reason: &str, message: &str) -> R<()> {
        self.call(&["clear", key, "--reason", reason, "--message", message]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_answer_is_read_past_a_notice_line() {
        let out = "notice\n{\"rev\":3,\"conditions\":[{\"key\":\"db-down\",\"owner\":\"heimdall\",\"type\":\"Alert\",\"status\":\"True\",\"flaps\":2}]}\n";
        let rows = parse_list(out).unwrap();
        assert_eq!(rows, vec![Condition { key: "db-down".into(), kind: "Alert".into(), status: "True".into(), flaps: 2 }]);
        assert!(rows[0].firing());
        assert!(parse_list("no json").is_none());
    }

    #[test]
    fn a_timeout_is_saturation() {
        assert_eq!(fail_of(124), Failure::Saturated);
        assert_eq!(fail_of(2), Failure::Other);
    }

    struct Store(std::sync::Mutex<Vec<Vec<String>>>, Result<(i32, &'static str), &'static str>);

    impl spira_store::lc_call::Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            self.1.map(|(code, out)| spira_store::lc_call::Reply { code, out: out.into(), err: String::new() }).map_err(String::from)
        }
    }

    fn ops(answer: Result<(i32, &'static str), &'static str>) -> (LcConditions, std::sync::Arc<Store>) {
        let s = std::sync::Arc::new(Store(Default::default(), answer));
        (LcConditions { client: LcClient::over(s.clone() as std::sync::Arc<dyn spira_store::lc_call::Transport>, true) }, s)
    }

    #[test]
    fn conditions_are_read_and_set_over_the_store_client() {
        let (o, s) = ops(Ok((0, r#"{"rev":1,"conditions":[]}"#)));
        assert_eq!(o.list(), Ok(vec![]));
        o.clear("db-down", "ok", "cleared").unwrap();
        let asked = s.0.lock().unwrap();
        assert_eq!(asked[0], ["condition", "list", "--owner", "heimdall"]);
        assert_eq!(asked[1], ["condition", "clear", "db-down", "--reason", "ok", "--message", "cleared", "--owner", "heimdall"]);
    }

    #[test]
    fn a_store_past_its_deadline_is_saturated_and_one_not_listening_is_not() {
        assert_eq!(ops(Ok((2, "cannot tell: the request passed its deadline"))).0.list(), Err(Failure::Saturated));
        assert_eq!(ops(Err("cannot tell: the lifecycle service took the request but its reply was lost (no reply)")).0.list(), Err(Failure::Saturated));
        assert_eq!(ops(Err("cannot tell: the lifecycle service is not answering on /x: refused")).0.list(), Err(Failure::Other));
    }
}
