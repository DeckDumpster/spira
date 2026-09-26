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
use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;

fn spira_home() -> String {
    env::var("SPIRA_HOME").unwrap_or_else(|_| ".".to_string())
}

fn spira_repo() -> String {
    env::var("SPIRA_REPO").unwrap_or_else(|_| ".".to_string())
}

fn mail_sh() -> String {
    format!("{}/mail.sh", spira_home())
}

fn concierge_sh() -> String {
    format!("{}/concierge.sh", spira_repo())
}

fn default_from() -> String {
    "Reconciler <reconciler@spira>".to_string()
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
    let result = match sub {
        Some("gap") => run_gap(&Args::parse(&rest)),
        Some("escalate") => run_escalate(&Args::parse(&rest)),
        _ => Err("usage: reconciler-alert gap|escalate ...".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
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

fn run_gap(args: &Args) -> Result<(), String> {
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
        println!("reconciler-alert: {} — no alert (deduped or not a gap)", invariant);
        return Ok(());
    }

    let short = match &verdict.status {
        RawStatus::Satisfied => "satisfied".to_string(),
        RawStatus::Gap { observed, .. } => observed.clone(),
        RawStatus::Unobservable { reason } => reason.clone(),
    };
    let subject = format!("reconciler: {} — {}", invariant, short);
    let evidence = compose_alert(invariant, now, &verdict, last_remedy);

    if io::concierge_is_running(&concierge_sh()) {
        let body = format!("## Alert\n{subject}\n\n{evidence}");
        io::mail_send(&mail_sh(), "concierge", &from, &subject, "alert", None, &body)?;
        println!("reconciler-alert: {} — sent to concierge", invariant);
    } else {
        let body = format!(
            "## Note\n{subject}\n\nThe Concierge is not running, so this alert is forwarded here as a note.\n\n{evidence}"
        );
        io::mail_send(&mail_sh(), "operator", &from, &subject, "note", None, &body)?;
        println!("reconciler-alert: {} — concierge not running, sent to operator as a note", invariant);
    }
    Ok(())
}

fn run_escalate(args: &Args) -> Result<(), String> {
    let class = args.get("class").ok_or("missing --class")?;
    let subject = args.get("subject").ok_or("missing --subject")?;
    let from = args.get("from").map(str::to_string).unwrap_or_else(default_from);
    let mut body = String::new();
    std::io::stdin().read_to_string(&mut body).map_err(|e| format!("reading body from stdin: {}", e))?;

    match classify_escalation(class) {
        Some(_) => {
            let default = args.get("default").ok_or(
                "escalating to the operator requires --default (law-escalate-decisions-not-problems: every ask carries a default)",
            )?;
            let full_body = format!("## Question\n{subject}\n\n## Default\n{default}\n\n{body}");
            io::mail_send(&mail_sh(), "operator", &from, subject, "question", Some(default), &full_body)?;
            println!("reconciler-alert: escalation ({}) sent to operator", class);
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
            io::mail_send(&mail_sh(), "concierge", &from, &refused_subject, "alert", None, &full_body)?;
            println!("reconciler-alert: escalation class '{}' refused, routed to concierge", class);
        }
    }
    Ok(())
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
}
