//! `queue verdict <repo>` — settle a CI result (DESIGN-verdict.md). Replaces verdict.sh.
//! queue.local settles the publish PR (§2.1); queue.forge settles the open batch PR (§2.2).

use std::path::{Path, PathBuf};

use super::{idents, landing_log, lifecycle_on, owner_refused, require_lc, resolve, Ctx, World, FAIL, OK, USAGE};
use crate::lock::{self, Acquire, Guard};
use crate::model::{LandMode, Member};
use crate::ports::ref_branch;
use crate::records::{self, write_atomic, Kv};

/// The publish settle's answer: red is left for the unlocked half (§2.1).
pub const RED: i32 = 3;

/// `queue verdict <repo>`: resolve, read the lifecycle switch, run one pass.
pub fn verdict(w: &World, repo: &str) -> i32 {
    if repo.is_empty() {
        w.err("queue.sh verdict: repo required");
        return USAGE;
    }
    let Ok(c) = resolve(w, "verdict", Some(repo)) else { return FAIL };
    if idents(w, "verdict", &[("repo", &c.r.name)]).is_err() {
        return FAIL;
    }
    let lc_on = lifecycle_on(w);
    if lc_on && require_lc(w, "verdict").is_err() {
        return FAIL;
    }
    pass(w, &c, lc_on)
}

/// One verdict pass for a resolved repository — `queue step` calls this in process.
pub fn pass(w: &World, c: &Ctx, lc_on: bool) -> i32 {
    let name = c.r.name.as_str();
    let Some(path) = c.r.path.clone() else {
        w.err(format!("verdict {name}: not a registered repository (no checkout path)"));
        return FAIL;
    };
    match c.r.mode {
        LandMode::QueueLocal => local_pass(w, c, &path),
        LandMode::Queue => forge_pass(w, c, &path, lc_on),
        _ => OK,
    }
}

// ------------------------------------------------------------------------ queue.local

fn local_pass(w: &World, c: &Ctx, path: &Path) -> i32 {
    let name = c.r.name.as_str();
    let (rc, out) = match lock::try_lock(&c.s.queue_dir, name) {
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
    let out = publish_status(w, c, path, &kv);
    let rc = match normalize_status(first_line(&out)) {
        "green" => {
            if !w.lib.push(path, &remote, &format!("{head}:refs/heads/{forge_branch}")) {
                w.err(format!(
                    "verdict {name}: publish PR {pr} green but {remote}/{forge_branch} would not fast-forward — something moved it; leaving the record for a hand look"
                ));
                w.lib.notify(
                    name,
                    &format!("publish PR {pr} green but fast-forward refused"),
                    &format!("{remote}/{forge_branch} did not fast-forward to {head} for PR {pr} — something else moved it. Left open for a hand look."),
                );
                return (FAIL, out);
            }
            w.forge.pr_close(&c.s.forge, path, &pr);
            let _ = std::fs::remove_file(&pfile);
            landing_log(&c.s.run, &format!("QUEUE PUBLISH_GREEN {} repo={name} pr={pr} head={head}", w.clock.now()));
            w.lib.notify(name, &format!("publish PR {pr} merged"), &format!("{remote}/{forge_branch} fast-forwarded to {head} (PR {pr})."));
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
    let ids: Vec<String> = kv.members().into_iter().map(|m| m.id).filter(|i| !i.is_empty()).collect();
    let member_ids = ids.join(",");

    // Local attribution (attribute.sh, a per-suite without-member rebuild against this
    // published range) is retired with attribute.sh, sp-uwhx0 — not ported: it bisects a
    // one-off range, not a round, and the batcher's own attribution (attrib.rs, sp-hvtgs)
    // has no seam for that shape. The fix-forward bead still files either way; it just
    // names red suites and members instead of a local reproduction.
    w.forge.pr_close(&c.s.forge, path, &pr);

    let mut body = format!("Publish PR {pr} red for {name} ({}).\n\n", if run_url.is_empty() { "run link unavailable" } else { &run_url });
    body.push_str(&format!("Published range: {}..{}\n", short(&forge_sha), short(&head)));
    body.push_str(&format!("Red suites: {}\n\n", if suites.is_empty() { "<none named>" } else { &suites }));
    body.push_str(&format!("Members in this publish: {}\n\n", if member_ids.is_empty() { "<none>" } else { &member_ids }));
    body.push_str("Fix forward on local/main — the next publish carries the fix. Production was never rolled back and no member bead was reopened.");
    let title = if suites.is_empty() { format!("publish PR {pr} red for {name}") } else { format!("publish PR {pr} red for {name}: {suites}") };
    let actor = w.var("SPIRA_QUEUE_ACTOR").unwrap_or_else(|| "queue.sh".into());
    let fid = w.lib.create_bug(&actor, &title, &c.s.verdict.incident_priority, &format!("spira,plan,repo:{name}"), &body);
    let fid_s = fid.clone().unwrap_or_else(|| "<create-failed>".into());

    let _ = std::fs::remove_file(&pfile);
    // Read by `publish`: holds off the next publish of this same head.
    let _ = write_atomic(&c.queue_file("publish-red"), &format!("head={head}\nfix_forward={fid_s}\n"));
    let suites_s = if suites.is_empty() { "none".to_string() } else { suites.clone() };
    landing_log(&c.s.run, &format!("QUEUE PUBLISH_RED {} repo={name} pr={pr} suites={suites_s} fix_forward={fid_s}", w.clock.now()));
    w.lib.notify(
        name,
        &format!("publish PR {pr} red"),
        &format!("PR {pr} red (suites: {suites_s}). Fix-forward bead: {}.", fid.clone().unwrap_or_else(|| "<create failed>".into())),
    );
    w.out(format!("verdict {name}: publish PR {pr} red — filed fix-forward {fid_s}"));
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
        match lock::try_lock(&c.s.queue_dir, &c.r.name) {
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

fn forge_pass(w: &World, c: &Ctx, path: &Path, lc_on: bool) -> i32 {
    let name = c.r.name.as_str();
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
    let rc = if open.is_file() { process(w, c, path, &open, lc_on) } else { OK };
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
}

impl Batch<'_> {
    fn members_str(&self) -> String {
        self.kv.get("members").unwrap_or("").to_string()
    }
}

fn process(w: &World, c: &Ctx, path: &Path, file: &Path, lc_on: bool) -> i32 {
    let name = c.r.name.as_str();
    let kv = match records::read_kv(file) {
        Ok(Some(kv)) => kv,
        _ => Kv::default(),
    };
    let g = |k: &str| kv.get(k).unwrap_or("").to_string();
    let (pr, head, base_sha) = (g("pr"), g("head"), g("base"));
    if pr.is_empty() || head.is_empty() || base_sha.is_empty() {
        w.err(format!("verdict {name}: malformed batch record"));
        return FAIL;
    }
    // ONE WRITER PER OPEN BATCH (sp-91hb5): a concierge claim refuses the whole pass.
    let actor = w.var("SPIRA_QUEUE_ACTOR").unwrap_or_else(|| "verdict".into());
    if owner_refused(w, kv.get("owner").unwrap_or(""), &actor, &format!("verdict {name}")) {
        return OK;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("verdict {name}: cannot resolve base ref"));
        return FAIL;
    };
    let members: Vec<Member> = kv.get("members").unwrap_or("").split_whitespace().map(Member::parse_record).collect();
    let b = Batch { name, path, file, pr, head, base_sha, branch: g("branch"), members, kv };

    let out = w.forge.check_status(&c.s.forge, path, &b.pr, &b.branch).unwrap_or_else(|| "pending".into());
    let raw = first_line(&out);
    let status = normalize_status(if raw.is_empty() { "pending" } else { raw }).to_string();
    match status.as_str() {
        "pending" => pending(w, c, &b),
        "harness_fault" => harness_fault(w, c, &b),
        "green" => green(w, c, &b, &base, &out, lc_on),
        "red" => red(w, c, &b, &out),
        s => {
            w.err(format!("verdict {name}: PR {} unknown check status: {s}", b.pr));
            FAIL
        }
    }
}

fn pending(w: &World, c: &Ctx, b: &Batch) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    let now = w.clock.now();
    let run = w.forge.run_id(&c.s.forge, b.path, &b.branch);
    let (started, last) = match &run {
        Some(r) => parse_run_metadata(&w.forge.run_metadata(&c.s.forge, b.path, r)),
        None => (None, None),
    };
    let run_age = match (started, &run) {
        (Some(s), _) => Some(now.saturating_sub(s)),
        (None, None) => Some(now.saturating_sub(b.kv.get("opened").and_then(|o| o.trim().parse().ok()).unwrap_or(0))),
        (None, Some(_)) => None,
    };
    let idle = last.map(|l| now.saturating_sub(l));
    let run_s = run.clone().unwrap_or_default();
    match classify_pending(run_age, idle, c.s.verdict.ci_maxsec, c.s.verdict.ci_idle_sec) {
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

/// Close the PR and hand every member back CERTIFIED; the record goes.
fn close_and_certify(w: &World, c: &Ctx, b: &Batch) {
    w.forge.pr_close(&c.s.forge, b.path, &b.pr);
    for m in &b.members {
        w.lib.land_mark(&m.id, "CERTIFIED", &m.tip, "");
    }
    let _ = std::fs::remove_file(b.file);
}

fn harness_fault(w: &World, c: &Ctx, b: &Batch) -> i32 {
    let (name, pr) = (b.name, &b.pr);
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

fn land_member(w: &World, path: &Path, m: &Member, sha: &str, reason: &str) {
    w.lib.land_mark(&m.id, "LANDED", &m.tip, reason);
    w.lib.gh_issue_closeout(&m.id, sha, path);
    w.lib.bead_close_on_land(&m.id, sha);
}

fn green(w: &World, c: &Ctx, b: &Batch, base: &str, out: &str, lc_on: bool) -> i32 {
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
        return fast_forward(w, c, b, base, &remote, &base_branch, out, lc_on);
    }
    base_moved(w, c, b, &remote, current)
}

#[allow(clippy::too_many_arguments)]
fn fast_forward(w: &World, c: &Ctx, b: &Batch, base: &str, remote: &str, base_branch: &str, out: &str, lc_on: bool) -> i32 {
    let (name, pr) = (b.name, &b.pr);
    if !w.lib.push(b.path, remote, &format!("{}:{base_branch}", b.head)) {
        w.err(format!("verdict {name}: PR {pr} fast-forward push failed"));
        return FAIL;
    }
    w.out(format!("verdict {name}: PR {pr} landed by fast-forward ({})", b.head));
    if lc_on {
        lc_land(w, b);
    }
    w.lib.notify(
        name,
        &format!("PR {pr} merged (fast-forward)"),
        &format!("PR {pr} merged onto {base_branch} by fast-forward (head {}). Members: {}", b.head, b.members_str()),
    );
    for m in &b.members {
        land_member(w, b.path, m, &b.head, "");
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

/// The lifecycle's GREEN → LANDED walk (switch ON only, D3); best-effort, never blocks.
fn lc_land(w: &World, b: &Batch) {
    let (Some(id), Some(v)) = (b.kv.get("batch_id"), b.kv.get("version").and_then(|v| v.trim().parse::<u64>().ok())) else {
        return;
    };
    let steps = [("OPEN", format!("{{\"CiStarted\":{{\"run\":\"{}\"}}}}", b.pr)), ("CI_RUNNING", "\"Green\"".to_string())];
    let mut v = v;
    for (expect, kind) in steps {
        if let Err((rc, e)) = w.lc.batch_event(id, expect, &v.to_string(), "verdict.sh", &kind) {
            w.err(format!("verdict: spira-lc event batch {id} ({kind}) refused (rc={rc}): {e}"));
            return;
        }
        v += 1;
    }
    match w.lc.land_batch(id, &v.to_string(), "verdict.sh", &b.head) {
        Ok(()) => w.out(format!("verdict: {id} landed on spira-lc (sha={})", b.head)),
        Err((rc, e)) => w.err(format!("verdict: spira-lc land refused for {id} (rc={rc}): {e}")),
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
                if !w.git.merge_no_ff(&wt, &w.lib.land_subject(&m.id), &m.tip) {
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
                    w.lib.notify(name, &format!("PR {pr} rebuilt (base moved)"), &format!("PR {pr} rebuilt onto moved base {cur} and force-pushed (head {h})."));
                    return OK;
                }
            } else if let Some(id) = conflict {
                w.out(format!("verdict {name}: PR {pr} base moved — conflict in {id}; closing and requeuing"));
            }
        }
    }
    w.forge.pr_close(&c.s.forge, b.path, pr);
    w.out(format!("verdict {name}: PR {pr} base moved ({}) — closed, members requeued", current.as_deref().unwrap_or("unknown")));
    for m in &b.members {
        match current.as_ref() {
            Some(cur) if w.git.is_ancestor(b.path, &m.tip, cur) => {
                land_member(w, b.path, m, cur, "already-in-base");
                w.out(format!("verdict {name}: {} already in moved base — LANDED", m.id));
            }
            _ => w.lib.land_mark(&m.id, "CERTIFIED", &m.tip, ""),
        }
    }
    let _ = std::fs::remove_file(b.file);
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
    let suites = red_suites_csv(out);
    if suites.is_empty() {
        w.out(format!("verdict {name}: PR {pr} red — {bo}no suite annotations; leaving for the batcher to re-check"));
        return OK;
    }
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
