//! `queue round mended|return|environment|flaky|unsure|triage-ended|await-ask <batch> <failure>`: a
//! mender's outcome for one failed suite. The verbs only record; the batcher decides the next
//! pass from the recorded outcomes. Everything that bounds a mender (deadline, caps, the
//! escalation check, one writer per branch) is enforced by spira-lc, not here.

use std::collections::BTreeMap;
use std::path::Path;

use lifecycle::mending::{self, FileChange, MendFacts};

use super::round::load_failed_batch;
use super::{
    actor, czar_ok, idents, read_text, repo_path, require_lc, resolve, World, FAIL, OK, USAGE,
};
use crate::cli::Text;
use crate::model::Member;

/// Repo-relative list of gate and fence paths a mend may touch only with an ask attached.
const FENCE_LIST: &str = "spira/mend-fences";
const SUITE_GLOB: &str = "**/test-*.sh";

#[allow(clippy::too_many_arguments)]
pub fn run(
    w: &World,
    outcome: &str,
    batch: &str,
    suite: &str,
    repo: Option<&str>,
    tips: &str,
    checked: &str,
    ask: Option<&str>,
    reason: &Text,
) -> i32 {
    let label = format!("round {outcome}");
    if !czar_ok(w) {
        return FAIL;
    }
    let Ok(c) = resolve(w, &label, repo) else {
        return FAIL;
    };
    let Ok(path) = repo_path(w, &label, &c) else {
        return FAIL;
    };
    if idents(w, &label, &[("batch id", batch), ("suite", suite)]).is_err()
        || require_lc(w, &label).is_err()
    {
        return FAIL;
    }
    let Ok(kv) = load_failed_batch(w, &label, &c, batch) else {
        return FAIL;
    };
    let Some((pass, _)) = w.lc.batch_pass(batch) else {
        w.err(format!(
            "queue.sh {label}: spira-lc cannot say which pass {batch} is on"
        ));
        return FAIL;
    };
    let text = read_text(w, reason).unwrap_or_default();
    let diagnosis = text.trim();
    let by = actor(w);
    let send = |kind: serde_json::Value| {
        w.lc.mending_event(batch, pass, suite, &kind.to_string(), &by)
    };
    let report = |r: Result<(), (i32, String)>, did: &str| match r {
        Ok(()) => {
            w.out(format!(
                "queue.sh {label}: {did} for {suite} in pass {pass} of {batch}"
            ));
            OK
        }
        Err((rc, out)) => {
            w.err(format!(
                "queue.sh {label}: spira-lc refused for {suite} (rc={rc}): {out}"
            ));
            FAIL
        }
    };
    let now = || w.clock.now() as i64;
    let write_diagnosis = || {
        if !diagnosis.is_empty() {
            let _ = send(serde_json::json!({"Diagnosis": {"text": diagnosis}}));
        }
    };
    match outcome {
        "triage-ended" => report(
            send(serde_json::json!({"TriageEnded": {"at": now()}})),
            "triage ended",
        ),
        "await-ask" => {
            let Some(a) = ask else {
                w.err(format!("queue.sh {label}: --ask <id> is required: the open ask the mend waits on"));
                return USAGE;
            };
            if idents(w, &label, &[("ask id", a)]).is_err() {
                return FAIL;
            }
            report(send(serde_json::json!({"Await": {"ask": a}})), "awaiting its ask")
        }
        "return" => {
            if diagnosis.is_empty() {
                w.err(format!(
                    "queue.sh {label}: --reason is required: a return carries its diagnosis"
                ));
                return USAGE;
            }
            stamp_triage(&send, now());
            report(
                send(serde_json::json!({"Return": {"at": now(), "diagnosis": diagnosis}})),
                "returned",
            )
        }
        "environment" | "flaky" | "unsure" => {
            write_diagnosis();
            stamp_triage(&send, now());
            let (name, did) = match outcome {
                "environment" => ("Environment", "recorded as the environment"),
                "flaky" => ("Flaky", "recorded as flaky"),
                _ => ("Unsure", "recorded as unsure"),
            };
            let r = send(serde_json::json!({ name: {"at": now()} }));
            if outcome == "flaky" && r.is_ok() {
                w.scripts.observe_flake(suite, kv.get("head").unwrap_or(""));
            }
            report(r, did)
        }
        _ => mended(
            w, &label, &path, &kv, batch, pass, suite, tips, checked, ask, diagnosis, &by,
        ),
    }
}

fn stamp_triage(send: &dyn Fn(serde_json::Value) -> Result<(), (i32, String)>, at: i64) {
    let _ = send(serde_json::json!({"TriageEnded": {"at": at}}));
}

#[allow(clippy::too_many_arguments)]
fn mended(
    w: &World,
    label: &str,
    path: &Path,
    kv: &crate::records::Kv,
    batch: &str,
    pass: u32,
    suite: &str,
    tips: &str,
    checked: &str,
    ask: Option<&str>,
    diagnosis: &str,
    by: &str,
) -> i32 {
    let wanted: Vec<(String, String)> = tips
        .split(',')
        .filter(|t| !t.is_empty())
        .filter_map(|t| {
            t.split_once(':')
                .map(|(b, tip)| (b.to_string(), tip.to_string()))
        })
        .collect();
    if wanted.is_empty() {
        w.err(format!(
            "queue.sh {label}: --tips <bead:tip,...> is required: the mended tips"
        ));
        return USAGE;
    }
    if wanted
        .iter()
        .try_for_each(|(b, t)| idents(w, label, &[("bead id", b), ("tip", t)]))
        .is_err()
    {
        return FAIL;
    }
    if let Some(a) = ask {
        if idents(w, label, &[("ask id", a)]).is_err() {
            return FAIL;
        }
    }
    let members: Vec<Member> = kv.members();
    let base = kv.get("base").unwrap_or("").to_string();
    let beads: Vec<String> = wanted.iter().map(|(b, _)| b.clone()).collect();
    let recorded: BTreeMap<String, String> = match w.lc.mend_tips(&beads) {
        Ok(t) => t,
        Err(e) => {
            w.err(format!(
                "queue.sh {label}: cannot read the recorded mends: {e}"
            ));
            return FAIL;
        }
    };
    let giveup = |why: &str| {
        let _ = w.lc.mending_event(
            batch,
            pass,
            suite,
            &serde_json::json!({"Return": {"at": 0, "diagnosis": why}}).to_string(),
            by,
        );
        w.err(format!(
            "queue.sh {label}: {why} — the failure is returned with that diagnosis"
        ));
        FAIL
    };

    let mut moves = Vec::new();
    let mut changes: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut required: Vec<String> = Vec::new();
    for (bead, tip) in &wanted {
        let Some(m) = members.iter().find(|m| m.id == *bead) else {
            w.err(format!(
                "queue.sh {label}: {bead} is not a member of round {batch}"
            ));
            return FAIL;
        };
        if !w.git.commit_exists(path, tip) {
            w.err(format!(
                "queue.sh {label}: {tip} is not a commit in {}",
                path.display()
            ));
            return FAIL;
        }
        let current = recorded.get(bead).cloned().unwrap_or_else(|| m.tip.clone());
        if current == *tip {
            w.err(format!(
                "queue.sh {label}: {bead} at {tip} is not changed by this mend"
            ));
            return FAIL;
        }
        if !w.git.is_ancestor(path, &current, tip) {
            return giveup(&format!("fix no longer applies: {tip} for {bead} is not built on the branch's current tip {current}"));
        }
        let stat = match w.git.numstat(path, &current, tip) {
            Ok(s) => s,
            Err(e) => {
                w.err(format!(
                    "queue.sh {label}: cannot diff {bead} {current}..{tip}: {e}"
                ));
                return FAIL;
            }
        };
        let paths: Vec<String> = stat.iter().map(|(p, _, _)| p.clone()).collect();
        for (p, a, d) in stat {
            let e = changes.entry(p).or_default();
            e.0 += a;
            e.1 += d;
        }
        required.extend(mending::required_checks(
            &paths,
            |p| crate_dir(w, path, tip, p),
            |p| {
                let name = p.rsplit('/').next().unwrap_or(p);
                w.git.grep_files(path, tip, name, SUITE_GLOB)
            },
        ));
        moves.push(format!("{bead}:{current}:{tip}"));
    }
    required.sort();
    required.dedup();

    let mut member_files: Vec<String> = Vec::new();
    for m in &members {
        match w.git.numstat(path, &base, &m.tip) {
            Ok(s) => member_files.extend(s.into_iter().map(|(p, _, _)| p)),
            Err(e) => {
                w.err(format!(
                    "queue.sh {label}: cannot diff the member {} against the base: {e}",
                    m.id
                ));
                return FAIL;
            }
        }
    }
    let Some(fences) = w.git.file_at(path, &base, FENCE_LIST) else {
        w.err(format!("queue.sh {label}: {FENCE_LIST} is not at the round's base — a mend cannot be assessed without the fence list, refused"));
        return FAIL;
    };
    let fences: Vec<String> = fences
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    let facts = MendFacts {
        changes: changes
            .into_iter()
            .map(|(path, (added, deleted))| FileChange {
                path,
                added,
                deleted,
            })
            .collect(),
        member_files,
        required_checks: required,
        checked: checked
            .split(',')
            .filter(|c| !c.is_empty())
            .map(String::from)
            .collect(),
        ask: ask.map(String::from),
    };
    if !diagnosis.is_empty() {
        let _ = w.lc.mending_event(
            batch,
            pass,
            suite,
            &serde_json::json!({"Diagnosis": {"text": diagnosis}}).to_string(),
            by,
        );
    }
    let _ = w.lc.mending_event(
        batch,
        pass,
        suite,
        &serde_json::json!({"TriageEnded": {"at": w.clock.now() as i64}}).to_string(),
        by,
    );
    let facts_json = serde_json::to_string(&facts).unwrap_or_default();
    let fences_json = serde_json::to_string(&fences).unwrap_or_default();
    match w.lc.mend(
        batch,
        pass,
        suite,
        &moves.join(","),
        &facts_json,
        &fences_json,
        by,
    ) {
        Ok(()) => {
            w.out(format!(
                "queue.sh {label}: mended {suite} in pass {pass} of {batch}: {}",
                moves.join(" ")
            ));
            OK
        }
        Err((rc, out)) => {
            if out.contains("MendCap") {
                escalate_cap(w, batch, suite, &out);
            }
            w.err(format!(
                "queue.sh {label}: spira-lc refused the mend (rc={rc}): {out}"
            ));
            FAIL
        }
    }
}

/// The crate directory a file belongs to: the nearest ancestor holding a Cargo.toml at `rev`.
fn crate_dir(w: &World, repo: &Path, rev: &str, file: &str) -> Option<String> {
    let mut dir = file.rsplit_once('/')?.0;
    loop {
        if w.git.path_exists(repo, rev, &format!("{dir}/Cargo.toml")) {
            return Some(dir.to_string());
        }
        dir = dir.rsplit_once('/')?.0;
    }
}

/// A bead that fails a third time is returned with its diagnoses and one ask. The machine
/// returns the failure once, so this is sent once.
fn escalate_cap(w: &World, batch: &str, suite: &str, detail: &str) {
    let body = format!(
        "{suite} failed in round {batch} for a bead the mender cap already covers.\n\n{detail}\n\n\
         The failure was returned with the diagnoses so far and the bead leaves the round.\n\
         Default: leave it returned; a builder takes it with those diagnoses.\n"
    );
    w.scripts
        .mail_operator(&format!("A bead hit the mend cap in round {batch}"), &body);
}
