// reconciler-alert — the alert path (sp-fufyb): an evidence-carrying wake to the Concierge,
// deduplicated per gap, and an operator escalation path that accepts only permissions,
// policy or destructive/irreversible-on-production-data and refuses everything else by
// construction, routing the refusal back to the Concierge.
//
//   reconciler-alert gap --invariant <key> --now <secs> --state <path> --status <s> [...]
//   reconciler-alert escalate --class <c> --subject <s> [--default <d>] [--from <f>] < body
//
// A future composer (sp-ocmes, sp-rh0x3) links reconciler-engine directly for the pure
// diff-plus-hysteresis logic; this binary is the IO seam a caller invokes once per verdict,
// and what the end-to-end suite drives.

mod io;

use reconciler_engine::alert::{classify_escalation, compose_alert, should_alert};
use reconciler_engine::core::{RawStatus, Verdict};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

/// One message for `mail send`.
#[derive(Debug, Clone, PartialEq)]
struct Message {
    mailbox: &'static str,
    from: String,
    subject: String,
    kind: &'static str,
    default: Option<String>,
    class: Option<String>,
    body: String,
}

/// The alert path's two effects: a message into a mailbox, and whether the Concierge is there
/// to read it. Tests answer both in memory.
trait Wake {
    fn mail(&self, m: &Message) -> Result<(), String>;
    fn concierge_running(&self) -> bool;
}

/// Every program the alert path runs, by name on the launcher's PATH.
struct Seams {
    mail: String,
    tmux: String,
}

impl Seams {
    fn production() -> Seams {
        Seams { mail: "mail".into(), tmux: "tmux".into() }
    }
}

impl Wake for Seams {
    fn mail(&self, m: &Message) -> Result<(), String> {
        io::mail_send(&self.mail, m.mailbox, &m.from, &m.subject, m.kind, m.default.as_deref(), m.class.as_deref(), &m.body)
    }

    fn concierge_running(&self) -> bool {
        io::session_is_running(&self.tmux, CONCIERGE_SESSION)
    }
}

/// The Concierge's tmux session carries its mailbox name.
const CONCIERGE_SESSION: &str = "concierge";

fn default_from() -> String {
    reconciler_engine::mail::FROM.to_string()
}

struct Args(std::collections::HashMap<String, String>, std::collections::HashSet<String>);

impl Args {
    fn parse(argv: &[String]) -> Args {
        let mut values = std::collections::HashMap::new();
        let mut flags = std::collections::HashSet::new();
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            if let Some(name) = a.strip_prefix("--") {
                match FLAG_NAMES.iter().find(|f| **f == name) {
                    Some(_) => {
                        flags.insert(name.to_string());
                        i += 1;
                    }
                    None => {
                        if i + 1 < argv.len() {
                            values.insert(name.to_string(), argv[i + 1].clone());
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                }
            } else {
                i += 1;
            }
        }
        Args(values, flags)
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    fn flag(&self, key: &str) -> bool {
        self.1.contains(key)
    }
}

// The boolean, valueless flags. Every other `--x` is treated as `--x <value>`.
const FLAG_NAMES: &[&str] = &["is-gap", "remedy-failed"];

fn main() -> ExitCode {
    let argv: Vec<String> = env::args().skip(1).collect();
    let sub = argv.first().map(String::as_str);
    let rest: Vec<String> = argv.iter().skip(1).cloned().collect();
    let seams = Seams::production();
    let sink = spira_otel::metrics::MetricsSink::for_component(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "spira-reconciler-alert");
    let result = spira_otel::metrics::instrument_result(&sink, "reconciler-alert", sub.unwrap_or("none"), || match sub {
        Some("gap") => run_gap(&seams, &Args::parse(&rest)),
        Some("escalate") => run_escalate(&seams, &Args::parse(&rest), &mut std::io::stdin()),
        _ => Err("usage: reconciler-alert gap|escalate ...".to_string()),
    });
    match result {
        Ok(outcome) => {
            println!("reconciler-alert: {outcome}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("reconciler-alert: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn parse_status(args: &Args) -> Result<RawStatus, String> {
    match args.get("status") {
        Some("satisfied") => Ok(RawStatus::Satisfied),
        Some("gap") => {
            let desired = args.get("desired").ok_or("gap status requires --desired")?;
            let observed = args.get("observed").ok_or("gap status requires --observed")?;
            Ok(RawStatus::Gap { desired: desired.to_string(), observed: observed.to_string(), since_hint: None })
        }
        Some("unobservable") => {
            let reason = args.get("reason").ok_or("unobservable status requires --reason")?;
            Ok(RawStatus::Unobservable { reason: reason.to_string() })
        }
        Some(other) => Err(format!("unknown --status {} (want satisfied|gap|unobservable)", other)),
        None => Err("missing --status".to_string()),
    }
}

/// One verdict through the alert path; the outcome line says where the wake went, or why none was sent.
fn run_gap(wake: &dyn Wake, args: &Args) -> Result<String, String> {
    let invariant = args.get("invariant").ok_or("missing --invariant")?;
    let now: u64 = args
        .get("now")
        .ok_or("missing --now")?
        .parse()
        .map_err(|_| "--now must be an integer".to_string())?;
    let state_path = PathBuf::from(args.get("state").ok_or("missing --state")?);
    let status = parse_status(args)?;
    let since: Option<u64> = args.get("since").map(|s| s.parse()).transpose().map_err(|_| "--since must be an integer".to_string())?;
    let is_gap = args.flag("is-gap");
    let remedy_failed = args.flag("remedy-failed");
    let last_remedy = args.get("last-remedy");
    let from = args.get("from").map(str::to_string).unwrap_or_else(default_from);

    let verdict = Verdict { status, since, is_gap, just_closed: false, remedy_failed };

    let mut alerted = io::load_alerted(&state_path);
    let prev = alerted.get(invariant).copied();
    let (fire, next) = should_alert(&verdict, prev);
    match next {
        Some(s) => {
            alerted.insert(invariant.to_string(), s);
        }
        None => {
            alerted.remove(invariant);
        }
    }
    io::save_alerted(&state_path, &alerted).map_err(|e| format!("could not persist {}: {}", state_path.display(), e))?;

    if !fire {
        return Ok(format!("{} — no alert (deduped or not a gap)", invariant));
    }

    let short = match &verdict.status {
        RawStatus::Satisfied => "satisfied".to_string(),
        RawStatus::Gap { observed, .. } => observed.clone(),
        RawStatus::Unobservable { reason } | RawStatus::Deliberate { reason } => reason.clone(),
    };
    let subject = format!("reconciler: {} — {}", invariant, short);
    let evidence = compose_alert(invariant, now, &verdict, last_remedy);

    let body = format!("## Alert\n{subject}\n\n{evidence}");
    wake.mail(&Message { mailbox: "concierge", from, subject, kind: "alert", default: None, class: None, body })?;
    if wake.concierge_running() {
        Ok(format!("{} — sent to concierge", invariant))
    } else {
        Ok(format!("{} — concierge not running, queued in its mailbox", invariant))
    }
}

/// One escalation, its body read from `input` once the arguments hold; the outcome line says
/// which mailbox it reached.
fn run_escalate(wake: &dyn Wake, args: &Args, input: &mut dyn std::io::Read) -> Result<String, String> {
    let class = args.get("class").ok_or("missing --class")?;
    let subject = args.get("subject").ok_or("missing --subject")?;
    let from = args.get("from").map(str::to_string).unwrap_or_else(default_from);
    let mut body = String::new();
    input.read_to_string(&mut body).map_err(|e| format!("reading body from stdin: {}", e))?;

    match classify_escalation(class) {
        Some(_) => {
            let default = args.get("default").ok_or(
                "escalating to the operator requires --default (law-escalate-decisions-not-problems: every ask carries a default)",
            )?;
            let full_body = format!("## Question\n{subject}\n\n## Default\n{default}\n\n## Class basis\nEscalated as {class}.\n\n{body}");
            wake.mail(&Message {
                mailbox: "operator",
                from,
                subject: subject.to_string(),
                kind: "question",
                default: Some(default.to_string()),
                class: Some(class.to_string()),
                body: full_body,
            })?;
            Ok(format!("escalation ({}) sent to operator", class))
        }
        None => {
            let refused_subject = format!(
                "escalation refused: '{}' is not permissions, policy or destructive — routed back for judgement: {}",
                class, subject
            );
            let full_body = format!(
                "## Alert\nAn escalation named class '{class}', which is not one of the three the \
                 operator path accepts (permissions, policy, destructive-or-irreversible-on-production-data). \
                 Refused by construction and routed back for your judgement.\n\n{body}"
            );
            wake.mail(&Message {
                mailbox: "concierge",
                from,
                subject: refused_subject,
                kind: "alert",
                default: None,
                class: None,
                body: full_body,
            })?;
            Ok(format!("escalation class '{}' refused, routed to concierge", class))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_parse_reads_valued_flags() {
        let a = Args::parse(&["--invariant".to_string(), "fleet".to_string(), "--now".to_string(), "5".to_string()]);
        assert_eq!(a.get("invariant"), Some("fleet"));
        assert_eq!(a.get("now"), Some("5"));
    }

    #[test]
    fn args_parse_reads_boolean_flags_without_consuming_the_next_value() {
        let a = Args::parse(&["--is-gap".to_string(), "--invariant".to_string(), "x".to_string()]);
        assert!(a.flag("is-gap"));
        assert_eq!(a.get("invariant"), Some("x"));
    }

    #[test]
    fn parse_status_gap_requires_desired_and_observed() {
        let a = Args::parse(&["--status".to_string(), "gap".to_string()]);
        assert!(parse_status(&a).is_err());
        let a = Args::parse(&[
            "--status".to_string(), "gap".to_string(),
            "--desired".to_string(), "d".to_string(),
            "--observed".to_string(), "o".to_string(),
        ]);
        assert_eq!(parse_status(&a), Ok(RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None }));
    }

    #[test]
    fn parse_status_rejects_unknown_values() {
        let a = Args::parse(&["--status".to_string(), "wat".to_string()]);
        assert!(parse_status(&a).is_err());
    }

    // Each recorded message goes through the real mail lint against the tree's kind files, as
    // `mail send` would.

    use std::path::Path;

    /// Records every message and answers "is the Concierge running" with a fixed value.
    struct Recorder {
        concierge_up: bool,
        refuse: bool,
        sent: std::cell::RefCell<Vec<Message>>,
    }

    impl Wake for Recorder {
        fn mail(&self, m: &Message) -> Result<(), String> {
            if self.refuse {
                return Err(format!("mail send {}: exit 1", m.mailbox));
            }
            self.sent.borrow_mut().push(m.clone());
            Ok(())
        }

        fn concierge_running(&self) -> bool {
            self.concierge_up
        }
    }

    struct World {
        dir: testkit::TempDir,
        wake: Recorder,
    }

    impl World {
        /// `concierge_up`: whether the Concierge's session is there. `refuse`: whether mail refuses.
        fn new(concierge_up: bool, refuse: bool) -> World {
            World { dir: testkit::TempDir::new("ralert"), wake: Recorder { concierge_up, refuse, sent: Default::default() } }
        }

        fn gap(&self, argv: &[&str]) -> Result<String, String> {
            let mut a: Vec<String> = vec!["--state".into(), self.dir.join("alerted.json").display().to_string()];
            a.extend(argv.iter().map(|s| s.to_string()));
            run_gap(&self.wake, &Args::parse(&a))
        }

        fn escalate(&self, argv: &[&str], body: &str) -> Result<String, String> {
            let a: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
            run_escalate(&self.wake, &Args::parse(&a), &mut body.as_bytes())
        }

        /// Every message sent so far, in the order sent.
        fn sent(&self) -> Vec<Message> {
            self.wake.sent.borrow().clone()
        }
    }

    /// What `mail send` would say of this message, judged by the tree's own kind files.
    fn mail_lint(m: &Message) -> Result<(), String> {
        let kinds = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/mail/kinds");
        let input = mail::lint::LintInput {
            from: &m.from,
            subject: &m.subject,
            kind: m.kind,
            default: m.default.as_deref().unwrap_or(""),
            urgent: false,
            body: &m.body,
            digest: false,
        };
        mail::lint::lint_check(&input, &kinds, "sp")
    }

    const FLEET_GAP: &[&str] = &[
        "--invariant", "fleet-size", "--status", "gap", "--desired", "8 dwarves", "--observed", "3 dwarves",
        "--since", "940", "--is-gap",
    ];

    fn at(now: &str, argv: &[&str]) -> Vec<String> {
        let mut a = vec!["--now".to_string(), now.to_string()];
        a.extend(argv.iter().map(|s| s.to_string()));
        a
    }

    fn strs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn one_streak_alerts_once_and_a_new_streak_after_it_closes_alerts_again() {
        let w = World::new(true, false);
        w.gap(&strs(&at("1000", FLEET_GAP))).unwrap();
        assert_eq!(w.sent().len(), 1, "the first pass past grace alerts");
        let out = w.gap(&strs(&at("1060", FLEET_GAP))).unwrap();
        assert_eq!(out, "fleet-size — no alert (deduped or not a gap)");
        assert_eq!(w.sent().len(), 1, "the same streak on a later pass sends nothing");

        w.gap(&["--invariant", "fleet-size", "--now", "1100", "--status", "satisfied"]).unwrap();
        let reopened = [
            "--invariant", "fleet-size", "--now", "1200", "--status", "gap", "--desired", "8 dwarves", "--observed", "2 dwarves",
            "--since", "1150", "--is-gap",
        ];
        assert_eq!(w.gap(&reopened).unwrap(), "fleet-size — sent to concierge");
        let sent = w.sent();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(sent[1].body.contains("observed:  2 dwarves"), "{}", sent[1].body);
    }

    #[test]
    fn a_gap_still_inside_its_grace_sends_nothing() {
        let w = World::new(true, false);
        let graced = [
            "--invariant", "cockpit-dashboards", "--now", "3000", "--status", "gap", "--desired", "configured", "--observed", "none",
            "--since", "2990",
        ];
        assert_eq!(w.gap(&graced).unwrap(), "cockpit-dashboards — no alert (deduped or not a gap)");
        assert!(w.sent().is_empty());
    }

    #[test]
    fn an_unobservable_input_past_grace_alerts_as_unobservable_never_as_satisfied() {
        let w = World::new(true, false);
        let blind = [
            "--invariant", "queue-watch", "--now", "4000", "--status", "unobservable", "--reason", "no reading in 90s",
            "--since", "3900", "--is-gap",
        ];
        assert_eq!(w.gap(&blind).unwrap(), "queue-watch — sent to concierge");
        let m = &w.sent()[0];
        assert!(m.body.contains("desired:   (unobservable)") && m.body.contains("observed:  no reading in 90s"), "{}", m.body);
        assert!(!m.body.contains("satisfied"), "{}", m.body);
        mail_lint(m).unwrap();
    }

    #[test]
    fn a_remedy_that_did_not_close_the_gap_is_named_as_failed() {
        let w = World::new(true, false);
        let mut a = at("5000", FLEET_GAP);
        a.extend(["--remedy-failed".to_string(), "--last-remedy".to_string(), "restart the landing unit".to_string()]);
        w.gap(&strs(&a)).unwrap();
        let m = &w.sent()[0];
        assert!(m.body.contains("last remedy tried: restart the landing unit — did not close the gap"), "{}", m.body);
    }

    #[test]
    fn with_the_concierge_not_running_the_alert_still_waits_in_its_mailbox_not_the_operators() {
        let w = World::new(false, false);
        let out = w.gap(&strs(&at("6000", FLEET_GAP))).unwrap();
        assert_eq!(out, "fleet-size — concierge not running, queued in its mailbox");
        let sent = w.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].mailbox, "concierge");
        assert!(sent[0].body.contains("invariant: fleet-size"), "{}", sent[0].body);
    }

    #[test]
    fn a_mail_refusal_is_an_error_never_a_delivered_wake() {
        let w = World::new(true, true);
        let err = w.gap(&strs(&at("1000", FLEET_GAP))).unwrap_err();
        assert!(err.contains("send concierge: exit 1"), "{err}");
    }

    #[test]
    fn an_escalation_without_a_default_is_refused_and_sends_nothing() {
        let w = World::new(true, false);
        let err = w.escalate(&["--class", "policy", "--subject", "s"], "b\n").unwrap_err();
        assert!(err.contains("requires --default"), "{err}");
        assert!(w.sent().is_empty());
    }

    #[test]
    fn an_escalation_in_any_other_class_is_refused_and_routed_back_to_the_concierge_as_an_alert() {
        let w = World::new(true, false);
        let argv = ["--class", "urgent", "--subject", "an unpermitted escalation class", "--default", "d"];
        let out = w.escalate(&argv, "stop filing the same ticket every night\n").unwrap();
        assert_eq!(out, "escalation class 'urgent' refused, routed to concierge");
        let sent = w.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        let m = &sent[0];
        assert_eq!((m.mailbox, m.kind), ("concierge", "alert"));
        assert!(m.subject.contains("'urgent' is not permissions, policy or destructive"), "{}", m.subject);
        assert!(m.body.contains("stop filing the same ticket every night"), "{}", m.body);
        mail_lint(m).expect("the alert kind accepts the refusal this path sends");
    }
}
