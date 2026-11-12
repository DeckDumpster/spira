//! The production ports: the clock, the store's busy read over [`LcClient`], the idea list
//! through `bead.sh`, and the operator mail through the `mail` library.

use crate::{Idea, Ports};
use spira_store::lc_call::LcClient;
use std::io::Write;
use std::process::{Command, Stdio};

const BUSY_STATES: &str = "READY,REWORK,WORKING,SUBMITTED,CERTIFIED,IN_DELIVERY";

/// One operator mail, sent in-process through the `mail` library.
pub type SendFn = Box<dyn Fn(&mail::cmds::SendArgs, String) -> Result<(), String>>;

pub struct Real {
    pub tz: String,
    lc: Result<LcClient, String>,
    send_fn: SendFn,
}

impl Real {
    /// Production: the configured store client and the `mail` library.
    pub fn new(tz: String) -> Real {
        Real::with(tz, LcClient::configured(), Box::new(send_in_process))
    }

    pub fn with(tz: String, lc: Result<LcClient, String>, send_fn: SendFn) -> Real {
        Real { tz, lc, send_fn }
    }
}

/// `mail send`'s own path: the config, the bead store's door, then the recorded send.
fn send_in_process(args: &mail::cmds::SendArgs, body: String) -> Result<(), String> {
    let env = mail::env::Env::load().map_err(|e| format!("mail: {e}"))?;
    mail::maildir::mailbox_valid(args.mailbox).map_err(|e| format!("mail: send: {e}"))?;
    let bdq = spira_config::release_env::tool("bdq").map_err(|e| format!("mail: {e}"))?;
    let bd = mail::bead::BdCli { bdq, conn_retries: env.bd_conn_retries };
    mail::cmds::send_recorded(&bd, &env, args, body).map(|_| ()).map_err(|e| format!("mail: send: {e}"))
}

fn env_tool(var: &str, default: &str) -> String {
    std::env::var(var).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| default.into())
}

impl Ports for Real {
    fn now(&self) -> u64 {
        if let Some(n) = std::env::var("SPIRA_NOW").ok().and_then(|v| v.parse().ok()) {
            return n;
        }
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }

    fn local_hour(&self, now: u64) -> Result<u32, String> {
        // not a Spira edge: needs a tz-database library to read the operator's zone in-process
        let out = spira_config::bounded::bounded("date")
            .env("TZ", &self.tz)
            .args(["-d", &format!("@{now}"), "+%H"])
            .output()
            .map_err(|e| e.to_string())?;
        String::from_utf8_lossy(&out.stdout).trim().parse().map_err(|_| format!("date gave no hour for TZ={}", self.tz))
    }

    fn busy(&self) -> Result<bool, String> {
        let lc = self.lc.as_ref().map_err(Clone::clone)?;
        let out = lc.call(&["list", "--state", BUSY_STATES]).map_err(String::from)?;
        if out.code != 0 {
            return Err(format!("spira-lc list exited {}", out.code));
        }
        let rows: serde_json::Value = serde_json::from_str(&out.out).map_err(|e| format!("spira-lc list is not JSON: {e}"))?;
        let rows = rows.as_array().ok_or("spira-lc list is not an array")?;
        Ok(rows.iter().any(|r| !has_holds(r.get("holds"))))
    }

    fn ideas(&self) -> Result<Vec<Idea>, String> {
        // FORK-EDGE: unserved verb content (bead.sh ideas, the operator tool's idea list)
        let out = spira_config::bounded::bounded(env_tool("IDEA_PROMPT_BEAD", "bead.sh"))
            .args(["ideas", "--json"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("bead.sh ideas exited {}", out.status));
        }
        let rows: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("bead.sh ideas is not JSON: {e}"))?;
        let text = |r: &serde_json::Value, k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        Ok(rows.as_array().ok_or("bead.sh ideas is not an array")?.iter().map(|r| Idea { id: text(r, "id"), title: text(r, "title") }).collect())
    }

    fn send(&self, subject: &str, default: &str, body: &str) -> Result<(), String> {
        let from = "Spira Ideas <ideas@spira>";
        let Some(stand_in) = std::env::var("IDEA_PROMPT_MAIL").ok().filter(|s| !s.is_empty()) else {
            let args = mail::cmds::SendArgs {
                mailbox: "operator",
                from: Some(from),
                subject,
                kind: "question",
                default,
                class: "design",
                bead: "",
                urgent: false,
                digest: false,
                dry_run: false,
                requested_by: "",
            };
            return (self.send_fn)(&args, body.to_string()).map_err(|e| format!("mail send operator: {e}"));
        };
        // batch-job: mail send delivers to the operator mailbox and runs for as long as that takes
        // FORK-EDGE: a fixture's IDEA_PROMPT_MAIL stand-in only; production sends in-process
        let mut child = Command::new(stand_in)
            .args(["send", "operator", "--from", from, "--subject", subject, "--kind", "question", "--class", "design", "--default", default])
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("mail send operator: {e}"))?;
        child.stdin.take().ok_or("mail send operator: no stdin")?.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        let st = child.wait().map_err(|e| e.to_string())?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("mail send operator exited {st}"))
        }
    }
}

fn has_holds(v: Option<&serde_json::Value>) -> bool {
    match v {
        Some(serde_json::Value::String(s)) => !matches!(s.trim(), "" | "[]" | "null"),
        Some(serde_json::Value::Array(a)) => !a.is_empty(),
        _ => false,
    }
}


#[cfg(test)]
mod real_tests {
    use super::*;
    use spira_store::lc_call::{Reply, Transport};
    use std::sync::{Arc, Mutex};

    struct Scripted(&'static str, Mutex<Vec<String>>);

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<Reply, String> {
            self.1.lock().unwrap().push(argv.join(" "));
            Ok(Reply { code: 0, out: self.0.into(), err: String::new() })
        }
    }

    fn real(rows: &'static str, sent: Arc<Mutex<Vec<String>>>) -> (Real, Arc<Scripted>) {
        let t = Arc::new(Scripted(rows, Mutex::default()));
        let send: SendFn = Box::new(move |a: &mail::cmds::SendArgs, body: String| {
            sent.lock().unwrap().push(format!("{} {} {} {} {} {}|{body}", a.mailbox, a.from.unwrap_or(""), a.subject, a.kind, a.class, a.default));
            Ok(())
        });
        (Real::with("UTC".into(), Ok(LcClient::over(t.clone(), true)), send), t)
    }

    #[test]
    fn busy_is_read_through_the_store_client() {
        let (r, t) = real(r#"[{"bead_id":"a","holds":"[]"}]"#, Arc::default());
        assert_eq!(r.busy(), Ok(true));
        assert_eq!(*t.1.lock().unwrap(), vec![format!("list --state {BUSY_STATES}")]);
        let (r, _) = real(r#"[{"bead_id":"a","holds":["ask"]}]"#, Arc::default());
        assert_eq!(r.busy(), Ok(false));
    }

    #[test]
    fn the_prompt_is_mailed_in_process_with_the_cli_flags() {
        let _env = testkit::env(&[("IDEA_PROMPT_MAIL", None)]);
        let sent: Arc<Mutex<Vec<String>>> = Arc::default();
        let (r, _) = real("[]", sent.clone());
        r.send("which idea?", "none", "the body").unwrap();
        assert_eq!(*sent.lock().unwrap(), vec!["operator Spira Ideas <ideas@spira> which idea? question design none|the body".to_string()]);
    }
}
