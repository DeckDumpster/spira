//! `queue verdict <repo>` — settle a CI result (DESIGN-verdict.md). Replaces verdict.sh.
//! queue.local settles the publish PR (§2.1); queue.forge settles the open batch PR (§2.2).

use std::path::{Path, PathBuf};

use super::{idents, landing_log, owner_refused, require_lc, resolve, Ctx, World, FAIL, OK, USAGE};
use crate::lock::{self, Acquire, Guard};
use crate::model::{LandMode, Member};
use crate::ports::ref_branch;
use crate::records::{self, write_atomic, Kv};

/// The publish settle's answer: red is left for the unlocked half (§2.1).
pub const RED: i32 = 3;

/// `queue verdict <repo>`: resolve, require the lifecycle machine, run one pass.
pub fn verdict(w: &World, repo: &str) -> i32 {
    if repo.is_empty() {
        w.err("queue.sh verdict: repo required");
        return USAGE;
    }
    let Ok(c) = resolve(w, "verdict", Some(repo)) else { return FAIL };
    if idents(w, "verdict", &[("repo", &c.r.name)]).is_err() {
        return FAIL;
    }
    if require_lc(w, "verdict").is_err() {
        return FAIL;
    }
    pass(w, &c)
}

/// One verdict pass for a resolved repository — `queue step` calls this in process.
pub fn pass(w: &World, c: &Ctx) -> i32 {
    let name = c.r.name.as_str();
    let Some(path) = c.r.path.clone() else {
        w.err(format!("verdict {name}: not a registered repository (no checkout path)"));
        return FAIL;
    };
    match c.r.mode {
        LandMode::QueueLocal => local_pass(w, c, &path),
        LandMode::Queue => forge_pass(w, c, &path),
        _ => OK,
    }
}

// ------------------------------------------------------------------------ queue.local

fn local_pass(w: &World, c: &Ctx, path: &Path) -> i32 {
    let name = c.r.name.as_str();
    let (rc, out) = match lock::try_lock_as(&c.s.queue_dir, name, "verdict") {
        Acquire::Held(g) => {
            let r = settle_locked(w, c, path);
            drop(g);
            r
        }
        Acquire::Busy => {
            w.out(format!("verdict {name}: another queue operation holds the lock"));
            return OK;
        }
        Acquire::Unopenable => {
            w.err(format!("verdict {name}: cannot open lock file"));
            return FAIL;
        }
    };
    match rc {
        RED => settle_publish_red(w, c, path, &out),
        rc => rc,
    }
}

/// `provision_fault` means the branch was never tested: a harness fault, retried rather
/// than judged.
pub fn normalize_status(s: &str) -> &str {
    match s {
        "provision_fault" => "harness_fault",
        s => s,
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim_end()
}

/// Every `<prefix>` line's value, in order.
fn tagged<'a>(out: &'a str, prefix: &str) -> Vec<&'a str> {
    out.lines().filter_map(|l| l.strip_prefix(prefix)).collect()
}

/// The red suites of a check-status answer, de-duplicated, as a CSV.
fn red_suites_csv(out: &str) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for l in tagged(out, "red-suite: ") {
        for s in l.split_whitespace() {
            if !seen.contains(&s) {
                seen.push(s);
            }
        }
    }
    seen.join(",")
}

/// Failing tests named in a failed step's log tail: `FAILED <file>::<test>` files, else the
/// repo's own `CI FAILED during step: <tier>`, else a bare `N failed` count as `tests`.
fn test_red_csv(out: &str) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut tier: Option<String> = None;
    let mut counted = false;
    for l in tagged(out, "step-log: ") {
        let l = l.trim();
        if let Some(rest) = l.strip_prefix("FAILED ") {
            let file = rest.split("::").next().unwrap_or("").split_whitespace().next().unwrap_or("");
            if !file.is_empty() && !seen.iter().any(|s| s == file) {
                seen.push(file.to_string());
            }
        } else if let Some(t) = l.split("CI FAILED during step:").nth(1) {
            tier = Some(t.trim().replace(char::is_whitespace, "-")).filter(|t| !t.is_empty());
        } else if l.contains(" failed") && l.trim_start_matches('=').trim_start().split_whitespace().next().is_some_and(|n| n.parse::<u32>().is_ok()) {
            counted = true;
        }
    }
    if seen.is_empty() {
        if let Some(t) = tier {
            seen.push(t);
        } else if counted {
            seen.push("tests".into());
        }
    }
    seen.join(",")
}

fn publish_status(w: &World, c: &Ctx, path: &Path, kv: &Kv) -> String {
    let pr = kv.get("pr").unwrap_or("");
    let branch = kv.get("branch").unwrap_or("");
    w.forge.check_status(&c.s.forge, path, pr, branch).unwrap_or_default()
}

/// The locked half: 0 settled or waiting, 1 a fault left for a hand look, 3 red (the
/// caller holds the queue lock; `to-forge` polls this while it holds it for the move).
pub fn settle_publish(w: &World, c: &Ctx, path: &Path) -> i32 {
    settle_locked(w, c, path).0
}

/// [`settle_publish`] plus the check-status answer it judged, which the red half reads.
fn settle_locked(w: &World, c: &Ctx, path: &Path) -> (i32, String) {
    let name = c.r.name.as_str();
    let pfile = c.queue_file("publish");
    let Ok(Some(kv)) = records::read_kv(&pfile) else { return (OK, String::new()) };
    let field = |k: &str| kv.get(k).unwrap_or("").to_string();
    let (pr, remote, forge_branch, head) = (field("pr"), field("remote"), field("forge_branch"), field("head"));
    if [&pr, &field("branch"), &remote, &forge_branch, &head].iter().any(|v| v.is_empty()) {
        w.err(format!("verdict {name}: publish record is missing a field — leaving it for a hand look: {}", pfile.display()));
        return (FAIL, String::new());
    }
    // A closed PR's retriggered Gate run can conclude green with its required jobs skipped,
    // so CI's word is not read until the PR itself is known open.
    match w.forge.pr_state(&c.s.forge, path, &pr).as_deref().map(str::trim) {
        Some("open") => {}
        Some(s @ ("closed" | "merged")) => {
            let _ = std::fs::remove_file(&pfile);
            landing_log(&c.s.run, &format!("QUEUE PUBLISH_ABANDONED {} repo={name} pr={pr} state={s}", w.clock.now()));
            w.lib.notify(
                &c.s.mailbox,
                name,
                &format!("publish PR {pr} abandoned ({s})"),
                &format!("PR {pr} for {name} is {s}, not open — retiring the publish record without moving {remote}/{forge_branch}. A closed-unmerged PR is never green regardless of what its CI says; the next publish carries anything since."),
            );
            w.out(format!("verdict {name}: publish PR {pr} is {s}, not open — abandoned, {remote}/{forge_branch} untouched"));
            return (OK, String::new());
        }
        s => {
            w.out(format!("verdict {name}: publish PR {pr}: cannot read PR state ({}) — waiting", s.filter(|v| !v.is_empty()).unwrap_or("unknown")));
            return (OK, String::new());
        }
    }
    let out = publish_status(w, c, path, &kv);
    let rc = match normalize_status(first_line(&out)) {
        "green" => {
            if !w.lib.push(path, &remote, &format!("{head}:refs/heads/{forge_branch}")) {
                w.err(format!(
                    "verdict {name}: publish PR {pr} green but {remote}/{forge_branch} would not fast-forward — something moved it; leaving the record for a hand look"
                ));
                w.lib.notify(
                    &c.s.mailbox,
                    name,
                    &format!("publish PR {pr} green but fast-forward refused"),
                    &format!("{remote}/{forge_branch} did not fast-forward to {head} for PR {pr} — something else moved it. Left open for a hand look."),
                );
                return (FAIL, out);
            }
            w.forge.pr_close(&c.s.forge, path, &pr);
            let _ = std::fs::remove_file(&pfile);
            landing_log(&c.s.run, &format!("QUEUE PUBLISH_GREEN {} repo={name} pr={pr} head={head}", w.clock.now()));
            w.lib.notify(&c.s.mailbox, name, &format!("publish PR {pr} merged"), &format!("{remote}/{forge_branch} fast-forwarded to {head} (PR {pr})."));
            w.out(format!("verdict {name}: publish PR {pr} green — {remote}/{forge_branch} fast-forwarded to {head}"));
            OK
        }
        "red" => RED,
        s => {
            w.out(format!("verdict {name}: publish PR {pr}: {s} — waiting"));
            OK
        }
    };
    (rc, out)
}

fn short(s: &str) -> String {
    s.chars().take(12).collect()
}

/// The unlocked half of a red publish: attribute locally, file ONE fix-forward bead, retire
/// the record, mark the head so `publish` waits for a change (§2.1).
pub fn settle_publish_red(w: &World, c: &Ctx, path: &Path, out: &str) -> i32 {
    let name = c.r.name.as_str();
    let pfile = c.queue_file("publish");
    let Ok(Some(kv)) = records::read_kv(&pfile) else { return OK };
    let field = |k: &str| kv.get(k).unwrap_or("").to_string();
    let (pr, forge_sha, head) = (field("pr"), field("base"), field("head"));
    let suites = red_suites_csv(out);
    let run_url = tagged(out, "run-url: ").last().map(|s| s.to_string()).unwrap_or_default();
    let run_id = tagged(out, "run-id: ").last().map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s.chars().all(|ch| ch.is_ascii_digit())).unwrap_or_default();
    let failed_steps = tagged(out, "failed-step: ");
    let step_log = tagged(out, "step-log: ");
    let ids: Vec<String> = kv.members().into_iter().map(|m| m.id).filter(|i| !i.is_empty()).collect();
    let member_ids = ids.join(",");

    // Local attribution (attribute.sh, a per-suite without-member rebuild against this
    // published range) is retired with attribute.sh, sp-uwhx0 — not ported: it bisects a
    // one-off range, not a round, and the batcher's own attribution (attrib.rs, sp-hvtgs)
    // has no seam for that shape. The fix-forward bead still files either way; it just
    // names red suites and members instead of a local reproduction.
    let fail_lines = if suites.is_empty() {
        String::new()
    } else {
        w.forge.run_id(&c.s.forge, path, &field("branch")).map(|run| w.forge.fail_lines(&c.s.forge, path, &run, &suites.replace(',', " "))).unwrap_or_default()
    };
    let fail_lines = fail_lines.trim_end().to_string();
    w.lib.cancel_runs(&c.s.forge, path, &field("branch"));
    w.forge.pr_close(&c.s.forge, path, &pr);

    let filed_marker = c.queue_file(&format!("publish-red-run-{run_id}"));
    if !run_id.is_empty() {
        if let Ok(Some(prior)) = records::read_kv(&filed_marker) {
            let _ = std::fs::remove_file(&pfile);
            let _ = write_atomic(&c.queue_file("publish-red"), &format!("head={head}\nfix_forward={}\nsuites={suites}\n", prior.get("fix_forward").unwrap_or("<create-failed>")));
            w.out(format!("verdict {name}: publish PR {pr} red — run {run_id} already has tracker {}; not filing another", prior.get("fix_forward").unwrap_or("?")));
            return OK;
        }
    }

    let mut body = format!("Publish PR {pr} red for {name} ({}).\n\n", if run_url.is_empty() { "run link unavailable" } else { &run_url });
    body.push_str(&format!("Published range: {}..{}\n", short(&forge_sha), short(&head)));
    body.push_str(&format!("Red suites: {}\n\n", if suites.is_empty() { "<none named>" } else { &suites }));
    if !fail_lines.is_empty() {
        body.push_str(&format!("Failing lines:\n{fail_lines}\n\n"));
    }
    if !failed_steps.is_empty() {
        body.push_str("Failed steps:\n");
        for f in &failed_steps {
            body.push_str(&format!("- {f}\n"));
        }
        if !step_log.is_empty() {
            body.push_str(&format!("\nLog tail of the failed step:\n{}\n", step_log.join("\n")));
        }
        body.push('\n');
    }
    body.push_str(&format!("Members in this publish: {}\n\n", if member_ids.is_empty() { "<none>" } else { &member_ids }));
    body.push_str("Fix forward on local/main — the next publish carries the fix. Production was never rolled back and no member bead was reopened.");
    let title = if suites.is_empty() { format!("publish PR {pr} red for {name}") } else { format!("publish PR {pr} red for {name}: {suites}") };
    let actor = w.var("SPIRA_QUEUE_ACTOR").unwrap_or_else(|| "queue.sh".into());
    let prior = records::read_kv(&c.queue_file("publish-red")).ok().flatten().filter(|k| !suites.is_empty() && k.get("suites") == Some(suites.as_str()));
    let amended = prior.as_ref().and_then(|k| k.get("fix_forward")).filter(|id| id.starts_with(|ch: char| ch.is_ascii_alphanumeric()) && !id.starts_with('<')).map(str::to_string).filter(|id| {
        w.lib.amend_bug(&actor, id, &format!("Publish PR {pr} red again for the same suites ({suites}); {}. Range {}..{}; members: {}.{}", if run_url.is_empty() { "run link unavailable" } else { &run_url }, short(&forge_sha), short(&head), if member_ids.is_empty() { "<none>" } else { &member_ids }, if fail_lines.is_empty() { String::new() } else { format!("\nFailing lines:\n{fail_lines}") }))
    });
    let mut labels = format!("spira,plan,repo:{name}");
    if !c.s.verdict.red_tracker_label.is_empty() {
        labels.push(',');
        labels.push_str(&c.s.verdict.red_tracker_label);
    }
    let fid = amended.clone().or_else(|| w.lib.create_bug(&actor, &title, &c.s.verdict.incident_priority, &labels, &body));
    let fid_s = fid.clone().unwrap_or_else(|| "<create-failed>".into());

    let _ = std::fs::remove_file(&pfile);
    if !run_id.is_empty() && fid.is_some() {
        let _ = write_atomic(&filed_marker, &format!("fix_forward={fid_s}\n"));
    }
    // Read by `publish`: holds off the next publish of this same head.
    let _ = write_atomic(&c.queue_file("publish-red"), &format!("head={head}\nfix_forward={fid_s}\nsuites={suites}\n"));
    let suites_s = if suites.is_empty() { "none".to_string() } else { suites.clone() };
    landing_log(&c.s.run, &format!("QUEUE PUBLISH_RED {} repo={name} pr={pr} suites={suites_s} fix_forward={fid_s}", w.clock.now()));
    w.lib.notify(
        &c.s.mailbox,
        name,
        &format!("publish PR {pr} red"),
        &format!("PR {pr} red (suites: {suites_s}). Fix-forward bead: {}.", fid.clone().unwrap_or_else(|| "<create failed>".into())),
    );
    w.out(format!("verdict {name}: publish PR {pr} red — {} fix-forward {fid_s}", if amended.is_some() { "amended open" } else { "filed" }));
    // A red publish with no fix-forward bead is a fault the operator must see.
    if fid.is_none() {
        FAIL
    } else {
        OK
    }
}

// ------------------------------------------------------------------------ queue.forge

/// Take the queue lock, waiting up to `lock_wait` seconds (flock -w), polling each second.
fn lock_waiting(w: &World, c: &Ctx) -> Result<Guard, bool> {
    let mut waited = 0;
    loop {
        match lock::try_lock_as(&c.s.queue_dir, &c.r.name, "verdict") {
            Acquire::Held(g) => return Ok(g),
            Acquire::Unopenable => return Err(false),
            Acquire::Busy if waited >= c.s.verdict.lock_wait => return Err(true),
            Acquire::Busy => {
                w.clock.sleep(1);
                waited += 1;
            }
        }
    }
}

/// A provisional member (admitted SUBMITTED, its gate still running) whose gate has gone FAIL at
/// the tip the batch carries is ejected with the gate's own failing reason; the batcher re-cuts.
fn eject_gate_red_provisionals(w: &World, c: &Ctx) {
    let kv = match records::read_kv(&c.queue_file("open")) {
        Ok(Some(kv)) => kv,
        _ => return,
    };
    let provisional: Vec<&str> = kv.get_first("provisional").unwrap_or("").split_whitespace().collect();
    for m in kv.members().into_iter().filter(|m| provisional.contains(&m.id.as_str())) {
        let Some(why) = super::round::gate_red_at(c, &m.id, &m.tip) else { continue };
        let reason = format!("provisional member {}: its own gate went FAIL at {} while the batch was open: {why}", m.id, m.tip);
        w.out(format!("verdict {}: {reason}", c.r.name));
        super::batch::eject(w, &m.id, Some(&c.r.name), &crate::cli::Text::Arg(reason), "", true, false, false);
        return;
    }
}

fn forge_pass(w: &World, c: &Ctx, path: &Path) -> i32 {
    let name = c.r.name.as_str();
    eject_gate_red_provisionals(w, c);
    let skips_file = c.queue_file("lock-skips");
    let guard = match lock_waiting(w, c) {
        Ok(g) => g,
        Err(false) => {
            w.err(format!("verdict {name}: cannot open lock file"));
            return FAIL;
        }
        Err(true) => {
            let skips = std::fs::read_to_string(&skips_file).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0) + 1;
            let _ = std::fs::write(&skips_file, format!("{skips}\n"));
            if skips == c.s.verdict.lock_starve_max {
                w.out(format!("verdict {name}: queue lock starvation — skipped {skips} consecutive ticks waiting for lock"));
            } else {
                w.out(format!("verdict {name}: another queue operation holds the lock"));
            }
            return OK;
        }
    };
    let _ = std::fs::remove_file(&skips_file);
    let open = c.queue_file("open");
    let rc = if open.is_file() { process(w, c, path, &open) } else { OK };
    close_stray_queue_prs(w, c, path, &open);
    drop(guard);
    // D2: the express-takeover stash had one writer (batch.sh, retired). Never silently skip one.
    let stashed = stashed_attributions(&c.s.queue_dir.join(name));
    if !stashed.is_empty() {
        for f in &stashed {
            w.err(format!(
                "verdict {name}: {} is an express-takeover stash — its attribution is retired (DESIGN-verdict.md D2); settle it by hand (queue abandon) and remove it",
                f.display()
            ));
        }
        return FAIL;
    }
    rc
}

/// A queue PR whose branch is not the recorded batch is a duplicate cut: close it and cancel
/// its runs. A record that exists but names no branch is unreadable, so nothing is closed.
fn close_stray_queue_prs(w: &World, c: &Ctx, path: &Path, open: &Path) {
    let recorded = if open.is_file() {
        match records::read_kv(open).ok().flatten().and_then(|kv| kv.get("branch").map(String::from)).filter(|b| !b.is_empty()) {
            Some(b) => b,
            None => return,
        }
    } else {
        String::new()
    };
    let Some(prs) = w.forge.queue_prs(&c.s.forge, path) else { return };
    for (pr, branch) in prs.into_iter().filter(|(_, b)| *b != recorded) {
        w.out(format!("verdict {}: closing stray queue PR {pr} ({branch}) — not the recorded batch", c.r.name));
        w.forge.pr_close(&c.s.forge, path, &pr);
        w.lib.cancel_runs(&c.s.forge, path, &branch);
    }
}

fn stashed_attributions(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    n.starts_with("attributing-") && !n.ends_with(".lock") && p.is_file()
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The pending branch's decision (verdict.sh `verdict_action pending`). `run_age`/`idle`
/// are seconds already elapsed, None when unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    WaitUnknown,
    WaitRunning,
    WaitProgressing,
    Cancel,
}

pub fn classify_pending(run_age: Option<u64>, idle: Option<u64>, maxsec: u64, idle_max: u64) -> Pending {
    let Some(age) = run_age else { return Pending::WaitUnknown };
    if age < maxsec {
        return Pending::WaitRunning;
    }
    match idle {
        Some(i) if i < idle_max => Pending::WaitProgressing,
        _ => Pending::Cancel,
    }
}

/// The harness-fault branch's decision: re-run while the budget lasts.
pub fn classify_fault_rerun(retries: u64, max_retries: u64) -> bool {
    retries < max_retries
}

/// `run-metadata`'s start time and the LATEST last-activity (a step's own timestamp lags
/// the job's). Unparseable values are unknown.
pub fn parse_run_metadata(meta: &str) -> (Option<u64>, Option<u64>) {
    let mut started = None;
    let mut last: Option<u64> = None;
    for l in meta.lines() {
        if let Some(v) = l.strip_prefix("started-at: ") {
            started = v.trim().parse().ok();
        } else if let Some(v) = l.strip_prefix("last-activity: ") {
            if let Ok(t) = v.trim().parse::<u64>() {
                last = Some(last.map_or(t, |m| m.max(t)));
            }
        }
    }
    (started, last)
}

/// The run's declared job timeout in seconds, which the stuck-run cap may never undercut.
pub fn parse_declared_timeout(meta: &str) -> Option<u64> {
    meta.lines().filter_map(|l| l.strip_prefix("timeout-sec: ")?.trim().parse().ok()).max()
}

struct Batch<'a> {
    name: &'a str,
    path: &'a Path,
    file: &'a Path,
    kv: Kv,
    pr: String,
    head: String,
    base_sha: String,
    branch: String,
    members: Vec<Member>,
    pr_state: String,
}

impl Batch<'_> {
    fn members_str(&self) -> String {
        self.kv.get("members").unwrap_or("").to_string()
    }
}

fn process(w: &World, c: &Ctx, path: &Path, file: &Path) -> i32 {
    let name = c.r.name.as_str();
    let kv = match records::read_kv(file) {
        Ok(Some(kv)) => kv,
        _ => Kv::default(),
    };
    let g = |k: &str| kv.get(k).unwrap_or("").to_string();
    // ONE WRITER PER OPEN BATCH (sp-91hb5): a concierge claim refuses the whole pass.
    let actor = w.var("SPIRA_QUEUE_ACTOR").unwrap_or_else(|| "verdict".into());
    if owner_refused(w, kv.get("owner").unwrap_or(""), &actor, &format!("verdict {name}")) {
        return OK;
    }
    if let Some(rc) = super::reconcile::step(w, c, path, file, &kv) {
        return rc;
    }
    let (pr, head, base_sha) = (g("pr"), g("head"), g("base"));
    if pr.is_empty() || head.is_empty() || base_sha.is_empty() {
        w.err(format!("verdict {name}: malformed batch record"));
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("verdict {name}: cannot resolve base ref"));
        return FAIL;
    };
    let members: Vec<Member> = kv.get("members").unwrap_or("").split_whitespace().map(Member::parse_record).collect();
    let pr_state = w.forge.pr_state(&c.s.forge, path, &pr).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".into());
    let b = Batch { name, path, file, pr, head, base_sha, branch: g("branch"), members, kv, pr_state };

    let out = w.forge.check_status(&c.s.forge, path, &b.pr, &b.branch).unwrap_or_else(|| "pending".into());
    let raw = first_line(&out);
    let status = normalize_status(if raw.is_empty() { "pending" } else { raw }).to_string();
    if status != "pending" {
        record_forge_state(w, c, &b, &status, "", "");
        record_pass(w, c, &b, &status, &out);
    }
    match status.as_str() {
        "pending" => pending(w, c, &b),
        "harness_fault" => harness_fault(w, c, &b, &out),
        "green" => green(w, c, &b, &base, &out),
        "red" => red(w, c, &b, &out),
        s => {
            w.err(format!("verdict {name}: PR {} unknown check status: {s}", b.pr));
            FAIL
        }
    }
}

/// What the verdict last saw of the forge for this batch, left where the ops pane reads it so
/// the pane never polls the forge itself.
fn record_forge_state(w: &World, c: &Ctx, b: &Batch, ci: &str, run: &str, meta: &str) {
    let (started, last) = parse_run_metadata(meta);
    let step = meta.lines().rev().find_map(|l| l.strip_prefix("running-step: ")).unwrap_or("").trim();
    let mut kv = Kv::default();
    kv.push("batch_id", b.kv.get("batch_id").unwrap_or(""));
    kv.push("pr", &b.pr);
    kv.push("pr_state", &b.pr_state);
    kv.push("ci", ci);
    kv.push("run", run);
    kv.push("run_started", &started.map(|t| t.to_string()).unwrap_or_default());
    kv.push("last_activity", &last.map(|t| t.to_string()).unwrap_or_default());
    kv.push("step", &records::one_line(step));
    kv.push("wall", &c.s.verdict.ci_maxsec.to_string());
    kv.push("observed", &w.clock.now().to_string());
    if let Err(e) = write_atomic(&c.queue_file("forge-state"), &kv.render()) {
        w.err(format!("verdict {}: cannot record the forge state: {e}", b.name));
    }
}

/// One `cert-pass` row per judged CI run of the batch, so the throughput report counts forge
/// batches as it counts rounds. The batch record keeps what was recorded, so a verdict pass that
/// re-reads the same settled run (a failed push, a held PR) writes no second row.
fn record_pass(w: &World, c: &Ctx, b: &Batch, status: &str, out: &str) {
    use tsd::pass::{Cause, PassRecord, Place};
    let retries = b.kv.get("retries").unwrap_or("0");
    let key = format!("{}/{retries}/{status}", b.head);
    if b.kv.get("pass_recorded") == Some(key.as_str()) {
        return;
    }
    let batch = [b.kv.get("batch_id"), Some(b.branch.as_str()), Some(b.pr.as_str())].into_iter().flatten().find(|v| !v.is_empty()).unwrap_or("").to_string();
    let passes: u64 = b.kv.get("passes").and_then(|p| p.trim().parse().ok()).unwrap_or(0);
    let now = w.clock.now();
    let started = w.forge.run_id(&c.s.forge, b.path, &b.branch).and_then(|r| parse_run_metadata(&w.forge.run_metadata(&c.s.forge, b.path, &r)).0);
    let cause = match status {
        "green" if tagged(out, "head-sha: ").last().map(|h| h.trim()) == Some(b.head.as_str()) => Cause::Green,
        "red" => Cause::RedAttributed,
        _ => Cause::Infra,
    };
    let rec = PassRecord {
        repo: b.name.to_string(),
        batch,
        pass: (passes + 1).to_string(),
        members: b.members.iter().map(|m| m.id.clone()).collect(),
        instance_type: String::new(),
        vcpus: None,
        place: Place::Local,
        capacity_wait_s: 0,
        boot_s: 0,
        build_s: 0,
        suites_s: started.map_or(0, |s| now.saturating_sub(s)),
        cause,
        detail: format!("PR {}", b.pr),
    };
    let written = rec.write(&c.s.run, &tsd::default_host(), &tsd::iso_utc(now));
    let marked = written.and_then(|()| rewrite(b.file, &["pass_recorded", "passes"], &[("pass_recorded", &key), ("passes", &(passes + 1).to_string())]));
    if let Err(e) = marked {
        w.err(format!("verdict {}: cannot record the certification pass: {e}", b.name));
    }
}

fn pending(w: &World, c: &Ctx, b: &Batch) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    let now = w.clock.now();
    let run = w.forge.run_id(&c.s.forge, b.path, &b.branch);
    let meta = run.as_ref().map(|r| w.forge.run_metadata(&c.s.forge, b.path, r)).unwrap_or_default();
    let (started, last) = parse_run_metadata(&meta);
    record_forge_state(w, c, b, "pending", run.as_deref().unwrap_or(""), &meta);
    let maxsec = c.s.verdict.ci_maxsec.max(parse_declared_timeout(&meta).unwrap_or(0));
    let run_age = match (started, &run) {
        (Some(s), _) => Some(now.saturating_sub(s)),
        (None, None) => Some(now.saturating_sub(b.kv.get("opened").and_then(|o| o.trim().parse().ok()).unwrap_or(0))),
        (None, Some(_)) => None,
    };
    let idle = last.map(|l| now.saturating_sub(l));
    let run_s = run.clone().unwrap_or_default();
    match classify_pending(run_age, idle, maxsec, c.s.verdict.ci_idle_sec) {
        Pending::WaitUnknown => w.out(format!("verdict {name}: PR {pr} pending (run {run_s} age unknown)")),
        Pending::WaitRunning => w.out(format!("verdict {name}: PR {pr} pending (run age {}s)", run_age.unwrap_or(0))),
        Pending::WaitProgressing => w.out(format!(
            "verdict {name}: PR {pr} run {} progressing (last activity {}s ago)",
            run.as_deref().unwrap_or("?"),
            idle.unwrap_or(0)
        )),
        Pending::Cancel => {
            // Cancel explicitly so the shutdown is logged; the next pass sees harness_fault.
            if let Some(r) = &run {
                w.forge.run_cancel(&c.s.forge, b.path, r);
            }
            w.out(format!(
                "verdict {name}: PR {pr} run stuck ({}s, idle {}s) — cancelled; will retry on next pass",
                run_age.unwrap_or(0),
                idle.unwrap_or(0)
            ));
        }
    }
    OK
}

fn rewrite(file: &Path, drop: &[&str], add: &[(&str, &str)]) -> Result<(), String> {
    let mut kv = records::read_kv(file)?.unwrap_or_default();
    for k in drop {
        kv.remove(k);
    }
    for (k, v) in add {
        kv.push(k, v);
    }
    write_atomic(file, &kv.render())
}

/// Abandon the batch on spira-lc, which requeues every member to CERTIFIED. Reported, never
/// fatal: the PR and the record are going either way.
fn lc_requeue(w: &World, b: &Batch, reason: &str) {
    let Some(id) = b.kv.get("batch_id") else { return };
    let r = crate::ident::bounded_text(reason);
    if let Err((rc, e)) = super::lc_abandon(w, id, "verdict.sh", &r) {
        w.err(format!("verdict: spira-lc abandon-batch refused for {id} (rc={rc}): {e}"));
    }
}

/// Close the PR and hand every member back CERTIFIED; the record goes.
fn close_and_certify(w: &World, c: &Ctx, b: &Batch) {
    w.lib.cancel_runs(&c.s.forge, b.path, &b.branch);
    w.forge.pr_close(&c.s.forge, b.path, &b.pr);
    lc_requeue(w, b, "batch closed");
    let _ = std::fs::remove_file(b.file);
}

/// A capacity failure changes nothing on a re-run or a re-cut: close the batch, hold the repo
/// off for an exponentially growing, bounded interval, and say so once (the first failure).
fn capacity_fault(w: &World, c: &Ctx, b: &Batch, marker: &str) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    let file = c.queue_file("capacity-backoff");
    let failures = records::read_kv(&file).ok().flatten().and_then(|k| k.get("failures").and_then(|f| f.trim().parse::<u32>().ok())).unwrap_or(0) + 1;
    let (min, max) = (c.s.verdict.capacity_backoff_min, c.s.verdict.capacity_backoff_max.max(c.s.verdict.capacity_backoff_min));
    let wait = min.saturating_mul(1u64 << (failures - 1).min(20)).min(max);
    let until = w.clock.now() + wait;
    if let Err(e) = write_atomic(&file, &format!("failures={failures}\nuntil={until}\nreason={}\n", records::one_line(marker))) {
        w.err(format!("verdict {name}: cannot record the capacity backoff: {e}"));
    }
    w.out(format!("verdict {name}: PR {pr} provision failed on capacity ({marker}) — closing batch; no re-cut for {wait}s (failure {failures})"));
    close_and_certify(w, c, b);
    if failures == 1 {
        w.scripts.mail_operator(
            &format!("Merge queue: {name} waiting on capacity"),
            &format!("## Note\nMerge queue batch for {name} failed in provision on capacity ({marker}). PR {pr} closed, members returned to CERTIFIED.\n\nThe repo is not re-cut until capacity changes: backing off {wait}s, doubling per consecutive capacity failure up to {max}s. Reported once; further failures are silent.\n"),
        );
    }
    OK
}

fn harness_fault(w: &World, c: &Ctx, b: &Batch, out: &str) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    if let Some(marker) = tagged(out, "capacity: ").last() {
        return capacity_fault(w, c, b, marker.trim());
    }
    let retries: u64 = b.kv.get("retries").and_then(|r| r.trim().parse().ok()).unwrap_or(0);
    let max = c.s.verdict.infra_retries;
    if classify_fault_rerun(retries, max) {
        if let Some(r) = w.forge.run_id(&c.s.forge, b.path, &b.branch) {
            w.forge.workflow_rerun(&c.s.forge, b.path, &r);
        }
        w.out(format!("verdict {name}: PR {pr} harness fault — re-running (attempt {}/{max})", retries + 1));
        if let Err(e) = rewrite(b.file, &["retries"], &[("retries", &(retries + 1).to_string())]) {
            w.err(format!("verdict {name}: cannot record the retry: {e}"));
            return FAIL;
        }
        return OK;
    }
    w.out(format!("verdict {name}: PR {pr} harness fault — retries exhausted; closing batch"));
    close_and_certify(w, c, b);
    w.scripts.mail_operator(
        &format!("Merge queue: {name} CI fault after {} attempts", retries + 1),
        &format!(
            "## Note\nMerge queue batch for {name} closed after {} failed CI run attempts.\n\nPR {pr} (head {}) has been closed. Members returned to CERTIFIED.\n",
            retries + 1,
            b.head
        ),
    );
    OK
}

fn land_member(w: &World, c: &Ctx, path: &Path, m: &Member, sha: &str) {
    w.lib.gh_issue_closeout(&m.id, sha, path);
    super::helpers::close_on_land(w, &c.s.submitted_label, &m.id, sha);
}

fn green(w: &World, c: &Ctx, b: &Batch, base: &str, out: &str) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    let ci_head = tagged(out, "head-sha: ").last().map(|s| s.trim().to_string()).unwrap_or_default();
    if ci_head.is_empty() || ci_head != b.head {
        // An unverifiable head is refused exactly like a mismatched one
        // (law-fail-closed-at-the-source): "unverifiable" is not "clean".
        close_and_certify(w, c, b);
        if ci_head.is_empty() {
            w.out(format!("verdict {name}: PR {pr} green but head-sha missing — cannot verify sealed head; harness fault; PR closed, members requeued"));
            w.scripts.mail_operator(
                &format!("Merge queue: {name} CI head unverifiable (missing head-sha)"),
                &format!(
                    "## Note\nMerge queue batch for {name}: CI reported green but named no head-sha, so its result cannot be tied to the sealed batch head.\n\nSealed batch head: {}\n\nTreated the same as a head mismatch. Members returned to CERTIFIED.\n",
                    b.head
                ),
            );
        } else {
            w.out(format!("verdict {name}: PR {pr} CI head mismatch (ci={ci_head} sealed={}) — harness fault; PR closed, members requeued", b.head));
            w.scripts.mail_operator(
                &format!("Merge queue: {name} CI head mismatch"),
                &format!(
                    "## Note\nMerge queue batch for {name}: CI result belongs to a different commit.\n\nCI-reported PR head: {ci_head}\nSealed batch head: {}\n\nSomething pushed to the batch branch after sealing. Members returned to CERTIFIED.\n",
                    b.head
                ),
            );
        }
        return OK;
    }
    let remote = c.r.ref_remote(base).unwrap_or_default();
    let base_branch = ref_branch(base).to_string();
    let current = w.git.rev_parse(b.path, base);
    if current.as_deref() == Some(b.base_sha.as_str()) {
        return fast_forward(w, c, b, base, &remote, &base_branch, out);
    }
    base_moved(w, c, b, &remote, current)
}

#[allow(clippy::too_many_arguments)]
fn fast_forward(w: &World, c: &Ctx, b: &Batch, base: &str, remote: &str, base_branch: &str, out: &str) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    if !w.lib.push(b.path, remote, &format!("{}:{base_branch}", b.head)) {
        w.err(format!("verdict {name}: PR {pr} fast-forward push failed"));
        return FAIL;
    }
    let _ = std::fs::remove_file(c.queue_file("capacity-backoff"));
    w.out(format!("verdict {name}: PR {pr} landed by fast-forward ({})", b.head));
    if !lc_land(w, b) {
        // The base has moved but the lifecycle did not take the land: keep the record so the
        // reconciler lands it once the PR reads merged.
        return FAIL;
    }
    w.lib.notify(
        &c.s.mailbox,
        name,
        &format!("PR {pr} merged (fast-forward)"),
        &format!("PR {pr} merged onto {base_branch} by fast-forward (head {}). Members: {}", b.head, b.members_str()),
    );
    for m in &b.members {
        land_member(w, c, b.path, m, &b.head);
    }
    for suite in tagged(out, "flaky: ") {
        w.scripts.observe_flake(suite.trim(), &b.head);
    }
    let _ = std::fs::remove_file(b.file);
    if !b.branch.is_empty() {
        w.lib.push(b.path, remote, &format!(":refs/heads/{}", b.branch));
        if w.git.branch_delete_sanctioned(b.path, &b.branch) {
            w.out(format!("verdict {name}: deleted batch branch {}", b.branch));
        } else {
            w.err(format!("verdict {name}: local delete of {} failed", b.branch));
        }
    }
    reap_stale_queue_refs(w, c, b.path, base);
    OK
}

/// Delete local `spira/queue/*` refs already on the base, except the open batch's own.
fn reap_stale_queue_refs(w: &World, c: &Ctx, path: &Path, base: &str) {
    let name = c.r.name.as_str();
    let open_br = records::read_kv(&c.queue_file("open")).ok().flatten().and_then(|k| k.get("branch").map(String::from));
    for (br, _) in w.git.branches(path, "refs/heads/spira/queue/") {
        if br.is_empty() || Some(&br) == open_br.as_ref() {
            continue;
        }
        if w.git.is_ancestor(path, &br, base) && w.git.branch_delete_sanctioned(path, &br) {
            w.out(format!("verdict {name}: reaped stale queue ref {br}"));
        }
    }
}

/// The lifecycle's GREEN → LANDED walk (D3); best-effort, never blocks.
fn lc_land(w: &World, b: &Batch) -> bool {
    let (Some(id), Some(v)) = (b.kv.get("batch_id"), b.kv.get("version").and_then(|v| v.trim().parse::<u64>().ok())) else {
        return true;
    };
    let steps = [("OPEN", format!("{{\"CiStarted\":{{\"run\":\"{}\"}}}}", b.pr)), ("CI_RUNNING", "\"Green\"".to_string())];
    let mut v = v;
    for (expect, kind) in steps {
        if let Err((rc, e)) = w.lc.batch_event(id, expect, &v.to_string(), "verdict.sh", &kind) {
            w.err(format!("verdict: spira-lc event batch {id} ({kind}) refused (rc={rc}): {e}"));
            return false;
        }
        v += 1;
    }
    match w.lc.land_batch(id, &v.to_string(), "verdict.sh", &b.head) {
        Ok(()) => {
            w.out(format!("verdict: {id} landed on spira-lc (sha={})", b.head));
            true
        }
        Err((rc, e)) => {
            w.err(format!("verdict: spira-lc land refused for {id} (rc={rc}): {e}"));
            false
        }
    }
}

fn base_moved(w: &World, c: &Ctx, b: &Batch, remote: &str, current: Option<String>) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    // A tip already in the new base means the batch merged some other way: no rebuild.
    let any_in_base = current.as_ref().is_some_and(|cur| b.members.iter().any(|m| w.git.is_ancestor(b.path, &m.tip, cur)));
    if let (false, false, Some(cur)) = (any_in_base, b.branch.is_empty(), current.as_ref()) {
        let wt = c.s.run.join("worktree").join(format!(
            ".batch-{}-mov{}",
            b.path.file_name().and_then(|n| n.to_str()).unwrap_or("repo"),
            w.env.pid()
        ));
        w.git.worktree_remove(b.path, &wt);
        if w.git.worktree_add_detached(b.path, &wt, cur) {
            let mut conflict = None;
            for m in &b.members {
                if !super::round::merge_member(w, c, &wt, &m.id, &m.tip) {
                    w.git.merge_abort(&wt);
                    conflict = Some(m.id.clone());
                    break;
                }
            }
            let new_head = if conflict.is_none() { w.git.rev_parse(&wt, "HEAD") } else { None };
            w.git.worktree_remove(b.path, &wt);
            if let Some(h) = new_head {
                if w.lib.push(b.path, remote, &format!("+{h}:refs/heads/{}", b.branch)) {
                    let members = b.members_str();
                    if let Err(e) = rewrite(b.file, &["head", "members", "retries", "base"], &[("head", &h), ("retries", "0"), ("members", &members), ("base", cur)]) {
                        w.err(format!("verdict {name}: PR {pr} rebuilt but the record could not be resealed: {e}"));
                        return FAIL;
                    }
                    w.out(format!("verdict {name}: PR {pr} rebuilt on moved base ({cur}) — re-pushed (head {h})"));
                    w.lib.notify(&c.s.mailbox, name, &format!("PR {pr} rebuilt (base moved)"), &format!("PR {pr} rebuilt onto moved base {cur} and force-pushed (head {h})."));
                    return OK;
                }
            } else if let Some(id) = conflict {
                w.out(format!("verdict {name}: PR {pr} base moved — conflict in {id}; closing and requeuing"));
            }
        }
    }
    w.lib.cancel_runs(&c.s.forge, b.path, &b.branch);
    w.forge.pr_close(&c.s.forge, b.path, pr);
    w.out(format!("verdict {name}: PR {pr} base moved ({}) — closed, members requeued", current.as_deref().unwrap_or("unknown")));
    lc_requeue(w, b, "base moved");
    for m in &b.members {
        if let Some(cur) = current.as_ref().filter(|cur| w.git.is_ancestor(b.path, &m.tip, cur)) {
            super::land::lc_deliver(w, b.path, cur, m);
            land_member(w, c, b.path, m, cur);
            w.out(format!("verdict {name}: {} already in moved base — LANDED", m.id));
        }
    }
    let _ = std::fs::remove_file(b.file);
    OK
}

/// Whether the batch's latest run is queued or in progress, as the forge itself says.
fn run_active(w: &World, c: &Ctx, b: &Batch) -> bool {
    let Some(run) = w.forge.run_id(&c.s.forge, b.path, &b.branch) else { return false };
    let meta = w.forge.run_metadata(&c.s.forge, b.path, &run);
    meta.lines().filter_map(|l| l.strip_prefix("run-status: ")).any(|s| matches!(s.trim(), "queued" | "in_progress" | "waiting" | "pending" | "requested"))
}

/// `(path, line)` for every `path:line[:col]` the failed step's log tail names.
fn located_lines(out: &str) -> Vec<(String, String)> {
    let mut hits = Vec::new();
    for l in tagged(out, "step-log: ") {
        for tok in l.split_whitespace() {
            let mut parts = tok.trim_start_matches("-->").split(':');
            let (Some(path), Some(line)) = (parts.next(), parts.next()) else { continue };
            let path = path.trim_start_matches("./");
            if path.contains('.') && !path.contains("//") && !path.is_empty() && !line.is_empty() && line.chars().all(|ch| ch.is_ascii_digit()) {
                hits.push((path.to_string(), l.trim().to_string()));
            }
        }
    }
    hits
}

/// Members whose own changed files a failed step's log names with a file:line, with those lines.
fn member_lint_reds(w: &World, b: &Batch, out: &str) -> Vec<(Member, Vec<String>)> {
    let hits = located_lines(out);
    if hits.is_empty() {
        return Vec::new();
    }
    let mut faulty = Vec::new();
    for m in &b.members {
        let from = w.git.merge_base(b.path, &b.base_sha, &m.tip).unwrap_or_else(|| b.base_sha.clone());
        let Ok(changed) = w.git.numstat(b.path, &from, &m.tip) else { continue };
        let mut lines: Vec<String> = Vec::new();
        for (path, line) in &hits {
            if changed.iter().any(|(f, _, _)| f == path) && !lines.contains(line) {
                lines.push(line.clone());
            }
        }
        if !lines.is_empty() {
            faulty.push((m.clone(), lines));
        }
    }
    faulty
}

/// A red forge-batch step is judged like a red VM-round suite: the Failed event opens the
/// mending row and a mender bead names the member and the step. The member's red tip stays
/// barred, so the next batch carries only a mended tip.
fn summon_forge_mender(w: &World, c: &Ctx, b: &Batch, faulty: &[(Member, Vec<String>)], step: &str) {
    let batch_id = b.kv.get("batch_id").unwrap_or("");
    if batch_id.is_empty() {
        return;
    }
    let suite = step.split(|ch: char| !ch.is_ascii_alphanumeric()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-").to_ascii_lowercase();
    let suite = if suite.is_empty() { "forge-step".to_string() } else { suite };
    if super::round::forge_failed(w, batch_id, &suite).is_err() {
        return;
    }
    let evidence: String = faulty.iter().map(|(m, lines)| format!("Member {} (tip {}):\n{}", m.id, m.tip, lines.join("\n"))).collect::<Vec<_>>().join("\n\n");
    let members = faulty.iter().map(|(m, _)| m.id.as_str()).collect::<Vec<_>>().join(",");
    super::round::summon_mender(w, c, batch_id, &suite, &format!("Forge batch PR {} step {suite} is red in member code: {members}", b.pr), Some(&evidence));
}

/// Eject the members the red names (with the lines), record their tips so no re-cut re-admits
/// them, and abandon the batch so the survivors return to CERTIFIED.
fn eject_red_members(w: &World, c: &Ctx, b: &Batch, faulty: &[(Member, Vec<String>)], step: &str, bo: &str) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    let batch_id = b.kv.get("batch_id").unwrap_or("").to_string();
    let mut failed = false;
    summon_forge_mender(w, c, b, faulty, step);
    for (m, lines) in faulty {
        let why = format!("batch PR {pr} red at {}: {}", m.tip, lines.join(" | "));
        if let Err(e) = super::round::record_batch_red(c, &m.id, &m.tip) {
            w.err(format!("verdict {name}: cannot record {}'s red tip: {e}", m.id));
            failed = true;
        }
        w.lib.cause_event(&m.id, "eject-red");
        w.lib.release_claim(&m.id);
        if !batch_id.is_empty() {
            if let Err((rc, e)) = super::lc_eject_member(w, &batch_id, &m.id, "verdict.sh", &crate::ident::bounded_text(&why)) {
                w.err(format!("verdict {name}: spira-lc eject-member refused for {} (rc={rc}): {e}", m.id));
                failed = true;
            }
        }
        failed |= super::batch::lc_return(w, &m.id, true).is_err();
        w.lib.comment(&m.id, &format!("Ejected from batch PR {pr}: its own files failed CI.\n\n{}\n\nFix the failing issue and re-certify before rejoining the queue.", lines.join("\n")));
        w.out(format!("verdict {name}: PR {pr} red — {bo}{} ejected: {}", m.id, lines.join(" | ")));
    }
    lc_requeue(w, b, &format!("member red: {}", faulty.iter().map(|(m, _)| m.id.as_str()).collect::<Vec<_>>().join(",")));
    w.lib.cancel_runs(&c.s.forge, b.path, &b.branch);
    w.forge.pr_close(&c.s.forge, b.path, pr);
    let _ = std::fs::remove_file(b.file);
    if failed {
        FAIL
    } else {
        OK
    }
}

/// A red with no suite annotations is a setup red: re-run it once in full, then abandon the
/// batch with the evidence if it is still red. The re-run may be refused (no input changed),
/// so the grace is a clock, not a verdict; the next batch is what changes the input.
fn setup_red(w: &World, c: &Ctx, b: &Batch, out: &str, bo: &str) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    let now = w.clock.now();
    let step = tagged(out, "failed-step: ").first().map(|s| s.trim().to_string()).unwrap_or_default();
    let rerun_at: Option<u64> = b.kv.get("setup_rerun").and_then(|t| t.trim().parse().ok());
    if let Some(t) = rerun_at {
        if b.kv.get("setup_incident").is_none() && now.saturating_sub(t) < c.s.verdict.ci_idle_sec && run_active(w, c, b) {
            w.out(format!("verdict {name}: PR {pr} red — {bo}setup red, re-run in flight"));
            return OK;
        }
    } else if let Some(run) = w.forge.run_id(&c.s.forge, b.path, &b.branch) {
        w.forge.workflow_rerun(&c.s.forge, b.path, &run);
        w.out(format!("verdict {name}: PR {pr} red — {bo}no suite annotations (step: {}); re-running run {run} once", if step.is_empty() { "unknown" } else { &step }));
        return match rewrite(b.file, &["setup_rerun", "setup_step"], &[("setup_rerun", &now.to_string()), ("setup_step", &step)]) {
            Ok(()) => OK,
            Err(e) => {
                w.err(format!("verdict {name}: PR {pr} re-run started but not recorded: {e}"));
                FAIL
            }
        };
    }
    let first = b.kv.get("setup_step").unwrap_or("").to_string();
    let log = tagged(out, "step-log: ").join("\n");
    let shown = if step.is_empty() { "unknown" } else { step.as_str() };
    let same = if rerun_at.is_none() {
        "no run id to re-run"
    } else if first == step {
        "same failing step on the re-run"
    } else {
        "a different failing step on the re-run"
    };
    if rerun_at.is_some() && first == step && b.members.len() == 1 {
        let mut lines: Vec<String> = log.lines().rev().take(5).map(str::to_string).collect();
        lines.reverse();
        if lines.is_empty() {
            lines.push(format!("failed step: {shown}"));
        }
        return eject_red_members(w, c, b, &[(b.members[0].clone(), lines)], &step, bo);
    }
    let evidence = format!("no suite annotations ({same}); failed step: {shown}; log tail:\n{log}");
    w.forge.pr_comment(&c.s.forge, b.path, &b.pr, &crate::ident::bounded_text(&format!("Batch abandoned: {evidence}")));
    w.scripts.mail_operator(
        &format!("Merge queue: {name} PR {pr} red in setup, abandoned"),
        &format!(
            "## Batch abandoned\nBatch PR {pr} for {name} (head {}) is red with no suite annotations ({same}); members are back to CERTIFIED and the next batch is cut.\n\nFailed step: {shown}\nFirst failed step: {}\n\nLog tail:\n{log}\n",
            b.head,
            if first.is_empty() { "unknown" } else { &first }
        ),
    );
    w.lib.cancel_runs(&c.s.forge, b.path, &b.branch);
    w.forge.pr_close(&c.s.forge, b.path, &b.pr);
    lc_requeue(w, b, &format!("setup red, {same}: {shown}: {}", log.lines().last().unwrap_or("")));
    let _ = std::fs::remove_file(b.file);
    w.out(format!("verdict {name}: PR {pr} red — {bo}no suite annotations, {same}; batch abandoned, members requeued"));
    OK
}

/// A red batch PR goes to the batcher persona (D1): the suites and a run link, nothing else
/// touched — the PR and its members stay exactly as they are.
fn red(w: &World, c: &Ctx, b: &Batch, out: &str) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    // verdict.sh's lines named the batcher's own batches "batcher-owned"; kept byte-identical
    // for them, and simply absent for a hand-cut batch (D1).
    let bo = if b.kv.get("owner") == Some("batcher") { "batcher-owned, " } else { "" };
    if let Some(j) = b.kv.get("judgement").filter(|j| !j.is_empty()) {
        w.out(format!("verdict {name}: PR {pr} red — {bo}judgement already summoned ({j})"));
        return OK;
    }
    let mut suites = red_suites_csv(out);
    if suites.is_empty() {
        suites = test_red_csv(out);
    }
    if suites.is_empty() {
        let faulty = member_lint_reds(w, b, out);
        if !faulty.is_empty() {
            let step = tagged(out, "failed-step: ").first().map(|s| s.trim().to_string()).unwrap_or_default();
            return eject_red_members(w, c, b, &faulty, &step, bo);
        }
        return setup_red(w, c, b, out, bo);
    }
    super::round::raise_forge_reds(w, c, &b.kv, &suites);
    if c.s.batcher_off {
        let lead = if bo.is_empty() { String::new() } else { "batcher-owned but ".into() };
        w.err(format!("verdict {name}: PR {pr} red ({suites}) — {lead}the batcher is off (SPIRA_BATCHER_ENABLE=0); cannot summon judgement"));
        return FAIL;
    }
    let Some(bin) = c.s.batcher_bin.as_ref() else {
        w.err(format!("verdict {name}: PR {pr} red ({suites}) — no batcher program; cannot summon judgement"));
        return FAIL;
    };
    let ids: Vec<&str> = b.members.iter().map(|m| m.id.as_str()).collect();
    let mut evidence = format!("PR {pr}");
    if let Some(u) = tagged(out, "run-url: ").last() {
        evidence.push_str(&format!(" — {u}"));
    }
    let r = w.scripts.judgement_ci(bin, &c.s, name, &suites, &ids.join(","), &evidence);
    let id = r.out.lines().find_map(|l| l.strip_prefix("id=")).map(str::trim).filter(|i| !i.is_empty());
    match id {
        Some(id) if r.rc == 0 => {
            if let Err(e) = rewrite(b.file, &["judgement"], &[("judgement", id)]) {
                w.err(format!("verdict {name}: PR {pr} judgement {id} filed but not recorded: {e}"));
                return FAIL;
            }
            w.out(format!("verdict {name}: PR {pr} red ({suites}) — {bo}summoned judgement ({id})"));
            OK
        }
        _ => {
            w.err(format!("verdict {name}: PR {pr} red ({suites}) — {bo}judgement-ci failed: {}", r.out.trim_end()));
            FAIL
        }
    }
}
