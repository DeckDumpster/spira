//! [`Lc`] as the `spira-lc` program, spawned through [`Proc`] with the body on `--json`.

use std::sync::Arc;
use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};
use serde_json::{json, Value};

use crate::ports::Lc;

/// A control-store verb answers in milliseconds; spira-lc abandons a store call at 3 s itself.
pub const LC_DEADLINE: Duration = Duration::from_secs(5);

pub struct SpiraLc {
    pub proc_: Arc<dyn Proc>,
    pub program: String,
}

impl Lc for SpiraLc {
    fn call(&self, args: &[String]) -> (i32, Value) {
        let out = self.proc_.run(&ProcSpec {
            prog: self.program.clone(),
            args: args.to_vec(),
            stdin: Vec::new(),
            deadline: LC_DEADLINE,
        });
        match out {
            Ok(o) => {
                let code = o.code.unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&o.stdout);
                let v = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
                    json!({ "cannot_tell": format!("{} answered non-JSON: {}", self.program, String::from_utf8_lossy(&o.stderr).trim()) })
                });
                (code, v)
            }
            Err(e) => (
                -1,
                json!({ "cannot_tell": format!("{}: {e}", self.program) }),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use runtime::ports::ProcOutput;

    use super::*;

    struct Answers(Mutex<Vec<Result<ProcOutput, String>>>, Mutex<Vec<ProcSpec>>);

    impl Proc for Answers {
        fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
            crate::lock(&self.1).push(spec.clone());
            crate::lock(&self.0).remove(0)
        }
    }

    fn out(code: Option<i32>, stdout: &str, stderr: &str) -> Result<ProcOutput, String> {
        Ok(ProcOutput {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    }

    #[test]
    fn the_answer_is_the_exit_and_its_json_and_anything_else_cannot_tell() {
        let answers = Arc::new(Answers(
            Mutex::new(vec![
                out(Some(3), "{\"refused\":{\"field\":\"status\"}}\n", ""),
                out(Some(0), "not json", "boom"),
                out(None, "", ""),
                Err("spawn failed".into()),
            ]),
            Mutex::default(),
        ));
        let lc = SpiraLc {
            proc_: answers.clone(),
            program: "spira-lc".into(),
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
        assert!(v["cannot_tell"].as_str().unwrap().contains("spawn failed"));
        let spec = &crate::lock(&answers.1)[0];
        assert_eq!(
            (spec.prog.as_str(), spec.args.clone(), spec.deadline),
            ("spira-lc", args, LC_DEADLINE)
        );
    }
}
