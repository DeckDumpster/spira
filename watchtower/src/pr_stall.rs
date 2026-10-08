//! `--pr-stall-check` — PR-mode stall detector. For each stalled bead, in the order a red
//! check is tested first (the likeliest cause, sp-45rmp): red check -> escalate; allow_auto_
//! merge=false -> escalate once per repo; CONFLICTING -> requeue the delivery so landing
//! rebases next pass; otherwise -> arm auto-merge.

use crate::incident::{self, Finding};
use crate::lc;
use crate::log::log;
use crate::seams;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub struct Cfg {
    pub stall_secs: i64,
    pub gh_bin: String,
    pub gh_timeout: Duration,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            stall_secs: 60 * 60,
            gh_bin: "gh".to_string(),
            gh_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    EscalateRed,
    EscalateAutoMergeOff,
    RequeueConflicting,
    ArmAutoMerge,
}

const RED_CONCLUSIONS: [&str; 5] = ["FAILURE", "CANCELLED", "TIMED_OUT", "STALE", "ACTION_REQUIRED"];

/// `concls` is the comma-joined `statusCheckRollup[].conclusion` list the bash's `--jq`
/// produced. Red iff any token exactly matches one of the five failing conclusions —
/// mirroring the bash's `,${concls},` / `*,X,*` wrap-and-match, not a substring search.
pub fn is_red(concls: &str) -> bool {
    concls
        .split(',')
        .any(|c| RED_CONCLUSIONS.contains(&c))
}

/// The pure decision — red outranks auto-merge-off outranks CONFLICTING outranks arming,
/// exactly the bash's if/elif chain.
pub fn decide(red: bool, allow_auto_merge_false: bool, mergeable_conflicting: bool) -> Action {
    if red {
        Action::EscalateRed
    } else if allow_auto_merge_false {
        Action::EscalateAutoMergeOff
    } else if mergeable_conflicting {
        Action::RequeueConflicting
    } else {
        Action::ArmAutoMerge
    }
}

struct PrFacts {
    mergeable: String,
    concls: String,
    allow_auto_merge: String,
}

fn gh_pr_facts(cfg: &Cfg, repo_path: &str, bead_id: &str) -> PrFacts {
    let branch = format!("spira/{bead_id}");
    let out = Command::new("timeout")
        .arg(format!("{}", cfg.gh_timeout.as_secs()))
        .arg(&cfg.gh_bin)
        .args(["pr", "view", &branch, "--json", "mergeable,statusCheckRollup", "--jq",
            "[(.mergeable // \"\"), ((.statusCheckRollup // []) | map(.conclusion // \"\") | join(\",\"))] | join(\"\\t\")"])
        .current_dir(repo_path)
        .output();
    let (mergeable, concls) = match out {
        Ok(o) if o.status.success() => {
            // Only the trailing newline, never `trim_end()`: an empty conclusions list
            // leaves the tab as the last character before it, and `trim_end()` treats a
            // tab as whitespace too — stripping it collapses `"CONFLICTING\t"` to
            // `"CONFLICTING"`, and `split_once('\t')` then finds no tab to split on at
            // all, silently discarding a real (empty) value instead of parsing it.
            let text = String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string();
            match text.split_once('\t') {
                Some((a, b)) => (a.to_string(), b.to_string()),
                None => (String::new(), String::new()),
            }
        }
        _ => (String::new(), String::new()),
    };
    let aam_out = Command::new("timeout")
        .arg(format!("{}", cfg.gh_timeout.as_secs()))
        .arg(&cfg.gh_bin)
        .args(["repo", "view", "--json", "allowAutoMerge", "--jq", ".allowAutoMerge"])
        .current_dir(repo_path)
        .output();
    let allow_auto_merge = match aam_out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    };
    PrFacts {
        mergeable,
        concls,
        allow_auto_merge,
    }
}

pub struct Stalled {
    pub id: String,
    pub repo: String,
    pub age_secs: i64,
    pub version: String,
}

/// Every pr-mode delivery in PR_OPEN for longer than `stall_secs`, with the registry repo
/// whose checkout carries its branch. A row no repo carries is skipped, as an unregistered
/// repo always was. `None` is spira-lc unreachable.
pub fn gather_stalled(reg: &spira_config::repos::Registry, now: i64, cfg: &Cfg) -> Option<Vec<Stalled>> {
    let mut out = Vec::new();
    for row in lc::pr_open()? {
        if row.mode != "pr" {
            continue;
        }
        let Some(at) = row.entered_at else { continue };
        let age = now - at;
        if age < cfg.stall_secs {
            continue;
        }
        let refname = format!("refs/heads/spira/{}", row.id);
        let repo = reg.all().into_iter().find(|name| {
            reg.root(name).map(|root| crate::git::ref_exists(Some(&root), &refname)).unwrap_or(false)
        });
        if let Some(repo) = repo {
            out.push(Stalled { id: row.id, repo, age_secs: age, version: row.version });
        }
    }
    Some(out)
}

pub fn run(now: i64, spira_home: &str, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) {
    if !incident::is_usable(incident_sh) {
        log(&format!(
            "watchtower: pr-stall-check skipped — {} not readable",
            incident_sh
        ));
        return;
    }
    let reg = seams::registry(spira_home);
    let Some(stalled) = gather_stalled(&reg, now, cfg) else {
        log("watchtower: pr-stall-check skipped — spira-lc is unreachable");
        return;
    };
    if stalled.is_empty() {
        return;
    }
    for s in stalled {
        let repo_path = match reg.root(&s.repo) {
            Some(p) if Path::new(&p).join(".git").exists() => p,
            _ => continue,
        };
        let facts = gh_pr_facts(cfg, &repo_path, &s.id);
        let red = is_red(&facts.concls);
        let aam_false = facts.allow_auto_merge == "false";
        let conflicting = facts.mergeable == "CONFLICTING";
        let age_mins = s.age_secs / 60;

        match decide(red, aam_false, conflicting) {
            Action::EscalateRed => {
                let body = format!(
                    "PR stall: bead {} in repo {} has been waiting {} minutes with a failing check.\n\nAuto-merge cannot fire while checks are red, so arming it is a no-op. Fix or override the failing check on this pull request.\n",
                    s.id, s.repo, age_mins
                );
                let f = Finding::new(
                    db,
                    home_repo,
                    &format!("PR STALL: {} in {} has a failing check — auto-merge cannot fire", s.id, s.repo),
                    &body,
                )
                .priority(1)
                .reference(format!("incident:pr-stall-checks-red:{}:{}", s.repo, s.id))
                .cause("pr-stall-checks-red");
                incident::alarm(incident_sh, &f);
                log(&format!(
                    "watchtower: pr-stall-check: {} in {} has a failing check (age {}s) — escalated",
                    s.id, s.repo, s.age_secs
                ));
            }
            Action::EscalateAutoMergeOff => {
                let body = format!(
                    "PR stall: bead {} in repo {} has been waiting {} minutes.\n\nThe repository has allow_auto_merge=false. Auto-merge can never fire until it is enabled.\n\nEnable it: GitHub → repository Settings → General → Allow auto-merge.\n\nBead {} will remain stalled until this is enabled.\n",
                    s.id, s.repo, age_mins, s.id
                );
                let f = Finding::new(
                    db,
                    home_repo,
                    &format!("PR STALL: {} allow_auto_merge=false — enable it to unblock", s.repo),
                    &body,
                )
                .priority(1)
                .reference(format!("incident:pr-stall-auto-merge-off:{}", s.repo))
                .cause("pr-stall-auto-merge-off");
                incident::alarm(incident_sh, &f);
                log(&format!(
                    "watchtower: pr-stall-check: {} allow_auto_merge=false (bead {}, age {}s) — escalated",
                    s.repo, s.id, s.age_secs
                ));
            }
            Action::RequeueConflicting => {
                let requeued = lc::requeue_pr_open(&s.id, &s.version);
                log(&format!(
                    "watchtower: pr-stall-check: {} in {} is CONFLICTING — {}",
                    s.id,
                    s.repo,
                    if requeued { "requeued to trigger rebase" } else { "requeue refused or spira-lc unreachable" }
                ));
            }
            Action::ArmAutoMerge => {
                let ok = Command::new("timeout")
                    .arg(format!("{}", cfg.gh_timeout.as_secs()))
                    .arg(&cfg.gh_bin)
                    .args(["pr", "merge", "--auto", "--squash", &format!("spira/{}", s.id)])
                    .current_dir(&repo_path)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if ok {
                    log(&format!(
                        "watchtower: pr-stall-check: armed auto-merge for {} in {}",
                        s.id, s.repo
                    ));
                } else {
                    log(&format!(
                        "watchtower: pr-stall-check: could not arm auto-merge for {} in {} (age {}s)",
                        s.id, s.repo, s.age_secs
                    ));
                }
            }
        }
    }
    log("watchtower: pr-stall-check complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scar, caught by testenv (sp-lnmbq): a fake `gh` reporting CONFLICTING with an EMPTY
    /// conclusions list leaves the tab as the string's last character before the newline;
    /// `trim_end()` treats that tab as whitespace too and strips it, so `split_once('\t')`
    /// then finds nothing to split on and both fields come back empty — CONFLICTING was
    /// silently discarded and the branch fell through to arming auto-merge instead of
    /// requeueing the delivery. Exercises the real subprocess path, not just `decide()`.
    #[test]
    fn gh_pr_facts_parses_conflicting_with_an_empty_conclusions_list() {
        let d = testkit::TempDir::new("wt-prstall-ghfacts");
        let gh = d.join("gh-stub.sh");
        // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
        testkit::write_exe(
            &gh,
            "#!/usr/bin/env bash\ncase \"$*\" in\n  *'--jq .allowAutoMerge'*) echo true ;;\n  *'pr view'*'--json mergeable,statusCheckRollup'*) printf 'CONFLICTING\\t\\n' ;;\nesac\n",
        );
        let cfg = Cfg {
            stall_secs: 3600,
            gh_bin: gh.to_string_lossy().into_owned(),
            gh_timeout: std::time::Duration::from_secs(5),
        };
        let facts = gh_pr_facts(&cfg, d.to_str().unwrap(), "sp-x");
        assert_eq!(facts.mergeable, "CONFLICTING");
        assert_eq!(facts.concls, "");
    }

    #[test]
    fn red_outranks_everything_else() {
        assert_eq!(decide(true, true, true), Action::EscalateRed);
    }

    #[test]
    fn auto_merge_off_outranks_conflicting() {
        assert_eq!(decide(false, true, true), Action::EscalateAutoMergeOff);
    }

    #[test]
    fn conflicting_requeues_when_neither_red_nor_auto_merge_off() {
        assert_eq!(decide(false, false, true), Action::RequeueConflicting);
    }

    #[test]
    fn otherwise_arms_auto_merge() {
        assert_eq!(decide(false, false, false), Action::ArmAutoMerge);
    }

    #[test]
    fn is_red_matches_exact_tokens_not_substrings() {
        assert!(is_red("FAILURE"));
        assert!(is_red("SUCCESS,FAILURE"));
        assert!(is_red("CANCELLED,SUCCESS"));
        assert!(!is_red("SUCCESS,SUCCESS"));
        assert!(!is_red(""));
        // A conclusion string that merely CONTAINS "FAILURE" as a substring of a longer
        // token must not match — tokens are compared whole.
        assert!(!is_red("NOTAFAILURE"));
    }
}
