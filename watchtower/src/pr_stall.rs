//! `--pr-stall-check` — PR-mode stall detector. For each stalled bead, in the order a red
//! check is tested first (the likeliest cause, sp-45rmp): red check -> escalate; allow_auto_
//! merge=false -> escalate once per repo; CONFLICTING -> requeue the delivery so landing
//! rebases next pass; otherwise -> arm auto-merge.

use crate::incident::{self, Finding};
use crate::lc;
use crate::log::log;
use crate::seams;
use spira_store::lc_call::LcClient;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub struct Cfg {
    pub stall_secs: i64,
    pub gh_bin: String,
    pub gh_timeout: Duration,
}

impl Cfg {
    /// The check's config from the configured stall threshold in minutes.
    pub fn from_mins(stall_mins: i64, gh_bin: String, gh_timeout: Duration) -> Cfg {
        Cfg { stall_secs: stall_mins * 60, gh_bin, gh_timeout }
    }
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

/// Anything but a literal `true`/`false` is unknown — a failed read or a null field must
/// never be taken for `false`.
fn parse_bool(text: &str) -> Option<bool> {
    match text.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

struct PrFacts {
    mergeable: String,
    concls: String,
    allow_auto_merge: Option<bool>,
}

fn gh_counted(cfg: &Cfg, verb: &str, repo_path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    forge::spend::counted_output(&forge::telemetry::sink(), &format!("watchtower:{verb}"), args, || {
        Command::new("timeout").arg(format!("{}", cfg.gh_timeout.as_secs())).arg(&cfg.gh_bin).args(args).current_dir(repo_path).output()
    })
}

fn gh_pr_facts(cfg: &Cfg, repo_path: &str, bead_id: &str) -> PrFacts {
    let branch = format!("spira/{bead_id}");
    let out = gh_counted(cfg, "pr-stall", repo_path, &["pr", "view", &branch, "--json", "mergeable,statusCheckRollup", "--jq",
        "[(.mergeable // \"\"), ((.statusCheckRollup // []) | map(.conclusion // \"\") | join(\",\"))] | join(\"\\t\")"]);
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
    let aam_out = gh_counted(cfg, "pr-stall", repo_path, &["api", "repos/{owner}/{repo}", "--jq", ".allow_auto_merge"]);
    let allow_auto_merge = match aam_out {
        Ok(o) if o.status.success() => parse_bool(&String::from_utf8_lossy(&o.stdout)),
        _ => None,
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
pub fn gather_stalled(reg: &spira_config::repos::Registry, lc: &LcClient, now: i64, cfg: &Cfg) -> Option<Vec<Stalled>> {
    let repo_of = |id: &str| {
        let refname = format!("refs/heads/spira/{id}");
        reg.all().into_iter().find(|name| reg.root(name).map(|root| crate::git::ref_exists(Some(&root), &refname)).unwrap_or(false))
    };
    Some(stalled_in(lc::pr_open_on(lc)?, &repo_of, now, cfg.stall_secs))
}

/// The PR_OPEN rows that are pr-mode, older than `stall_secs`, and whose branch some repo carries.
pub fn stalled_in(rows: Vec<lc::DeliveryRow>, repo_of: &dyn Fn(&str) -> Option<String>, now: i64, stall_secs: i64) -> Vec<Stalled> {
    rows.into_iter()
        .filter(|row| row.mode == "pr")
        .filter_map(|row| {
            let age_secs = now - row.entered_at?;
            if age_secs < stall_secs {
                return None;
            }
            let repo = repo_of(&row.id)?;
            Some(Stalled { id: row.id, repo, age_secs, version: row.version })
        })
        .collect()
}

/// Beads CERTIFIED for longer than `stall_secs` in a pr-mode repo with no PR_OPEN delivery:
/// the landing pass never opened their pull request. Pure over its inputs.
pub fn unopened(
    certified: &[lc::BeadRow],
    open_ids: &[String],
    repo_of: &dyn Fn(&str) -> Option<String>,
    is_pr: &dyn Fn(&str) -> bool,
    now: i64,
    stall_secs: i64,
) -> Vec<(String, String, i64)> {
    certified
        .iter()
        .filter(|b| !open_ids.contains(&b.id))
        .filter_map(|b| {
            let age = now - b.since?;
            if age < stall_secs {
                return None;
            }
            let repo = repo_of(&b.id)?;
            is_pr(&repo).then(|| (b.id.clone(), repo, age))
        })
        .collect()
}

fn escalate_unopened(reg: &spira_config::repos::Registry, now: i64, act: &Acting, raise: &mut dyn FnMut(&Finding)) {
    let Acting { lc, cfg, db, home_repo } = *act;
    let (Some(certified), Some(open)) = (lc::beads_in_on(lc, "CERTIFIED"), lc::pr_open_on(lc)) else { return };
    let open_ids: Vec<String> = open.into_iter().map(|r| r.id).collect();
    let repo_of = |id: &str| {
        let refname = format!("refs/heads/spira/{id}");
        reg.all().into_iter().find(|n| reg.root(n).map(|r| crate::git::ref_exists(Some(&r), &refname)).unwrap_or(false))
    };
    let is_pr = |repo: &str| reg.land(repo) == "pr";
    for (id, repo, age) in unopened(&certified, &open_ids, &repo_of, &is_pr, now, (30 * 60).min(cfg.stall_secs)) {
        let body = format!(
            "PR stall: bead {id} in pr-mode repo {repo} has been CERTIFIED for {} minutes and no pull request is open.\n\nThe landing pass opens it; check that its walk is not held (SPIRA_BATCHER_FORGE_ENABLE) and that the branch exists in the repository checkout.\n",
            age / 60
        );
        let f = Finding::new(db, home_repo, &format!("PR STALL: {id} in {repo} is certified with no pull request"), &body)
            .priority(1)
            .reference(format!("incident:pr-stall-unopened:{repo}:{id}"))
            .cause("pr-stall-unopened");
        raise(&f);
        log(&format!("watchtower: pr-stall-check: {id} in {repo} certified {age}s with no pull request — escalated"));
    }
}

pub fn run(now: i64, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) {
    if !incident::is_usable(incident_sh) {
        log(&format!(
            "watchtower: pr-stall-check skipped — {} not readable",
            incident_sh
        ));
        return;
    }
    let Some(lc) = lc::client() else {
        log("watchtower: pr-stall-check skipped — spira-lc is unreachable");
        return;
    };
    let registry = match seams::registry() {
        Ok(r) => r,
        Err(e) => {
            log(&format!("watchtower: pr-stall-check skipped — {e}"));
            return;
        }
    };
    pass(&registry, now, &Acting { lc: &lc, cfg, db, home_repo }, &mut |f| {
        incident::alarm(incident_sh, f);
    });
}

/// One check pass over `reg`: certified-but-unopened escalations, then each stalled PR handled
/// by its repo's facts. Nothing is filed, requeued or armed when the store cannot answer.
fn pass(reg: &spira_config::repos::Registry, now: i64, act: &Acting, raise: &mut dyn FnMut(&Finding)) {
    escalate_unopened(reg, now, act, raise);
    let Some(stalled) = gather_stalled(reg, act.lc, now, act.cfg) else {
        log("watchtower: pr-stall-check skipped — spira-lc is unreachable");
        return;
    };
    for s in stalled {
        let repo_path = match reg.root(&s.repo) {
            Some(p) if Path::new(&p).join(".git").exists() => p,
            _ => continue,
        };
        let facts = gh_pr_facts(act.cfg, &repo_path, &s.id);
        if facts.allow_auto_merge.is_none() {
            log(&format!(
                "watchtower: pr-stall-check: allow_auto_merge unknown for {} — read failed or field absent",
                s.repo
            ));
        }
        handle(&s, &repo_path, &facts, act, raise);
    }
    log("watchtower: pr-stall-check complete");
}

/// The red-checks escalation for `s`; one per bead, its ref scoped by repo and bead.
fn red_finding(s: &Stalled, db: &str, home_repo: &str) -> Finding {
    let body = format!(
        "PR stall: bead {} in repo {} has been waiting {} minutes with a failing check.\n\nAuto-merge cannot fire while checks are red, so arming it is a no-op. Fix or override the failing check on this pull request.\n",
        s.id, s.repo, s.age_secs / 60
    );
    Finding::new(db, home_repo, &format!("PR STALL: {} in {} has a failing check — auto-merge cannot fire", s.id, s.repo), &body)
        .priority(1)
        .reference(format!("incident:pr-stall-checks-red:{}:{}", s.repo, s.id))
        .cause("pr-stall-checks-red")
}

/// The auto-merge-off escalation for `s`; one per repo, whichever bead found it.
fn auto_merge_off_finding(s: &Stalled, db: &str, home_repo: &str) -> Finding {
    let body = format!(
        "PR stall: bead {} in repo {} has been waiting {} minutes.\n\nThe repository has allow_auto_merge=false. Auto-merge can never fire until it is enabled.\n\nEnable it: GitHub → repository Settings → General → Allow auto-merge.\n\nBead {} will remain stalled until this is enabled.\n",
        s.id, s.repo, s.age_secs / 60, s.id
    );
    Finding::new(db, home_repo, &format!("PR STALL: {} allow_auto_merge=false — enable it to unblock", s.repo), &body)
        .priority(1)
        .reference(format!("incident:pr-stall-auto-merge-off:{}", s.repo))
        .cause("pr-stall-auto-merge-off")
}

/// What the check does for one stalled bead given the repo's facts: escalate through `raise`,
/// requeue through `lc`, or arm auto-merge through the configured `gh`.
/// The store, config and filing identity one check pass acts with.
struct Acting<'a> {
    lc: &'a LcClient,
    cfg: &'a Cfg,
    db: &'a str,
    home_repo: &'a str,
}

fn handle(s: &Stalled, repo_path: &str, facts: &PrFacts, act: &Acting, raise: &mut dyn FnMut(&Finding)) {
    let Acting { lc, cfg, db, home_repo } = *act;
    let red = is_red(&facts.concls);
    let conflicting = facts.mergeable == "CONFLICTING";
    match decide(red, facts.allow_auto_merge == Some(false), conflicting) {
        Action::EscalateRed => {
            raise(&red_finding(s, db, home_repo));
            log(&format!("watchtower: pr-stall-check: {} in {} has a failing check (age {}s) — escalated", s.id, s.repo, s.age_secs));
        }
        Action::EscalateAutoMergeOff => {
            raise(&auto_merge_off_finding(s, db, home_repo));
            log(&format!("watchtower: pr-stall-check: {} allow_auto_merge=false (bead {}, age {}s) — escalated", s.repo, s.id, s.age_secs));
        }
        Action::RequeueConflicting => {
            let requeued = lc::requeue_pr_open_on(lc, &s.id, &s.version);
            log(&format!(
                "watchtower: pr-stall-check: {} in {} is CONFLICTING — {}",
                s.id,
                s.repo,
                if requeued { "requeued to trigger rebase" } else { "requeue refused or spira-lc unreachable" }
            ));
        }
        Action::ArmAutoMerge => {
            if arm_auto_merge(cfg, repo_path, &s.id) {
                log(&format!("watchtower: pr-stall-check: armed auto-merge for {} in {}", s.id, s.repo));
            } else {
                log(&format!("watchtower: pr-stall-check: could not arm auto-merge for {} in {} (age {}s)", s.id, s.repo, s.age_secs));
            }
        }
    }
}

fn arm_auto_merge(cfg: &Cfg, repo_path: &str, bead_id: &str) -> bool {
    Command::new("timeout")
        .arg(format!("{}", cfg.gh_timeout.as_secs()))
        .arg(&cfg.gh_bin)
        .args(["pr", "merge", "--auto", "--squash", &format!("spira/{bead_id}")])
        .current_dir(repo_path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tuzt8ww)]
    #[test]
    fn a_certified_bead_with_no_pull_request_is_a_stall_only_in_a_pr_repo() {
        let row = |id: &str, since| lc::BeadRow { id: id.into(), tip: "t".into(), since: Some(since) };
        let certified = [row("sp-old", 0), row("sp-new", 3000), row("sp-open", 0), row("sp-q", 0)];
        let repo_of = |id: &str| Some(if id == "sp-q" { "qrepo" } else { "prrepo" }.to_string());
        let is_pr = |r: &str| r == "prrepo";
        let got = unopened(&certified, &["sp-open".to_string()], &repo_of, &is_pr, 3600, 1800);
        assert_eq!(got, vec![("sp-old".to_string(), "prrepo".to_string(), 3600)]);
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(tk6ncfn)]
    #[test]
    fn red_outranks_everything_else() {
        assert_eq!(decide(true, true, true), Action::EscalateRed);
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(taexdwx)]
    #[test]
    fn auto_merge_off_outranks_conflicting() {
        assert_eq!(decide(false, true, true), Action::EscalateAutoMergeOff);
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(ttqfaul)]
    #[test]
    fn conflicting_requeues_when_neither_red_nor_auto_merge_off() {
        assert_eq!(decide(false, false, true), Action::RequeueConflicting);
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(tae81w7)]
    #[test]
    fn otherwise_arms_auto_merge() {
        assert_eq!(decide(false, false, false), Action::ArmAutoMerge);
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(tczgebd)]
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

    use crate::lc::scripted::scripted_by;

    fn stalled(id: &str) -> Stalled {
        Stalled { id: id.into(), repo: "testrepo".into(), age_secs: 5400, version: "3".into() }
    }

    fn facts(mergeable: &str, concls: &str, allow_auto_merge: Option<bool>) -> PrFacts {
        PrFacts { mergeable: mergeable.into(), concls: concls.into(), allow_auto_merge }
    }

    /// A `gh` that logs its argv and answers the two reads `gh_pr_facts` makes.
    fn stub_gh(d: &std::path::Path, aam: &str, view: &str) -> Cfg {
        let gh = d.join("gh");
        let log = d.join("gh.log");
        testkit::write_exe(
            &gh,
            &format!(
                "#!/bin/sh\necho \"$*\" >> {}\ncase \"$*\" in\n  *'api repos/'*) printf '%s\\n' '{aam}' ;;\n  *'pr view'*) printf '{view}\\n' ;;\nesac\n",
                log.display()
            ),
        );
        Cfg { stall_secs: 3600, gh_bin: gh.to_str().unwrap().into(), gh_timeout: Duration::from_secs(5) }
    }

    fn gh_calls(d: &std::path::Path) -> String {
        std::fs::read_to_string(d.join("gh.log")).unwrap_or_default()
    }

    fn acted(s: &Stalled, f: &PrFacts, lc: &LcClient, cfg: &Cfg, repo_path: &str) -> Vec<Finding> {
        let mut raised = Vec::new();
        handle(s, repo_path, f, &Acting { lc, cfg, db: "db", home_repo: "spira" }, &mut |x| raised.push(x.clone()));
        raised
    }

    fn no_store() -> LcClient {
        scripted_by(&[]).1
    }

    // covers: UC-ops-detection-remediation-27 UC-ops-detection-remediation-28
    #[test_id(tjhexvl)]
    #[test]
    fn a_stalled_pr_in_a_repo_with_auto_merge_off_files_one_stall_scoped_to_the_repo() {
        let d = testkit::TempDir::new("wt-prs-aam-off");
        let cfg = stub_gh(&d, "false", "");
        let off = facts("MERGEABLE", "", Some(false));
        let first = acted(&stalled("sp-testA"), &off, &no_store(), &cfg, d.to_str().unwrap());
        let second = acted(&stalled("sp-testB"), &off, &no_store(), &cfg, d.to_str().unwrap());
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].reference.as_deref(), Some("incident:pr-stall-auto-merge-off:testrepo"));
        assert_eq!(first[0].reference, second[0].reference, "two stalled beads of one repo are one incident");
        assert_eq!(first[0].cause.as_deref(), Some("pr-stall-auto-merge-off"));
        assert!(first[0].title.contains("PR STALL") && first[0].title.contains("testrepo"));
        assert!(!gh_calls(&d).contains("pr merge"), "a repo that cannot auto-merge is not asked to");
    }

    // covers: UC-ops-detection-remediation-27 UC-ops-detection-remediation-28
    #[test_id(tgl8y0c)]
    #[test]
    fn a_failing_check_is_filed_per_bead_and_arming_is_not_attempted() {
        let d = testkit::TempDir::new("wt-prs-red");
        let cfg = stub_gh(&d, "true", "");
        let red = facts("CONFLICTING", "SUCCESS,FAILURE", Some(false));
        let raised = acted(&stalled("sp-testR"), &red, &no_store(), &cfg, d.to_str().unwrap());
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].reference.as_deref(), Some("incident:pr-stall-checks-red:testrepo:sp-testR"));
        assert_eq!(raised[0].cause.as_deref(), Some("pr-stall-checks-red"));
        assert!(gh_calls(&d).is_empty());
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(trczogt)]
    #[test]
    fn a_stalled_pr_in_a_repo_with_auto_merge_on_is_armed_and_nothing_is_filed() {
        let d = testkit::TempDir::new("wt-prs-arm");
        let cfg = stub_gh(&d, "true", "");
        let raised = acted(&stalled("sp-test2"), &facts("MERGEABLE", "SUCCESS", Some(true)), &no_store(), &cfg, d.to_str().unwrap());
        assert!(raised.is_empty());
        assert_eq!(gh_calls(&d).trim(), "pr merge --auto --squash spira/sp-test2");
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(tx64uv4)]
    #[test]
    fn a_conflicting_pr_is_requeued_as_the_harness_with_its_version_and_not_armed_or_filed() {
        let d = testkit::TempDir::new("wt-prs-conflict");
        let cfg = stub_gh(&d, "true", "");
        let (t, lc) = scripted_by(&[("show sp-testC", 0, r#"{"bead":{"tip":"abc1234"}}"#), ("event delivery", 0, "")]);
        let raised = acted(&stalled("sp-testC"), &facts("CONFLICTING", "", Some(true)), &lc, &cfg, d.to_str().unwrap());
        assert!(raised.is_empty());
        assert!(gh_calls(&d).is_empty(), "arming a conflicting PR cannot help");
        let seen = t.seen.lock().unwrap();
        let event = seen.iter().find(|a| a[0] == "event").expect("the delivery event");
        assert_eq!(event[..8], ["event", "delivery", "sp-testC", "--expect", "PR_OPEN", "--version", "3", "--actor"]);
        assert_eq!(event[8], "harness");
        assert!(event[10].contains("Requeued") && event[10].contains("abc1234"), "{}", event[10]);
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(to7jyaa)]
    #[test]
    fn a_read_gh_could_not_make_is_unknown_and_is_never_taken_for_auto_merge_off() {
        let d = testkit::TempDir::new("wt-prs-unknown");
        let mut cfg = stub_gh(&d, "not-a-bool", "MERGEABLE\\tSUCCESS");
        let read = gh_pr_facts(&cfg, d.to_str().unwrap(), "sp-x");
        assert_eq!((read.mergeable.as_str(), read.concls.as_str(), read.allow_auto_merge), ("MERGEABLE", "SUCCESS", None));
        assert_eq!(decide(is_red(&read.concls), read.allow_auto_merge == Some(false), false), Action::ArmAutoMerge);
        cfg.gh_bin = "does/not/exist/gh".into();
        let none = gh_pr_facts(&cfg, d.to_str().unwrap(), "sp-x");
        assert_eq!((none.mergeable.as_str(), none.allow_auto_merge), ("", None));
    }

    // covers: UC-ops-detection-remediation-27 UC-ops-detection-remediation-28
    #[test_id(tmiwk7x)]
    #[test]
    fn only_stale_pr_mode_deliveries_whose_branch_some_repo_carries_are_stalled() {
        let row = |id: &str, mode: &str, at: i64| lc::DeliveryRow { id: id.into(), mode: mode.into(), entered_at: Some(at), version: "3".into() };
        let rows = vec![row("sp-stale", "pr", 100), row("sp-fresh", "pr", 3500), row("sp-queue", "queue", 100), row("sp-nowhere", "pr", 100), lc::DeliveryRow { entered_at: None, ..row("sp-undated", "pr", 0) }];
        let repo_of = |id: &str| (id != "sp-nowhere").then(|| "testrepo".to_string());
        let got = stalled_in(rows, &repo_of, 3700, 3600);
        assert_eq!(got.iter().map(|s| (s.id.as_str(), s.repo.as_str(), s.age_secs)).collect::<Vec<_>>(), vec![("sp-stale", "testrepo", 3600)]);
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(tgv3o2a)]
    #[test]
    fn the_stall_scan_asks_the_store_for_pr_open_deliveries_only() {
        let (t, lc) = scripted_by(&[("list", 0, r#"[{"bead_id":"sp-a","mode":"pr","entered_at":5,"version":"3"}]"#)]);
        let rows = lc::pr_open_on(&lc).expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(t.seen.lock().unwrap()[0], ["list", "--delivery", "--state", "PR_OPEN"]);
        assert!(lc::pr_open_on(&scripted_by(&[("list", 2, "cannot tell")]).1).is_none(), "an unreachable store is not an empty one");
    }

    /// A pr-mode repo `testrepo` whose checkout carries the branches `spira/<id>` for `ids`, as a registry.
    fn world(d: &std::path::Path, ids: &[&str]) -> spira_config::repos::Registry {
        let repo = d.join("testrepo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(&repo).output().unwrap().status.success(), "{args:?}");
        git(&["init", "-q"]);
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "base"]);
        for id in ids {
            git(&["branch", &format!("spira/{id}")]);
        }
        let map = format!("testrepo|{}|pr|origin/main||\n", repo.display());
        spira_config::repos::Registry::new(Some(&map), &Default::default(), d)
    }

    fn passed(reg: &spira_config::repos::Registry, lc: &LcClient, cfg: &Cfg) -> Vec<Finding> {
        let mut raised = Vec::new();
        pass(reg, 10_000, &Acting { lc, cfg, db: "db", home_repo: "spira" }, &mut |f| raised.push(f.clone()));
        raised
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(t5zn0r6)]
    #[test]
    fn a_pass_over_a_stale_pr_in_an_auto_merge_off_repo_files_the_repo_scoped_stall() {
        let d = testkit::TempDir::new("wt-prs-pass");
        let reg = world(d.path(), &["sp-stale", "sp-fresh"]);
        let cfg = stub_gh(&d, "false", "MERGEABLE\\tSUCCESS");
        let rows = r#"[{"bead_id":"sp-stale","mode":"pr","entered_at":4000,"version":"3"},{"bead_id":"sp-fresh","mode":"pr","entered_at":9000,"version":"3"},{"bead_id":"sp-unknown","mode":"pr","entered_at":1,"version":"3"}]"#;
        let (t, lc) = scripted_by(&[("list --delivery", 0, rows), ("list --state CERTIFIED", 0, "[]")]);
        let raised = passed(&reg, &lc, &cfg);
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert_eq!(raised[0].reference.as_deref(), Some("incident:pr-stall-auto-merge-off:testrepo"));
        assert!(raised[0].body.contains("sp-stale") && !raised[0].body.contains("sp-fresh"));
        assert!(t.seen.lock().unwrap().iter().all(|a| a[0] == "list"), "nothing but reads went to the store");
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(txvmh8z)]
    #[test]
    fn a_pass_with_an_unreachable_store_files_nothing_arms_nothing_and_requeues_nothing() {
        let d = testkit::TempDir::new("wt-prs-pass-down");
        let reg = world(d.path(), &["sp-stale"]);
        let cfg = stub_gh(&d, "false", "CONFLICTING\\t");
        let (t, lc) = scripted_by(&[("list", 2, "cannot tell")]);
        assert!(passed(&reg, &lc, &cfg).is_empty());
        assert!(gh_calls(&d).is_empty());
        assert!(t.seen.lock().unwrap().iter().all(|a| a[0] == "list"), "no event was sent");
    }

    // covers: UC-ops-detection-remediation-28
    #[test_id(tv5na49)]
    #[test]
    fn the_configured_stall_minutes_are_what_decides_a_stall() {
        let d = testkit::TempDir::new("wt-prs-mins");
        let reg = world(d.path(), &["sp-stale"]);
        let rows = r#"[{"bead_id":"sp-stale","mode":"pr","entered_at":8200,"version":"3"}]"#;
        let (_, lc) = scripted_by(&[("list --delivery", 0, rows), ("list --state CERTIFIED", 0, "[]")]);
        let stub = stub_gh(&d, "false", "MERGEABLE\\tSUCCESS");
        let with = |mins| Cfg::from_mins(mins, stub.gh_bin.clone(), stub.gh_timeout);
        assert_eq!(with(30).stall_secs, 1800);
        assert_eq!(passed(&reg, &lc, &with(30)).len(), 1, "30 minutes old against a 30 minute threshold");
        assert!(passed(&reg, &lc, &with(31)).is_empty(), "and not against 31: the threshold is the configured one");
    }
}
