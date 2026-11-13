//! [`Lc`] through the store's one door ([`LcClient`]): the service's socket in-process, or the
//! configured `spira-lc` program when the config declares a stand-in or direct mode.

use std::time::Duration;

use serde_json::{json, Value};
use spira_store::lc_call::LcClient;

use crate::ports::Lc;

/// A control-store verb answers in milliseconds; spira-lc abandons a store call at 3 s itself.
pub const LC_DEADLINE: Duration = Duration::from_secs(5);

pub struct SpiraLc {
    pub client: LcClient,
}

impl Lc for SpiraLc {
    fn call(&self, args: &[String]) -> (i32, Value) {
        match self.client.within(LC_DEADLINE).call(args) {
            Ok(r) => {
                let v = serde_json::from_str(r.out.trim()).unwrap_or_else(|_| {
                    json!({ "cannot_tell": format!("spira-lc answered non-JSON: {}", r.text()) })
                });
                (r.code, v)
            }
            Err(e) => (-1, json!({ "cannot_tell": format!("spira-lc: {e}") })),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use spira_store::lc_call::{Reply, Transport};

    use super::*;

    pub(crate) struct Answers(
        pub Mutex<Vec<Result<Reply, String>>>,
        pub Mutex<Vec<(Vec<String>, Option<String>)>>,
    );

    impl Transport for Answers {
        fn exchange(
            &self,
            argv: &[String],
            stdin: Option<&str>,
            _d: Duration,
        ) -> Result<Reply, String> {
            crate::lock(&self.1).push((argv.to_vec(), stdin.map(str::to_string)));
            crate::lock(&self.0).remove(0)
        }
    }

    fn out(code: i32, stdout: &str, stderr: &str) -> Result<Reply, String> {
        Ok(Reply {
            code,
            out: stdout.into(),
            err: stderr.into(),
        })
    }

    #[test]
    fn the_answer_is_the_exit_and_its_json_and_anything_else_cannot_tell() {
        let answers = Arc::new(Answers(
            Mutex::new(vec![
                out(3, "{\"refused\":{\"field\":\"status\"}}\n", ""),
                out(0, "not json", "boom"),
                out(-1, "", ""),
                Err("cannot tell: the lifecycle service is not answering".into()),
            ]),
            Mutex::default(),
        ));
        let lc = SpiraLc {
            client: LcClient::over(answers.clone(), true),
        };
        let args = vec![
            "section".to_string(),
            "get".into(),
            "RoundController/r".into(),
        ];
        let (code, v) = lc.call(&args);
        assert_eq!((code, v["refused"]["field"].as_str()), (3, Some("status")));
        let (code, v) = lc.call(&args);
        assert_eq!(code, 0);
        assert!(v["cannot_tell"].as_str().unwrap().contains("boom"), "{v}");
        assert_eq!(lc.call(&args).0, -1, "a run with no exit code");
        let (code, v) = lc.call(&args);
        assert_eq!(code, -1);
        assert!(v["cannot_tell"].as_str().unwrap().contains("not answering"));
        let seen = crate::lock(&answers.1);
        assert_eq!(seen[0], (args, None), "the argv as given, nothing on stdin");
    }
}
