//! `--drift-check` — `drift.sh` finding untracked or modified files in the production
//! checkout, or installed units and drop-ins this install's manifest does not ship. One
//! incident per class, deduped by ref, and not fenrir-exempt: drift that never clears should
//! escalate the longer nobody has looked.

use crate::incident::{self, Finding};
use crate::log::log;

pub enum Probe {
    Clean,
    Drift(String),
    Unreadable,
}

pub fn classify(code: Option<i32>, out: &str, repo: &str) -> Probe {
    match code {
        Some(0) if out.trim().is_empty() => Probe::Clean,
        Some(0) => Probe::Drift(out.lines().map(|l| format!("DIRTY {l} ({repo})\n")).collect()),
        _ => Probe::Unreadable,
    }
}

fn checkout(repo: &str) -> Probe {
    let path = std::path::Path::new(repo);
    if !path.join(".git").exists() {
        return Probe::Unreadable;
    }
    let read = || -> Result<String, String> {
        let o = spira_config::bounded::bounded("git")
            .args(["--no-optional-locks", "-C", repo, "status", "--porcelain"])
            .output()
            .map_err(|e| format!("git status: {e}"))?;
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        match classify(o.status.code(), &out, repo) {
            Probe::Unreadable => Err(format!("git status exited {:?}", o.status.code())),
            _ => Ok(out),
        }
    };
    let held = match spira_config::git_refs::worktree_digest(path) {
        Some(digest) => spira_config::git_refs::held(path, "drift-status", &digest.to_string(), read),
        None => read(),
    };
    match held {
        Ok(out) => classify(Some(0), &out, repo),
        Err(_) => Probe::Unreadable,
    }
}

pub fn probe_of(d: install::unit_drift::UnitDrift) -> Probe {
    match d {
        install::unit_drift::UnitDrift::Clean => Probe::Clean,
        install::unit_drift::UnitDrift::Drift(t) => Probe::Drift(t),
        install::unit_drift::UnitDrift::Unreadable => Probe::Unreadable,
    }
}

pub const UNREADABLE_LIMIT: u32 = 3;

pub fn checkout_target(repo: Option<String>, release: Option<String>) -> Option<String> {
    repo.filter(|r| !r.is_empty()).or_else(|| release.filter(|r| !r.is_empty()))
}

pub fn is_retired_release(target: &str) -> bool {
    let p = std::path::Path::new(target);
    p.is_dir() && !p.join(".git").exists()
}

fn bump(state: &std::path::Path, unreadable: bool) -> u32 {
    if !unreadable {
        let _ = std::fs::remove_file(state);
        return 0;
    }
    let n = std::fs::read_to_string(state).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0) + 1;
    if let Some(dir) = state.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(state, n.to_string());
    n
}

pub fn run(now: i64, repo: &str, db: &str, home_repo: &str, incident_sh: &str, run_dir: &std::path::Path) {
    if !incident::is_usable(incident_sh) {
        log("watchtower: drift-check skipped — incident.sh not readable");
        return;
    }
    let classes: [(&str, &str, String); 2] = [
        (
            "checkout-drift",
            "checkout",
            format!("CHECKOUT DRIFT: {repo} carries untracked or modified files git does not expect"),
        ),
        (
            "unit-drift",
            "units",
            "UNIT DRIFT: the installed unit directory carries files this install does not ship".to_string(),
        ),
    ];
    for (cause, what, title) in classes {
        let state = run_dir.join("conditions").join(format!("drift-unreadable-{what}.count"));
        if what == "checkout" && !repo.is_empty() && is_retired_release(repo) {
            bump(&state, false);
            log(&format!("watchtower: drift-check: checkout half retired — {repo} is a release directory, not a git checkout"));
            continue;
        }
        let probe = match what {
            "units" => probe_of(install::unit_drift::units_drift(&install::unit_drift::default_unit_dir())),
            _ if repo.is_empty() => Probe::Unreadable,
            _ => checkout(repo),
        };
        let n = bump(&state, matches!(probe, Probe::Unreadable));
        match probe {
            Probe::Drift(out) => {
                let mut f = Finding::new(db, home_repo, &title, &out)
                    .reference(format!("incident:{cause}"))
                    .cause(cause);
                f.fenrir_exempt = false;
                incident::alarm_if_due(incident_sh, run_dir, now, &f);
                log(&format!("watchtower: drift-check filed {cause} escalation"));
            }
            Probe::Unreadable => {
                log(&format!("watchtower: drift-check: {what} check could not run ({n} consecutive)"));
                if n >= UNREADABLE_LIMIT {
                    let title = format!("DRIFT CHECK BLIND: the {what} drift check could not run {n} times in a row");
                    let body = format!("the {what} drift check exited without a verdict {n} consecutive runs; the {what} half of watchtower --drift-check is not checking anything.");
                    let f = Finding::new(db, home_repo, &title, &body)
                        .reference(format!("incident:drift-unreadable-{what}"))
                        .cause(format!("drift-unreadable-{what}"));
                    incident::alarm_if_due(incident_sh, run_dir, now, &f);
                    log(&format!("watchtower: drift-check filed {what} unreadable escalation"));
                }
            }
            Probe::Clean => {}
        }
    }
    log("watchtower: drift-check complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tb2febk)]
    #[test]
    fn git_status_output_is_clean_drift_or_unreadable() {
        assert!(matches!(classify(Some(0), "", "/r"), Probe::Clean));
        assert!(matches!(classify(Some(0), "?? x\n", "/r"), Probe::Drift(s) if s == "DIRTY ?? x (/r)\n"));
        for code in [Some(128), Some(124), None] {
            assert!(matches!(classify(code, "", "/r"), Probe::Unreadable));
        }
    }

    #[test_id(tbl7ijn)]
    #[test]
    fn a_directory_with_no_git_is_unreadable() {
        let d = testkit::TempDir::new("no-git");
        assert!(matches!(checkout(&d.path().display().to_string()), Probe::Unreadable));
    }

    #[test_id(t40ifkg)]
    #[test]
    fn target_prefers_repo_then_release() {
        assert_eq!(checkout_target(Some("r".into()), Some("rel".into())).as_deref(), Some("r"));
        assert_eq!(checkout_target(None, Some("rel".into())).as_deref(), Some("rel"));
        assert_eq!(checkout_target(Some(String::new()), None), None);
    }

    use install::unit_drift::units_drift_with;
    use std::collections::BTreeSet;

    fn shipped() -> Result<BTreeSet<String>, String> {
        Ok(["spira-runtime-prod.service", "spira-dolt.timer"].map(String::from).into())
    }

    fn fixture(files: &[&str]) -> testkit::TempDir {
        let d = testkit::TempDir::new("unit-drift");
        for f in files {
            let p = d.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        d
    }

    fn probe(d: &testkit::TempDir, shipped: Result<BTreeSet<String>, String>, local: &str) -> Probe {
        probe_of(units_drift_with(d.path(), shipped, local, "", &|b| b == "sp-gone"))
    }

    #[test_id(tqhu0dw)]
    #[test]
    fn a_shipped_unit_and_a_known_dropin_are_clean() {
        let d = fixture(&["spira-runtime-prod.service", "spira-dolt.timer", "spira-runtime-prod.service.d/cadence.conf"]);
        assert!(matches!(probe(&d, shipped(), ""), Probe::Clean));
    }

    #[test_id(tkzxe5d)]
    #[test]
    fn an_unshipped_unit_is_drift_naming_it() {
        let d = fixture(&["spira-dolt.timer", "foo.service"]);
        let want = format!(
            "UNSHIPPED {}/foo.service (unit file this install does not ship)\ndrift: to accept a file, declare it as a [[unit]] (path, reason, and bead or permanent = true) in the local-units manifest\n",
            d.path().display()
        );
        assert!(matches!(probe(&d, shipped(), ""), Probe::Drift(t) if t == want));
    }

    #[test_id(twm5rw3)]
    #[test]
    fn a_declared_local_unit_is_clean_until_its_bead_lands() {
        let d = fixture(&["foo.service", "bar.service"]);
        let local = "[[unit]]\npath = \"foo.service\"\npermanent = true\n[[unit]]\npath = \"bar.service\"\nbead = \"sp-open\"\n";
        assert!(matches!(probe(&d, shipped(), local), Probe::Clean));
        let landed = "[[unit]]\npath = \"foo.service\"\nbead = \"sp-gone\"\n";
        let d = fixture(&["foo.service"]);
        match probe(&d, shipped(), landed) {
            Probe::Drift(t) => assert!(t.starts_with(&format!("RETIRE-NOW {}/foo.service (declared local override; retiring bead sp-gone has landed)", d.path().display()))),
            _ => panic!("expected drift"),
        }
    }

    #[test_id(tbninzy)]
    #[test]
    fn an_unknown_dropin_is_drift() {
        let d = fixture(&["spira-dolt.timer", "spira-dolt.timer.d/refuse-manual-stop.conf"]);
        let want_line = format!("UNSHIPPED {}/spira-dolt.timer.d/refuse-manual-stop.conf (drop-in no repo mechanism installs)", d.path().display());
        assert!(matches!(probe(&d, shipped(), ""), Probe::Drift(t) if t.lines().next() == Some(want_line.as_str())));
    }

    #[test_id(t8t3h3d)]
    #[test]
    fn an_unreadable_manifest_is_unreadable_never_clean() {
        let d = fixture(&["foo.service"]);
        assert!(matches!(probe(&d, Err("no manifest".into()), ""), Probe::Unreadable));
        assert!(matches!(probe(&d, Ok(BTreeSet::new()), ""), Probe::Unreadable));
    }

    #[test_id(tfg55hk)]
    #[test]
    fn a_declaration_without_a_bead_is_named_and_trailing_comments_are_ignored() {
        let d = fixture(&["foo.service", "ok.service"]);
        let local = "[[unit]]\n  path = \"foo.service\"  # why\n[[unit]]\npath = \"ok.service\" # x\npermanent = true # y\n";
        let want = format!("UNSHIPPED {}/foo.service (declared but names no bead and is not permanent = true)", d.path().display());
        assert!(matches!(probe(&d, shipped(), local), Probe::Drift(t) if t.lines().next() == Some(want.as_str()) && !t.contains("ok.service")));
    }
}
