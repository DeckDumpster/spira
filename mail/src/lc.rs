//! Lifting a work bead's `ask` hold when its question is answered (sp-v62vn follow-up).
//!
//! `work ask` (and `work blocked`) place `Hold { Ask }` on the asking session's own bead, and
//! only a `Reply` or an `AskWithdrawn` event lifts it — so every path by which an answer
//! arrives has to emit one, or the bead waits forever. mail owns two of those paths: a reply
//! through `sendmail` (aerc's reply, its accept-default, any `In-Reply-To` answer) and the
//! operator's dismissal that `sweep-dismissed` turns into "default taken". The work bead an
//! ask is about travels on the ask mail as `X-Spira-Work-Bead` and in the index's sixth field.
//!
//! A refusal is not a failure here: a bead whose session never held (a question citing
//! another bead, a stale answer to an ask already lifted) has no ask hold to lift, and the
//! machine says so with exit 3 (or 1, no row). Only "cannot tell" is worth a warning.

/// `spira-lc <args>` → (exit code, combined output).
pub trait Lc {
    fn call(&self, args: &[String]) -> (i32, String);
}

/// The verbs the store's socket does not serve: spira-lc's CLI answers them itself, so they
/// still run it as a program. Everything else mail asks goes over [`LcClient`].
pub const UNSERVED: &[&str] = &["state", "reply", "withdraw-ask"];

/// The live service: [`LcClient`] for what the socket serves (and for `close-ask`, which the
/// client itself hands to the CLI), the program for [`UNSERVED`].
pub struct LcCli;

impl Lc for LcCli {
    fn call(&self, args: &[String]) -> (i32, String) {
        let client = spira_store::lc_call::LcClient::configured().map(|c| c.within(spira_store::lc_call::LC_TIMEOUT));
        Routed { client: client.as_ref().map_err(String::as_str), unserved: &LcProgram }.call(args)
    }
}

/// [`Lc`] split by verb: [`UNSERVED`] to `unserved`, the rest through `client` (a client that
/// could not be built answers "cannot tell" with why).
pub struct Routed<'a> {
    pub client: Result<&'a spira_store::lc_call::LcClient, &'a str>,
    pub unserved: &'a dyn Lc,
}

impl Lc for Routed<'_> {
    fn call(&self, args: &[String]) -> (i32, String) {
        if args.first().is_some_and(|v| UNSERVED.contains(&v.as_str())) {
            return self.unserved.call(args);
        }
        match self.client {
            Ok(c) => match c.call(args) {
                Ok(r) => (r.code, r.text()),
                Err(e) => (spira_store::lc_call::CANNOT_TELL, e.to_string()),
            },
            Err(e) => (spira_store::lc_call::CANNOT_TELL, e.to_string()),
        }
    }
}

/// `spira-lc` as a program, found without the caller's PATH, bounded at 15 s here so a
/// stalled machine cannot hold an answer's delivery (124 reads as cannot-tell).
pub struct LcProgram;

impl Lc for LcProgram {
    fn call(&self, args: &[String]) -> (i32, String) {
        let mut c = match spira_store::lc_call::lc_bin() {
            // FORK-EDGE: unserved verb state/reply/withdraw-ask (the socket does not serve callers.rs verbs)
            Ok(b) => std::process::Command::new(b),
            Err(e) => return (2, e),
        };
        c.args(args);
        spira_store::lc_call::run_bounded(c, spira_store::lc_call::LC_TIMEOUT)
    }
}

/// How a question ended: answered (a `Reply`, carrying the answer's message id) or closed
/// without an answer (`AskWithdrawn`).
pub enum Lift<'a> {
    Reply { message_id: &'a str },
    Withdraw,
}

/// Emit the lift for `work_bead`. `Some(warning)` only when the machine could not tell.
pub fn lift_ask(lc: &dyn Lc, work_bead: &str, how: Lift, actor: &str) -> Option<String> {
    if work_bead.is_empty() {
        return None;
    }
    if let (0, state) = lc.call(&["state".into(), work_bead.into()]) {
        if spira_store::lc_state::is_terminal(state.trim()) {
            return None;
        }
    }
    let args: Vec<String> = match how {
        Lift::Reply { message_id } => vec!["reply".into(), work_bead.into(), message_id.into(), actor.into()],
        Lift::Withdraw => vec!["withdraw-ask".into(), work_bead.into(), actor.into()],
    };
    match lc.call(&args) {
        (0, _) | (1, _) | (3, _) => None,
        (code, out) => Some(format!("mail: the ask hold on {work_bead} was not lifted (spira-lc {} exit {code}): {}", args[0], out.trim())),
    }
}

/// Record the operator's answer on the ask's own machine: who, his words, the channel. That
/// close also lifts the `ask` hold on the work bead the ask names. `Some(warning)` only when
/// the machine could not tell; exit 1 (a legacy ask bead, no ask row) and 3 (already closed)
/// are quiet.
pub fn answer_ask(lc: &dyn Lc, ask: &str, quote: &str, actor: &str, channel: &str, message_id: &str) -> Option<String> {
    let args: Vec<String> = ["close-ask", ask, "--exit", "answered", "--quote", quote, "--actor", actor, "--channel", channel, "--message-id", message_id]
        .iter()
        .map(|s| s.to_string())
        .collect();
    match lc.call(&args) {
        (0, _) | (1, _) | (3, _) => None,
        (code, out) => Some(format!("mail: the answer to {ask} was not recorded on the ask machine (spira-lc close-ask exit {code}): {}", out.trim())),
    }
}

/// A dismissal: the ask closes as DEFAULT_TAKEN, quoting the default that was executed.
pub fn take_default(lc: &dyn Lc, ask: &str, default: &str, actor: &str) -> Option<String> {
    let args: Vec<String> = ["close-ask", ask, "--exit", "default", "--quote", default, "--actor", actor].iter().map(|s| s.to_string()).collect();
    match lc.call(&args) {
        (0, _) | (1, _) | (3, _) => None,
        (code, out) => Some(format!("mail: the dismissal of {ask} was not recorded on the ask machine (spira-lc close-ask exit {code}): {}", out.trim())),
    }
}

/// The mailbox-style actor a reply's `From:` names (`Operator <operator@spira>` → `operator`).
pub fn actor_of(from: &str, fallback: &str) -> String {
    let inner = from.rsplit_once('<').map(|(_, r)| r).unwrap_or(from);
    let local = inner.split('@').next().unwrap_or("").trim().trim_end_matches('>');
    if local.is_empty() || local.contains(char::is_whitespace) {
        fallback.to_string()
    } else {
        local.to_string()
    }
}

#[cfg(test)]
pub mod fake {
    use super::Lc;
    use std::sync::Mutex;

    /// Records every write; answers each with `code`, and a `state` read with `state`.
    pub struct FakeLc {
        pub code: i32,
        pub state: String,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl FakeLc {
        pub fn new(code: i32) -> FakeLc {
            FakeLc { code, state: "READY".into(), calls: Mutex::new(Vec::new()) }
        }
        pub fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Lc for FakeLc {
        fn call(&self, args: &[String]) -> (i32, String) {
            if args[0] == "state" {
                return (0, self.state.clone());
            }
            self.calls.lock().unwrap().push(args.to_vec());
            (self.code, String::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeLc;
    use super::*;

    #[test]
    fn a_reply_names_the_work_bead_and_the_answers_message_id() {
        let lc = FakeLc::new(0);
        assert_eq!(lift_ask(&lc, "sp-w1", Lift::Reply { message_id: "m-1@spira" }, "operator"), None);
        assert_eq!(lc.calls(), vec![vec!["reply", "sp-w1", "m-1@spira", "operator"]]);
    }

    #[test]
    fn no_hold_or_no_row_is_quiet_but_cannot_tell_warns() {
        for quiet in [1, 3] {
            assert_eq!(lift_ask(&FakeLc::new(quiet), "sp-w1", Lift::Withdraw, "a"), None);
        }
        assert!(lift_ask(&FakeLc::new(2), "sp-w1", Lift::Withdraw, "a").unwrap().contains("sp-w1"));
        assert!(FakeLc::new(0).calls().is_empty());
        let lc = FakeLc::new(0);
        assert_eq!(lift_ask(&lc, "", Lift::Withdraw, "a"), None);
        assert!(lc.calls().is_empty(), "no work bead, no call");
    }

    #[test]
    fn an_answer_is_recorded_with_who_his_words_and_the_channel() {
        let lc = FakeLc::new(0);
        assert_eq!(answer_ask(&lc, "sp-a", "yes", "operator", "mail", "m-1@spira"), None);
        assert_eq!(
            lc.calls(),
            vec![vec!["close-ask", "sp-a", "--exit", "answered", "--quote", "yes", "--actor", "operator", "--channel", "mail", "--message-id", "m-1@spira"]]
        );
        assert!(answer_ask(&FakeLc::new(2), "sp-a", "y", "o", "mail", "m").unwrap().contains("sp-a"));
        assert_eq!(answer_ask(&FakeLc::new(1), "sp-a", "y", "o", "mail", "m"), None);
    }

    struct Scripted {
        seen: std::sync::Mutex<Vec<Vec<String>>>,
        reply: Result<spira_store::lc_call::Reply, String>,
    }

    impl spira_store::lc_call::Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            self.seen.lock().unwrap().push(argv.to_vec());
            self.reply.clone()
        }
    }

    fn client(reply: Result<spira_store::lc_call::Reply, String>) -> (std::sync::Arc<Scripted>, spira_store::lc_call::LcClient) {
        let t = std::sync::Arc::new(Scripted { seen: Default::default(), reply });
        (t.clone(), spira_store::lc_call::LcClient::over(t, false))
    }

    /// sp-ipwlic: an answer's close-ask goes through the store client, never a program of its
    /// own; only the verbs the socket does not serve reach the program.
    #[test]
    fn close_ask_goes_through_the_client_and_unserved_verbs_through_the_program() {
        let (t, c) = client(Ok(spira_store::lc_call::Reply { code: 0, ..Default::default() }));
        let program = FakeLc::new(0);
        let lc = Routed { client: Ok(&c), unserved: &program };
        assert_eq!(answer_ask(&lc, "sp-a", "yes", "operator", "mail", "m-1"), None);
        assert_eq!(lift_ask(&lc, "sp-w1", Lift::Withdraw, "operator"), None);
        assert_eq!(t.seen.lock().unwrap().iter().map(|a| a[0].clone()).collect::<Vec<_>>(), vec!["close-ask"]);
        assert_eq!(program.calls(), vec![vec!["withdraw-ask", "sp-w1", "operator"]]);
    }

    #[test]
    fn a_client_with_no_answer_is_cannot_tell_and_a_refusal_keeps_its_code() {
        let (_, down) = client(Err("cannot tell: not answering".into()));
        let program = FakeLc::new(0);
        let lc = Routed { client: Ok(&down), unserved: &program };
        assert!(answer_ask(&lc, "sp-a", "y", "o", "mail", "m").unwrap().contains("exit 2"));
        let (_, refused) = client(Ok(spira_store::lc_call::Reply { code: 3, out: "already closed".into(), err: String::new() }));
        assert_eq!(answer_ask(&Routed { client: Ok(&refused), unserved: &program }, "sp-a", "y", "o", "mail", "m"), None);
        let unbuilt = Routed { client: Err("SPIRA_LC_SOCKET does not resolve"), unserved: &program };
        assert!(answer_ask(&unbuilt, "sp-a", "y", "o", "mail", "m").unwrap().contains("SPIRA_LC_SOCKET"));
    }

    #[test]
    fn actor_of_reads_the_local_part() {
        assert_eq!(actor_of("Operator <operator@spira>", "x"), "operator");
        assert_eq!(actor_of("concierge@spira", "x"), "concierge");
        assert_eq!(actor_of("", "operator"), "operator");
    }
}
