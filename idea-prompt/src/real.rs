//! The production ports: the clock, the store's busy read over [`LcClient`], the idea list
//! through the store's door, and the operator mail through the `mail` library.

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
    let bd = mail::bead::BdCli::configured(env.bd_conn_retries).map_err(|e| format!("mail: {e}"))?;
    mail::cmds::send_recorded(&bd, &env, args, body).map(|_| ()).map_err(|e| format!("mail: send: {e}"))
}

/// The label parked ideas carry, resolved once per process; an empty label is an error, never
/// a query about nothing.
fn idea_label() -> Result<&'static str, String> {
    static LABEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    named(LABEL.get_or_init(|| bead::door::IDEA_LABEL.to_string()))
}

fn named(label: &str) -> Result<&str, String> {
    if label.trim().is_empty() {
        return Err("the schema names no idea label".into());
    }
    Ok(label)
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
        let lc = self.lc.as_ref().map_err(Clone::clone)?;
        let label = idea_label()?;
        let rows = bead::door::ideas(lc, label).map_err(|code| format!("idea list exited {code}"))?;
        Ok(rows.into_iter().map(|r| Idea { id: r.id, title: r.title }).collect())
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
            Ok(Reply { code: 0, out: self.0.into(), err: String::new(), read_ended: false })
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

    #[test_id(tkaq4mq)]
    #[test]
    fn busy_is_read_through_the_store_client() {
        let (r, t) = real(r#"[{"bead_id":"a","holds":"[]"}]"#, Arc::default());
        assert_eq!(r.busy(), Ok(true));
        assert_eq!(*t.1.lock().unwrap(), vec![format!("list --state {BUSY_STATES}")]);
        let (r, _) = real(r#"[{"bead_id":"a","holds":["ask"]}]"#, Arc::default());
        assert_eq!(r.busy(), Ok(false));
    }

    #[test_id(tx3b9qd)]
    #[test]
    fn an_empty_idea_label_is_an_error() {
        assert!(named("").is_err() && named(" ").is_err());
        assert_eq!(named("idea"), Ok("idea"));
    }

    #[test_id(tr7c1ws)]
    #[test]
    fn the_idea_list_is_read_over_the_store_client_under_the_idea_label() {
        let (r, t) = real(r#"[{"id":"sp-a","title":"an idea","status":"open","created_at":"2026-01-01T00:00:00Z"}]"#, Arc::default());
        assert_eq!(r.ideas(), Ok(vec![Idea { id: "sp-a".into(), title: "an idea".into() }]));
        let sent = t.1.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "one read per call, no schema lookup: {sent:?}");
        assert!(sent[0].starts_with("content list") && sent[0].contains("--label idea "), "{sent:?}");
    }

    #[test_id(tqhe93q)]
    #[test]
    fn the_prompt_is_mailed_in_process_with_the_cli_flags() {
        let _env = testkit::env(&[("IDEA_PROMPT_MAIL", None)]);
        let sent: Arc<Mutex<Vec<String>>> = Arc::default();
        let (r, _) = real("[]", sent.clone());
        r.send("which idea?", "none", "the body").unwrap();
        assert_eq!(*sent.lock().unwrap(), vec!["operator Spira Ideas <ideas@spira> which idea? question design none|the body".to_string()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render_body, Idea, Ports, DEFAULT_ANSWER};
    use mail::bead::{Bd, BdOut};
    use std::path::PathBuf;

    struct NoStore;

    impl Bd for NoStore {
        fn run(&self, _args: &[String], _stdin: Option<&str>) -> BdOut {
            BdOut::ok("")
        }
        fn close(&self, _id: &str, _reason: &str) -> Result<(), spira_store::lc_call::LcError> {
            Ok(())
        }
        fn ensure_ask(&self, _id: &str, _work_bead: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn env(root: &std::path::Path) -> mail::env::Env {
        mail::env::Env {
            mail_root: root.join("mail"),
            kinds_dir: testkit::manifest_dir().join("../spira/mail/kinds"),
            index_file: root.join("index"),
            mute: false,
            loom_budget_ms: 1000,
            repeat_window_s: 0,
            tidy_fresh_s: 0,
            id_prefix: "sp".into(),
            ask_label: String::new(),
            db: String::new(),
            bd_conn_retries: 1,
            operator_actor: "operator".into(),
            run_dir: root.join("run"),
            home: PathBuf::from(root),
            mail_from: None,
            lint_considered: None,
            repeat_considered: None,
            operator_considered: None,
            statute_guards: String::new(),
            statute_considered: None,
            allow_blocking: false,
            bead_id: None,
            lock_timeout_ms: 1000,
            condition: None,
        }
    }

    #[test_id(tap0ivg)]
    #[test]
    fn the_idle_prompt_is_delivered_to_the_operator() {
        let tmp = testkit::TempDir::new("idea-prompt-send");
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("run")).unwrap();
        let env = env(&root);
        let send_fn: SendFn = Box::new(move |args, body| mail::cmds::send(&NoStore, &env, args, body).map(|_| ()));
        let real = Real::with("UTC".into(), Err("no store".into()), send_fn);
        let body = render_body(&[Idea { id: "sp-a".into(), title: "an idea".into() }]);
        real.send("1 parked idea(s) — start any?", DEFAULT_ANSWER, &body).unwrap();
        assert_eq!(std::fs::read_dir(root.join("mail/operator/new")).unwrap().count(), 1);
    }
}
