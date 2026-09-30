//! `--pr-stall-check` — PR-mode stall detector. For each stalled bead, in the order a red
//! check is tested first (the likeliest cause, sp-45rmp): red check -> escalate; allow_auto_
//! merge=false -> escalate once per repo; CONFLICTING -> clear the landstate so landing
//! rebases next pass; otherwise -> arm auto-merge.

use crate::incident::{self, Finding};
use crate::landstate;
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
            gh_timeout: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    EscalateRed,
    EscalateAutoMergeOff,
    ClearConflicting,
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
        Action::ClearConflicting
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
            let text = String::from_utf8_lossy(&o.stdout).trim_end().to_string();
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
    pub file_path: std::path::PathBuf,
}

/// Every `REBASED pr-open:<repo>` landstate record older than `stall_secs`.
pub fn gather_stalled(landstate_dir: &Path, now: i64, cfg: &Cfg) -> Vec<Stalled> {
    let mut out = Vec::new();
    if !landstate_dir.is_dir() {
        return out;
    }
    for rec in landstate::read_dir(landstate_dir) {
        if rec.status != "REBASED" {
            continue;
        }
        let repo = match rec.reason.strip_prefix("pr-open:") {
            Some(r) if !r.is_empty() => r.to_string(),
            _ => continue,
        };
        let at = match rec.at_secs() {
            Some(a) => a,
            None => continue,
        };
        let age = now - at;
        if age < cfg.stall_secs {
            continue;
        }
        out.push(Stalled {
            id: rec.id.clone(),
            repo,
            age_secs: age,
            file_path: landstate_dir.join(&rec.id),
        });
    }
    out
}

pub fn run(now: i64, landstate_dir: &Path, spira_home: &str, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) {
    if !incident::is_usable(incident_sh) {
        log(&format!(
            "watchtower: pr-stall-check skipped — {} not readable",
            incident_sh
        ));
        return;
    }
    for s in gather_stalled(landstate_dir, now, cfg) {
        let repo_path = match seams::repo_root(spira_home, &s.repo) {
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
                incident::file(incident_sh, &f);
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
                incident::file(incident_sh, &f);
                log(&format!(
                    "watchtower: pr-stall-check: {} allow_auto_merge=false (bead {}, age {}s) — escalated",
                    s.repo, s.id, s.age_secs
                ));
            }
            Action::ClearConflicting => {
                let _ = std::fs::remove_file(&s.file_path);
                log(&format!(
                    "watchtower: pr-stall-check: {} in {} is CONFLICTING — cleared landstate to trigger rebase",
                    s.id, s.repo
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

    #[test]
    fn red_outranks_everything_else() {
        assert_eq!(decide(true, true, true), Action::EscalateRed);
    }

    #[test]
    fn auto_merge_off_outranks_conflicting() {
        assert_eq!(decide(false, true, true), Action::EscalateAutoMergeOff);
    }

    #[test]
    fn conflicting_clears_landstate_when_neither_red_nor_auto_merge_off() {
        assert_eq!(decide(false, false, true), Action::ClearConflicting);
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

    #[test]
    fn gather_stalled_only_picks_rebased_pr_open_past_the_threshold() {
        let d = testkit::TempDir::new("wt-pr-stall-gather");
        let ls = d.join("landstate");
        std::fs::create_dir_all(&ls).unwrap();
        let now = 1_700_100_000i64;
        let cfg = Cfg::default();
        // stale: age 7200s >= 3600s threshold
        std::fs::write(
            ls.join("sp-stale"),
            format!("REBASED deadbeef {} pr-open:spira\n", now - 7200),
        )
        .unwrap();
        // fresh: age 60s < threshold
        std::fs::write(
            ls.join("sp-fresh"),
            format!("REBASED deadbeef {} pr-open:spira\n", now - 60),
        )
        .unwrap();
        // wrong status
        std::fs::write(
            ls.join("sp-certified"),
            format!("CERTIFIED deadbeef {} pr-open:spira\n", now - 7200),
        )
        .unwrap();
        // wrong reason
        std::fs::write(
            ls.join("sp-other-reason"),
            format!("REBASED deadbeef {} something-else\n", now - 7200),
        )
        .unwrap();
        let stalled = gather_stalled(&ls, now, &cfg);
        assert_eq!(stalled.len(), 1);
        assert_eq!(stalled[0].id, "sp-stale");
        assert_eq!(stalled[0].repo, "spira");
    }
}
