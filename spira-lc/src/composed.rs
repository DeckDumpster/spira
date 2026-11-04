//! The composed verbs: each one a composition of the primitives with its planning-store (bd)
//! half — the closes, reopens and drops, the ask settle, the holds and the other caller verbs,
//! and the bead-content door. lc-serve answers them over its socket; the CLI is a client of
//! that answer, and runs this same function itself only in declared direct mode (a fixture
//! with no service). One implementation, so the socket and the CLI cannot drift apart.
//!
//! Two caller verbs stay with the caller: `deliver` and `content-on-base` prove a landing from
//! git in the caller's own checkout, which the service user never reads. The socket refuses
//! them like any verb it does not serve.

use crate::callers::{self, Answer, Bd, Machine};

/// The caller verbs proved from the caller's git checkout: never served.
pub const CALLER_SIDE: &[&str] = &["deliver", "content-on-base"];

/// The composed verbs other than the caller verbs.
const OWN: &[&str] = &[
    "unclaim",
    "reconcile-closed",
    "close-ask",
    "reconcile-asks",
    "reconcile-epics",
    "reconcile-content",
    "drop-orphans",
    "close",
    "drop-bead",
    "drop-row",
    "reopen",
    "bd-supersede",
    "bd-note",
    "content",
    "close-epic",
];

/// Sweeps over every row: batch jobs, bounded at the query deadline rather than a call's 3 s.
pub const SWEEPS: &[&str] = &["reconcile-asks", "reconcile-epics", "reconcile-content", "drop-orphans"];

/// Whether `verb` is answered by [`run`].
pub fn is_served(verb: &str) -> bool {
    OWN.contains(&verb) || (callers::is_verb(verb) && !CALLER_SIDE.contains(&verb))
}

/// The bead-content store (bd): reads, the narrow writes `bd-note`/`bd-supersede` make, and the
/// alert purge's delete.
pub trait Content {
    fn read(&self, args: &[String]) -> Result<String, String>;
    fn write(&self, args: &[&str]) -> Result<String, String>;
    fn delete(&self, ids: &[String]) -> Result<(), String>;
}

/// bd itself, as this process's configuration declares it.
pub struct LiveContent;

impl Content for LiveContent {
    fn read(&self, args: &[String]) -> Result<String, String> {
        crate::bd::run_stdin(args, None)
    }
    fn write(&self, args: &[&str]) -> Result<String, String> {
        crate::bd::write(args)
    }
    fn delete(&self, ids: &[String]) -> Result<(), String> {
        match crate::bd::tool("bdq", &[&["delete".to_string()][..], ids, &["--force".to_string()]].concat(), None, "purge-alerts", 120) {
            (0, _) => Ok(()),
            (rc, out) => Err(format!("bdq delete exited {rc}: {out}")),
        }
    }
}

/// Everything a composed verb reaches: the primitives, bd's bead half, the content store, the
/// request's stdin (`close --reason-file -`), and the run directory (`content purge-alerts`).
pub struct Ports<'a> {
    pub m: &'a mut dyn Machine,
    pub bd: &'a mut dyn Bd,
    pub content: &'a dyn Content,
    pub stdin: &'a mut dyn FnMut() -> Result<String, String>,
    pub run_dir: &'a dyn Fn() -> Result<std::path::PathBuf, String>,
    pub now: i64,
}

/// Answer one composed verb (`argv[0]` is the verb).
pub fn run(argv: &[String], p: Ports<'_>) -> Answer {
    let verb = argv.first().map(String::as_str).unwrap_or("");
    let rest = argv.get(1..).unwrap_or(&[]);
    let Ports { m, bd, content, stdin, run_dir, now } = p;
    match verb {
        "unclaim" => callers::unclaim(rest, m),
        "reconcile-closed" => callers::reconcile_closed(rest, m, bd),
        // The ask's state is written first; its decision bead then follows through reconcile-content.
        "close-ask" => {
            let (code, out) = m.call(argv);
            let mut ans = if spira_config::lc_call::is_refusal(code, &out) {
                Answer { code, stderr: line(&out), ..Default::default() }
            } else {
                Answer::out(code, out)
            };
            if code == 0 {
                if let Some(id) = rest.first() {
                    if let Err(e) = crate::reconcile_content::settle_ask(m, bd, id) {
                        ans.stderr.push_str(&format!("spira-lc close-ask: {id} is closed; its decision bead waits for reconcile-content: {}\n", e.trim()));
                    }
                }
            }
            ans
        }
        "reconcile-asks" => crate::reconcile_content::reconcile_asks(rest, m, bd),
        "reconcile-epics" => callers::reconcile_epics(rest, m, bd),
        "reconcile-content" => callers::reconcile_content(rest, m, bd),
        "drop-orphans" => callers::drop_orphans(rest, m, bd),
        "close" => {
            let mut read = |f: &str| -> Result<String, String> {
                if f == "-" {
                    stdin().map_err(|e| format!("stdin: {e}"))
                } else {
                    std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))
                }
            };
            callers::close(rest, m, bd, &mut read)
        }
        "drop-bead" => callers::drop_bead(rest, m, bd),
        "drop-row" => callers::drop_row(rest, m, bd),
        "reopen" => callers::reopen_cmd(rest, m, bd),
        "bd-supersede" => crate::store_write::supersede(rest, &|a| content.write(a)),
        "bd-note" => crate::store_write::note(rest, &|a| content.write(a)),
        "content" if rest.first().map(String::as_str) == Some("purge-alerts") => match run_dir() {
            Ok(dir) => crate::content::purge_alerts(&rest[1..], &|a| content.read(a), &|ids| content.delete(ids), &dir, now),
            Err(e) => Answer { code: callers::CANNOT_TELL, stderr: format!("spira-lc content purge-alerts: {e}\n"), ..Default::default() },
        },
        "content" => crate::content::run(rest, &|a| content.read(a)),
        "close-epic" => callers::close_epic(rest, m, bd),
        "drop" => callers::drop_cmd(rest, m, bd),
        v if is_served(v) => callers::run(v, rest, m),
        v => Answer { code: callers::CANNOT_TELL, stderr: format!("cannot tell: {v} is not a composed verb\n"), ..Default::default() },
    }
}

fn line(s: &str) -> String {
    if s.ends_with('\n') { s.to_string() } else { format!("{s}\n") }
}

/// The certification log line an answer carries (certify/resubmit only), appended to
/// `$SPIRA_RUN/lifecycle-cert.log` in lifecycle-cert.sh's shape: `<epoch> <verb> bead=<id> <detail>`.
/// `id` is the verb's first argument.
pub fn write_cert_log(id: &str, ans: &Answer, run_dir: Result<std::path::PathBuf, String>, now: i64) -> Option<String> {
    use std::io::Write;
    let (what, detail) = ans.cert_log.as_ref()?;
    match run_dir {
        Ok(dir) => {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("lifecycle-cert.log"))
                .and_then(|mut f| writeln!(f, "{now} {what} bead={id} {detail}"));
            None
        }
        Err(e) => Some(format!("spira-lc: {e} — lifecycle-cert.log not written")),
    }
}

/// What the CLI sends for a composed verb: `close --reason-file <file>` reads the caller's file
/// here, as the caller, and sends it as the request body (`--reason-file -`), so the service
/// never reads a path of the caller's; `-` sends this process's stdin. A file that cannot be read
/// is sent as named, so the service's answer is the refusal the verb has always given.
pub fn client_request(argv: &[String], stdin: &mut dyn FnMut() -> Result<String, String>) -> (Vec<String>, Option<String>) {
    let mut argv = argv.to_vec();
    if argv.first().map(String::as_str) != Some("close") {
        return (argv, None);
    }
    let Some(i) = argv.iter().position(|a| a == "--reason-file").map(|i| i + 1).filter(|&i| i < argv.len()) else {
        return (argv, None);
    };
    let body = if argv[i] == "-" { stdin().ok() } else { std::fs::read_to_string(&argv[i]).ok() };
    match body {
        Some(b) => {
            argv[i] = "-".into();
            (argv, Some(b))
        }
        None => (argv, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_git_proved_caller_verbs_stay_with_the_caller_and_every_other_composed_verb_is_served() {
        for v in ["deliver", "content-on-base", "show", "event", "close-on-land", "serve", "admin-migrate", "unpublished"] {
            assert!(!is_served(v), "{v}");
        }
        for v in ["close", "reopen", "reconcile-closed", "close-ask", "hold", "unhold", "drop", "disposition", "held", "reply", "content", "certify"] {
            assert!(is_served(v), "{v}");
        }
    }

    #[test]
    fn a_reason_file_is_read_by_the_caller_and_sent_as_the_body() {
        let dir = testkit::TempDir::new("lc-composed-reason");
        let f = dir.join("why");
        std::fs::write(&f, "landed: abc").unwrap();
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut none = || Err::<String, String>("no stdin".into());
        let (argv, body) = client_request(&v(&["close", "sp-a", "--reason-file", f.to_str().unwrap()]), &mut none);
        assert_eq!((argv, body), (v(&["close", "sp-a", "--reason-file", "-"]), Some("landed: abc".into())));
        let mut piped = || Ok::<String, String>("from stdin".into());
        assert_eq!(client_request(&v(&["close", "sp-a", "--reason-file", "-"]), &mut piped).1.as_deref(), Some("from stdin"));
        let missing = v(&["close", "sp-a", "--reason-file", "/no/such/file"]);
        assert_eq!(client_request(&missing, &mut none), (missing.clone(), None), "an unreadable file is sent as named; the service refuses it");
        assert_eq!(client_request(&v(&["hold", "sp-a", "wait"]), &mut piped).1, None, "only close sends a body");
    }
}
