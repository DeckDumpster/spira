//! `queue round`: the round lifecycle the batcher and the Concierge share, under queue.local.
//! A round is one record (`<queue_dir>/<repo>/round`), one worktree and one spira-lc batch;
//! the verbs differ from a hand's scratch scripts in that spira-lc, loom and round-duty can
//! all see the round (DESIGN.md §2.2).

use std::fs;
use std::path::{Path, PathBuf};

use super::batch::lc_return;
use super::land::land_local_with;
use super::{actor, czar_ok, idents, landing_log, lc_cas, lock_held_by_caller, read_text, repo_path, require_lc, resolve, take_lock, Ctx, World, FAIL, OK, USAGE};
use crate::cli::{Round, Text};
use crate::ident::bounded_text;
use crate::lock::Guard;
use crate::model::{parse_members, render_members, EjectCause, LandMode, Member};
use crate::records::{self, one_line, write_atomic, Kv};

/// certify's exit when the corpus could not be judged (the VM, the wall, the install), as
/// opposed to FAIL, which is a red round.
pub const FAULT: i32 = 4;

const RECORD: &str = "round";
const LC_ACTOR: &str = "queue.sh";
const BLOCKING: [&str; 5] = ["red", "timeout", "unreached", "deferred", "fault"];

pub fn run(w: &World, r: &Round) -> i32 {
    match r {
        Round::Open { repo, members, name, worktree } => open(w, repo.as_deref(), members, name.as_deref(), worktree.as_deref()),
        Round::Certify { batch, repo, attest } => certify(w, batch, repo.as_deref(), attest.as_deref()),
        Round::Eject { batch, id, repo, reason, suites, red, rebuild } => eject(w, batch, id, repo.as_deref(), reason, suites, *red, *rebuild),
        Round::Land { batch, repo } => land(w, batch, repo.as_deref()),
        Round::Abandon { batch, repo, reason } => abandon(w, batch, repo.as_deref(), reason),
        Round::Status { repo } => status(w, repo.as_deref()),
    }
}

fn local_ctx(w: &World, label: &str, repo: Option<&str>) -> Result<(Ctx, PathBuf), i32> {
    if !czar_ok(w) {
        return Err(FAIL);
    }
    let c = resolve(w, label, repo)?;
    let path = repo_path(w, label, &c)?;
    if c.r.mode != LandMode::QueueLocal {
        w.err(format!(
            "queue.sh {label}: repo is not in queue.local mode (mode={}) — a queue.forge round is a batch PR (open-batch, verdict)",
            c.r.mode.as_str()
        ));
        return Err(FAIL);
    }
    Ok((c, path))
}

fn lock(w: &World, label: &str, c: &Ctx) -> Result<Option<Guard>, i32> {
    if lock_held_by_caller(w) {
        Ok(None)
    } else {
        take_lock(w, label, c, "")
    }
}

fn load(w: &World, label: &str, c: &Ctx, batch: &str) -> Result<Kv, i32> {
    match records::read_kv(&c.queue_file(RECORD)) {
        Ok(Some(kv)) if kv.get("batch_id") == Some(batch) => Ok(kv),
        Ok(Some(kv)) => {
            w.err(format!("queue.sh {label}: the open round for {} is {}, not {batch}", c.r.name, kv.get("batch_id").unwrap_or("?")));
            Err(FAIL)
        }
        _ => {
            w.err(format!("queue.sh {label}: no open round for {}", c.r.name));
            Err(FAIL)
        }
    }
}

fn save(w: &World, label: &str, c: &Ctx, kv: &Kv) -> Result<(), i32> {
    write_atomic(&c.queue_file(RECORD), &kv.render()).map_err(|e| {
        w.err(format!("queue.sh {label}: cannot write the round record: {e}"));
        FAIL
    })
}

fn set(kv: &mut Kv, k: &str, v: &str) {
    kv.remove(k);
    kv.push(k, v);
}

fn set_phase(w: &World, kv: &mut Kv, phase: &str) {
    set(kv, "phase", phase);
    set(kv, "phase_at", &w.clock.now().to_string());
}

fn rounds_dir(c: &Ctx) -> PathBuf {
    c.s.run.join("rounds")
}

fn mark_running(c: &Ctx, batch: &str) {
    let dir = rounds_dir(c);
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join(format!("{batch}.running")), "");
}

fn finish(c: &Ctx, batch: &str, line: &str) {
    use std::io::Write;
    let dir = rounds_dir(c);
    let _ = fs::create_dir_all(&dir);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{batch}.result"))) {
        let _ = writeln!(f, "{line}");
    }
    let _ = fs::remove_file(dir.join(format!("{batch}.running")));
}

fn csv(ms: &[Member]) -> String {
    ms.iter().map(Member::render).collect::<Vec<_>>().join(",")
}

fn worktree_of(kv: &Kv) -> PathBuf {
    PathBuf::from(kv.get("worktree").unwrap_or(""))
}

/// The round's tree rebuilt from `base_sha` with `members` merged in order, in `wt`. A
/// member that no longer merges leaves `wt` as it was (at `fallback`) and is the Err.
fn assemble(w: &World, c: &Ctx, path: &Path, wt: &Path, base_sha: &str, members: &[Member], fallback: &str) -> Result<String, String> {
    w.git.worktree_prune(path);
    w.git.worktree_remove(path, wt);
    if let Some(p) = wt.parent() {
        let _ = fs::create_dir_all(p);
    }
    if !w.git.worktree_add_detached(path, wt, base_sha) {
        return Err("cannot create the round worktree".into());
    }
    for m in members {
        if !w.git.merge_no_ff(wt, &w.lib.land_subject(&m.id), &m.tip, &c.s.git_name, &c.s.git_email) {
            w.git.merge_abort(wt);
            w.git.worktree_remove(path, wt);
            let _ = w.git.worktree_add_detached(path, wt, fallback);
            return Err(format!("{} no longer merges onto the round", m.id));
        }
    }
    w.git.rev_parse(wt, "HEAD").ok_or_else(|| "the round worktree has no HEAD".to_string())
}

fn open(w: &World, repo: Option<&str>, members_arg: &Text, name: Option<&str>, worktree: Option<&Path>) -> i32 {
    let label = "round open";
    let text = match read_text(w, members_arg) {
        Ok(t) => t,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the members: {e}"));
            return USAGE;
        }
    };
    let wanted = parse_members(&text);
    if wanted.is_empty() {
        w.err(format!("queue.sh {label}: --members names no bead"));
        return USAGE;
    }
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    let repo_name = c.r.name.clone();
    if let Some(n) = name {
        if idents(w, label, &[("name", n)]).is_err() {
            return FAIL;
        }
    }
    if let Some(wt) = worktree {
        if let Err(e) = crate::ident::check_path("worktree", &wt.display().to_string()) {
            w.err(format!("queue.sh {label}: {e} — refused"));
            return FAIL;
        }
    }
    for m in &wanted {
        if idents(w, label, &[("member id", &m.id)]).is_err() || (!m.tip.is_empty() && idents(w, label, &[("member tip", &m.tip)]).is_err()) {
            return FAIL;
        }
    }
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let record = c.queue_file(RECORD);
    if let Ok(Some(kv)) = records::read_kv(&record) {
        w.err(format!(
            "queue.sh {label}: a round is already open for {repo_name} ({}, phase {}) — land or abandon it first",
            kv.get("batch_id").unwrap_or("?"),
            kv.get("phase").unwrap_or("?")
        ));
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("queue.sh {label}: cannot resolve landing ref for {repo_name}"));
        return FAIL;
    };
    let Some(base_sha) = w.git.rev_parse(&path, &base) else {
        w.err(format!("queue.sh {label}: cannot resolve {base}"));
        return FAIL;
    };

    let mut skips: Vec<String> = Vec::new();
    let mut admitted: Vec<Member> = Vec::new();
    for m in wanted {
        match w.lc.bead_row(&m.id) {
            None => skips.push(format!("{}: no lifecycle row (spira-lc could not say) — not admitted", m.id)),
            // SUBMITTED or CERTIFIED (law-a-round-is-feature-first-then-catch-all): the round's own
            // full suite is the certification, so a submitted tip need not pass a per-bead gate first.
            Some(r) if r.state != "CERTIFIED" && r.state != "SUBMITTED" => {
                skips.push(format!("{}: lifecycle state={} (not SUBMITTED or CERTIFIED) — not admitted", m.id, r.state))
            }
            Some(r) => match r.tip.filter(|t| !t.is_empty()) {
                Some(tip) if tip.starts_with(&m.tip) => admitted.push(Member { id: m.id, tip }),
                Some(tip) => skips.push(format!("{}: {} is not its row's tip {tip} — not admitted", m.id, m.tip)),
                None => skips.push(format!("{}: no submitted tip — not admitted", m.id)),
            },
        }
    }

    let wt = worktree.map(Path::to_path_buf).unwrap_or_else(|| c.s.run.join("worktree").join(format!(".round-{repo_name}")));
    w.git.worktree_prune(&path);
    w.git.worktree_remove(&path, &wt);
    if let Some(p) = wt.parent() {
        let _ = fs::create_dir_all(p);
    }
    if !w.git.worktree_add_detached(&path, &wt, &base_sha) {
        w.err(format!("queue.sh {label}: cannot create the round worktree {}", wt.display()));
        return FAIL;
    }
    let mut merged: Vec<Member> = Vec::new();
    for m in admitted {
        if w.git.merge_no_ff(&wt, &w.lib.land_subject(&m.id), &m.tip, &c.s.git_name, &c.s.git_email) {
            merged.push(m);
        } else {
            w.git.merge_abort(&wt);
            let why = if w.lib.base_conflict(&path, &base_sha, &m.tip) { "conflicts with base" } else { "conflicts with the round" };
            skips.push(format!("{}: {why}", m.id));
        }
    }
    for s in &skips {
        w.out(format!("queue.sh {label}: skip — {s}"));
    }
    if merged.is_empty() {
        w.err(format!("queue.sh {label}: no round — nothing admissible for {repo_name}"));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }
    let head = w.git.rev_parse(&wt, "HEAD").unwrap_or_default();
    let batch = name.map(str::to_string).unwrap_or_else(|| format!("{repo_name}-{}", w.clock.stamp()));
    if let Err((rc, out)) = w.lc.cut(&batch, &repo_name, &head, &base_sha, &csv(&merged), LC_ACTOR) {
        w.err(format!("queue.sh {label}: spira-lc cut refused for {batch} (rc={rc}): {out}"));
        w.git.worktree_remove(&path, &wt);
        return FAIL;
    }

    let now = w.clock.now().to_string();
    let mut kv = Kv::default();
    kv.push("batch_id", &batch);
    kv.push("repo", &repo_name);
    kv.push("head", &head);
    kv.push("base", &base_sha);
    kv.push("members", &render_members(&merged));
    kv.push("worktree", &wt.display().to_string());
    kv.push("actor", &actor(w));
    kv.push("opened", &now);
    set_phase(w, &mut kv, "opened");
    if save(w, label, &c, &kv).is_err() {
        return FAIL;
    }
    mark_running(&c, &batch);
    landing_log(&c.s.run, &format!("QUEUE ROUND-OPEN {now} repo={repo_name} batch={batch} head={head} members={}", csv(&merged)));
    w.out(format!("batch={batch}"));
    w.out(format!("head={head}"));
    w.out(format!("members={}", render_members(&merged)));
    OK
}

fn status(w: &World, repo: Option<&str>) -> i32 {
    let label = "round status";
    let Ok(c) = resolve(w, label, repo) else { return FAIL };
    let kv = match records::read_kv(&c.queue_file(RECORD)) {
        Ok(Some(kv)) => kv,
        _ => {
            w.out(format!("round=none\nrepo={}", c.r.name));
            return OK;
        }
    };
    let since = |k: &str| kv.get(k).and_then(|v| v.parse::<u64>().ok()).map_or(0, |t| w.clock.now().saturating_sub(t));
    w.out("round=open");
    for k in ["batch_id", "repo", "phase", "head", "base", "members"] {
        w.out(format!("{k}={}", kv.get(k).unwrap_or("")));
    }
    if let Some(red) = kv.get("red").filter(|r| !r.is_empty()) {
        w.out(format!("red={red}"));
    }
    w.out(format!("wall_secs={}", since("opened")));
    w.out(format!("phase_secs={}", since("phase_at")));
    OK
}

fn suite_statuses(dir: &Path, found: &mut Vec<(String, String)>, depth: u32) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 2 {
            suite_statuses(&p, found, depth + 1);
        } else if let Some(suite) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".result")) {
            let status = fs::read_to_string(&p).ok().and_then(|t| t.split_whitespace().next().map(String::from)).unwrap_or_default();
            found.push((suite.to_string(), status));
        }
    }
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn certify(w: &World, batch: &str, repo: Option<&str>, attest: Option<&str>) -> i32 {
    let label = "round certify";
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let who = actor(w);

    let (head, wt, run_id) = {
        let Ok(_g) = lock(w, label, &c) else { return FAIL };
        let Ok(mut kv) = load(w, label, &c, batch) else { return FAIL };
        let wt = worktree_of(&kv);
        let mut head = kv.get("head").unwrap_or("").to_string();
        if let Some(a) = attest {
            let at = w.git.rev_parse(&path, a);
            let at_wt = w.git.rev_parse(&wt, "HEAD");
            if at.is_none() || at != at_wt {
                w.err(format!("queue.sh {label}: --attest {a} is not the round worktree's head ({}) — refused", at_wt.unwrap_or_else(|| "<none>".into())));
                return FAIL;
            }
            head = at.unwrap_or_default();
            set(&mut kv, "head", &head);
        } else if w.git.rev_parse(&wt, "HEAD").as_deref() != Some(head.as_str()) {
            w.err(format!("queue.sh {label}: the round worktree {} is not at the round's head {head} — refused", wt.display()));
            return FAIL;
        }
        let run_id = format!("{batch}-{}", w.clock.now());
        let started = lc_cas(w, batch, |s, v| {
            if s == "OPEN" {
                w.lc.batch_event(batch, s, v, LC_ACTOR, &format!("{{\"CiStarted\":{{\"run\":\"{run_id}\"}}}}"))
            } else {
                Ok(())
            }
        });
        if let Err((rc, out)) = started {
            w.err(format!("queue.sh {label}: spira-lc refused to start the round {batch} (rc={rc}): {out}"));
            return FAIL;
        }
        set_phase(w, &mut kv, "certifying");
        kv.remove("red");
        if save(w, label, &c, &kv).is_err() {
            return FAIL;
        }
        mark_running(&c, batch);
        (head, wt, run_id)
    };

    let mut reds: Vec<String> = Vec::new();
    if attest.is_none() {
        let results = rounds_dir(&c).join(format!("{batch}.results"));
        let _ = fs::remove_dir_all(&results);
        let out = w.scripts.round_vm(&wt, &results, c.s.round_wall_secs);
        let mut found = Vec::new();
        suite_statuses(&results, &mut found, 0);
        let fault = match out.rc {
            0 | 1 if found.is_empty() => Some("round-vm ran and left no verdicts — a fault of the round machinery, not of the candidates".to_string()),
            0 | 1 => None,
            4 => {
                reds.push("workspace-build".into());
                None
            }
            124 | 137 => Some(format!("round-vm exceeded the {}s wall", c.s.round_wall_secs)),
            rc => Some(format!("round-vm exited {rc}")),
        };
        if let Some(why) = fault {
            let _g = lock(w, label, &c);
            if let Ok(mut kv) = load(w, label, &c, batch) {
                set_phase(w, &mut kv, "fault");
                let _ = save(w, label, &c, &kv);
            }
            w.err(format!("queue.sh {label}: {why}; the round is not judged\n{}", tail(&out.err, 20)));
            return FAULT;
        }
        reds.extend(found.iter().filter(|(_, s)| BLOCKING.contains(&s.as_str())).map(|(n, _)| n.clone()));
        reds.sort();
        reds.dedup();
    }

    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Ok(mut kv) = load(w, label, &c, batch) else { return FAIL };
    if kv.get("phase") != Some("certifying") || kv.get("head") != Some(head.as_str()) {
        w.err(format!("queue.sh {label}: the round {batch} changed while it was being certified — the result is discarded; certify again"));
        return FAIL;
    }
    if !reds.is_empty() {
        set(&mut kv, "red", &reds.join(","));
        set_phase(w, &mut kv, "red");
        if save(w, label, &c, &kv).is_err() {
            return FAIL;
        }
        mark_running(&c, batch);
        w.out(format!("queue.sh {label}: round {batch} is RED at {head}: {}", reds.join(",")));
        w.out(format!("red={}", reds.join(",")));
        return FAIL;
    }

    let greened = lc_cas(w, batch, |s, v| match s {
        "CI_RUNNING" => w.lc.batch_event(batch, s, v, LC_ACTOR, "\"Green\""),
        "GREEN" => Ok(()),
        other => Err((1, format!("the batch is {other}, not CI_RUNNING"))),
    });
    if let Err((rc, out)) = greened {
        w.err(format!("queue.sh {label}: spira-lc refused the GREEN for {batch} (rc={rc}): {out}"));
        return FAIL;
    }
    let Some(tree) = w.git.rev_parse(&path, &format!("{head}^{{tree}}")) else {
        w.err(format!("queue.sh {label}: cannot resolve {head}^{{tree}} — nothing can say what was judged"));
        return FAIL;
    };
    let cert = gate::cert::Cert {
        source: gate::cert::Source::Round,
        tree: tree.clone(),
        repo: c.r.name.clone(),
        rev: head.clone(),
        branch: format!("round/{batch}"),
        by: who,
        when: w.clock.stamp(),
        at: w.clock.now(),
        harness: "-".into(),
        suites: "full-corpus".into(),
    };
    let verdicts = gate::cert::verdicts_dir(w.var("SPIRA_VERDICTS").as_deref(), &c.s.run);
    if let Err(e) = gate::cert::write(&verdicts, &cert) {
        w.err(format!("queue.sh {label}: cannot record the round certificate for {head}: {e}"));
        return FAIL;
    }
    // A green round IS the full-suite pass on its head (sp-x334k): publish refuses a head with no
    // full-suite local-pass record, and the round's full corpus is the run that earns it.
    if let Err(e) = spira_config::local_pass::record(&c.s.run, spira_config::local_pass::Kind::FullSuite, &head, &format!("round {batch}"), &w.clock.now().to_string()) {
        w.err(format!("queue.sh {label}: cannot record the full-suite local pass for {head}: {e}"));
        return FAIL;
    }
    set_phase(w, &mut kv, "green");
    if save(w, label, &c, &kv).is_err() {
        return FAIL;
    }
    mark_running(&c, batch);
    landing_log(&c.s.run, &format!("QUEUE ROUND-GREEN {} repo={} batch={batch} head={head} tree={tree} run={run_id}", w.clock.now(), c.r.name));
    w.out(format!("queue.sh {label}: round {batch} is GREEN at {head} (tree {tree})"));
    OK
}

#[allow(clippy::too_many_arguments)]
fn eject(w: &World, batch: &str, id: &str, repo: Option<&str>, reason: &Text, suites: &str, red: bool, rebuild: bool) -> i32 {
    let label = "round eject";
    let reason = match read_text(w, reason) {
        Ok(r) if !r.trim().is_empty() => r,
        Ok(_) => {
            w.err(format!("queue.sh {label}: --reason is required — the bead carries it back to its builder"));
            return USAGE;
        }
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot read the reason: {e}"));
            return USAGE;
        }
    };
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if idents(w, label, &[("batch id", batch), ("bead id", id)]).is_err() {
        return FAIL;
    }
    if !suites.is_empty() && suites.split(',').try_for_each(|s| idents(w, label, &[("suite", s)])).is_err() {
        return FAIL;
    }
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Ok(mut kv) = load(w, label, &c, batch) else { return FAIL };
    let members = kv.members();
    if !members.iter().any(|m| m.id == id) {
        let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
        w.err(format!("queue.sh {label}: {id} is not a member of round {batch} (members: {})", ids.join(" ")));
        return FAIL;
    }
    let survivors: Vec<Member> = members.iter().filter(|m| m.id != id).cloned().collect();
    let wt = worktree_of(&kv);
    let old_head = kv.get("head").unwrap_or("").to_string();
    let base_sha = kv.get("base").unwrap_or("").to_string();

    let mut new_head = old_head.clone();
    if rebuild && !survivors.is_empty() {
        match assemble(w, &c, &path, &wt, &base_sha, &survivors, &old_head) {
            Ok(h) => new_head = h,
            Err(why) => {
                w.err(format!("queue.sh {label}: cannot rebuild the round without {id}: {why} — nothing changed"));
                return FAIL;
            }
        }
    }

    let cause = EjectCause::decide(red, suites);
    w.lib.bead_reopen(id, cause.as_str(), suites);
    lc_return(w, id);
    w.lib.release_claim(id);
    let why = bounded_text(&reason);
    match lc_cas(w, batch, |s, v| w.lc.eject_member(batch, id, s, v, LC_ACTOR, &why)) {
        Ok(()) => {}
        Err((rc, out)) => w.err(format!("queue.sh {label}: spira-lc eject-member refused for {id} (rc={rc}): {out}")),
    }
    let mut comment = format!("Ejected from round {batch} in {}.\n\n{reason}", c.r.name);
    comment.push_str("\n\nFix the failing issue and re-certify before rejoining the queue.");
    if !suites.is_empty() {
        comment.push_str(&format!("\n\nRecertification will force these suites regardless of SPIRA_CERTIFY_SUITES: {suites}"));
    }
    w.lib.comment(id, &comment);
    landing_log(&c.s.run, &format!("QUEUE ROUND-EJECT {} repo={} batch={batch} id={id} cause={} reason={}", w.clock.now(), c.r.name, cause.as_str(), one_line(&reason)));

    if survivors.is_empty() {
        let emptied = bounded_text(&format!("round emptied: {id} ejected — {reason}"));
        if let Err((rc, out)) = lc_cas(w, batch, |s, v| w.lc.abandon_batch(batch, s, v, LC_ACTOR, &emptied)) {
            w.err(format!("queue.sh {label}: spira-lc abandon-batch refused for the emptied round {batch} (rc={rc}): {out}"));
        }
        let _ = fs::remove_file(c.queue_file(RECORD));
        w.git.worktree_remove(&path, &wt);
        finish(&c, batch, &format!("emptied: {id} ejected, no member remains"));
        w.out(format!("queue.sh {label}: ejected {id}; the round {batch} has no member left and is closed"));
        return OK;
    }

    set(&mut kv, "members", &render_members(&survivors));
    set(&mut kv, "head", &new_head);
    kv.remove("red");
    let ejected = format!("{}{id} ", kv.get("ejected").unwrap_or(""));
    set(&mut kv, "ejected", &ejected);
    set_phase(w, &mut kv, "opened");
    if save(w, label, &c, &kv).is_err() {
        return FAIL;
    }
    mark_running(&c, batch);
    w.out(format!("queue.sh {label}: ejected {id} from round {batch}"));
    w.out(format!("head={new_head}"));
    w.out(format!("members={}", render_members(&survivors)));
    OK
}

fn land(w: &World, batch: &str, repo: Option<&str>) -> i32 {
    let label = "round land";
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Ok(kv) = load(w, label, &c, batch) else { return FAIL };
    let phase = kv.get("phase").unwrap_or("");
    if phase != "green" {
        w.err(format!("queue.sh {label}: round {batch} is {phase}, not green — certify it first"));
        return FAIL;
    }
    let head = kv.get("head").unwrap_or("").to_string();
    let members = kv.members();
    let wt = worktree_of(&kv);
    let bins = wt.join("target").join("release").is_dir().then_some(wt.as_path());
    let rc = land_local_with(w, Some(&c.r.name), &head, &Text::Arg(csv(&members)), bins, true);
    let base = c.r.landref.clone().unwrap_or_default();
    if rc != OK && !w.git.is_ancestor(&path, &head, &base) {
        return rc;
    }

    match lc_cas(w, batch, |_, v| w.lc.land_batch(batch, v, LC_ACTOR, &head)) {
        Ok(()) => {}
        Err((rc, out)) => w.err(format!("queue.sh {label}: {head} landed but spira-lc refused the batch landing for {batch} (rc={rc}): {out}")),
    }
    let _ = fs::remove_file(c.queue_file(RECORD));
    w.git.worktree_remove(&path, &wt);
    finish(&c, batch, &format!("landed {head} members={}", csv(&members)));
    landing_log(&c.s.run, &format!("QUEUE ROUND-LAND {} repo={} batch={batch} head={head} members={}", w.clock.now(), c.r.name, csv(&members)));
    w.out(format!("queue.sh {label}: round {batch} landed at {head} — {} member(s)", members.len()));
    rc
}

fn abandon(w: &World, batch: &str, repo: Option<&str>, reason: &Text) -> i32 {
    let label = "round abandon";
    let reason = read_text(w, reason).unwrap_or_default();
    if reason.trim().is_empty() {
        w.err(format!("queue.sh {label}: --reason is required — pass --reason \"<why>\" so the abandon is auditable"));
        return USAGE;
    }
    let Ok((c, path)) = local_ctx(w, label, repo) else { return FAIL };
    if idents(w, label, &[("batch id", batch)]).is_err() || require_lc(w, label).is_err() {
        return FAIL;
    }
    let Ok(_g) = lock(w, label, &c) else { return FAIL };
    let Ok(kv) = load(w, label, &c, batch) else { return FAIL };
    let members = kv.members();
    let why = bounded_text(&reason);
    match lc_cas(w, batch, |s, v| w.lc.abandon_batch(batch, s, v, LC_ACTOR, &why)) {
        Ok(()) => w.out(format!("queue.sh {label}: {batch} abandoned on spira-lc")),
        Err((1, out)) if out.is_empty() => w.err(format!("queue.sh {label}: spira-lc has no batch row for {batch}")),
        Err((rc, out)) => {
            w.err(format!("queue.sh {label}: spira-lc abandon-batch refused for {batch} (rc={rc}): {out} — the round stays open"));
            return FAIL;
        }
    }
    for m in &members {
        let state = w.lc.bead_row(&m.id).map(|r| r.state).unwrap_or_else(|| "unknown".into());
        w.out(format!("queue.sh {label}: {}: {state}", m.id));
    }
    let _ = fs::remove_file(c.queue_file(RECORD));
    w.git.worktree_remove(&path, &worktree_of(&kv));
    finish(&c, batch, &format!("abandoned: {}", one_line(&reason)));
    landing_log(&c.s.run, &format!("QUEUE ROUND-ABANDON {} repo={} batch={batch} reason={}", w.clock.now(), c.r.name, one_line(&reason)));
    w.out(format!("queue.sh {label}: round {batch} abandoned for {}", c.r.name));
    OK
}
