//! `--drift-check` — `drift.sh` finding untracked or modified files in the production
//! checkout, or installed units and drop-ins this install's manifest does not ship. One
//! incident per class, deduped by ref, and not sin-exempt: drift that never clears should
//! escalate the longer nobody has looked.

use crate::incident::{self, Finding};
use crate::log::log;
use std::process::Command;

pub enum Probe {
    Clean,
    Drift(String),
    Unreadable,
}

pub fn classify(code: Option<i32>, out: &str) -> Probe {
    match code {
        Some(0) => Probe::Clean,
        Some(1) => Probe::Drift(out.to_string()),
        _ => Probe::Unreadable,
    }
}

fn drift(drift_sh: &str, args: &[&str]) -> Probe {
    // batch-job: runs a gate, build or forge script that takes as long as its work
    match Command::new("bash").envs(spira_config::release_env::child_path_env_for_process()).arg(drift_sh).args(args).output() {
        Ok(o) => classify(o.status.code(), &String::from_utf8_lossy(&o.stdout)),
        Err(_) => Probe::Unreadable,
    }
}

pub fn run(spira_home: &str, repo: &str, db: &str, home_repo: &str, incident_sh: &str) {
    let drift_sh = format!("{spira_home}/drift.sh");
    if !incident::is_usable(incident_sh) || !std::path::Path::new(&drift_sh).is_file() {
        log("watchtower: drift-check skipped — incident.sh or drift.sh not readable");
        return;
    }
    let classes: [(&str, &[&str], &str, String); 2] = [
        (
            "checkout-drift",
            &["checkout", repo],
            "checkout",
            format!("CHECKOUT DRIFT: {repo} carries untracked or modified files git does not expect"),
        ),
        (
            "unit-drift",
            &["units"],
            "units",
            "UNIT DRIFT: the installed unit directory carries files this install does not ship".to_string(),
        ),
    ];
    for (cause, args, what, title) in classes {
        match drift(&drift_sh, args) {
            Probe::Drift(out) => {
                let mut f = Finding::new(db, home_repo, &title, &out)
                    .reference(format!("incident:{cause}"))
                    .cause(cause);
                f.sin_exempt = false;
                incident::alarm(incident_sh, &f);
                log(&format!("watchtower: drift-check filed {cause} escalation"));
            }
            Probe::Unreadable => log(&format!("watchtower: drift-check: {what} check could not run")),
            Probe::Clean => {}
        }
    }
    log("watchtower: drift-check complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_one_is_drift_and_anything_else_unrecognised_is_unreadable() {
        assert!(matches!(classify(Some(0), ""), Probe::Clean));
        assert!(matches!(classify(Some(1), "DIRTY x"), Probe::Drift(s) if s == "DIRTY x"));
        assert!(matches!(classify(Some(3), ""), Probe::Unreadable));
        assert!(matches!(classify(None, ""), Probe::Unreadable));
    }
}
