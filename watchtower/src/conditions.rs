//! Standing conditions: a probe reports what is true now; this turns that into one incident
//! bead per condition while it stands, and closes the bead when it stops standing.
//!
//! A condition that stays crossed files nothing further (an open bead for its reference
//! already holds it). One that clears has its bead closed. A probe that could not read
//! reports [`Reading::Unknown`], which neither files nor clears.

use crate::incident::{self, Finding};
use crate::log::log;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    pub key: String,
    pub reference: String,
    pub title: String,
    pub body: String,
    pub priority: u32,
    pub sustain_secs: i64,
}

pub enum Reading {
    Unknown,
    Standing(Vec<Cond>),
}

pub struct Ctx<'a> {
    pub run: &'a Path,
    pub db: &'a str,
    pub home_repo: &'a str,
    pub incident_sh: &'a str,
    /// The store's client: bd's rows through its content door.
    pub lc: &'a spira_store::lc_call::LcClient,
    /// The lifecycle machine (`spira-lc`): an incident bead is a work bead, so whether it is
    /// still unfinished is its lifecycle row's answer, never bd's status (sp-jgjvh).
    pub lc_bin: &'a str,
    /// `SPIRA_CONDITIONS_MAX_FILE`: the alarms one pass raises across all its probes.
    pub max_file: usize,
    pub alarm: &'a dyn Fn(&str, &Finding) -> incident::AlarmOutcome,
    pub filed: std::cell::Cell<usize>,
    pub deferred: std::cell::RefCell<Vec<String>>,
}

impl<'a> Ctx<'a> {
    pub fn new(run: &'a Path, db: &'a str, home_repo: &'a str, incident_sh: &'a str, lc: &'a spira_store::lc_call::LcClient, lc_bin: &'a str, max_file: usize) -> Self {
        Ctx { run, db, home_repo, incident_sh, lc, lc_bin, max_file, alarm: &incident::alarm_outcome, filed: Default::default(), deferred: Default::default() }
    }

    /// Names the conditions the cap held back, in one line, once the pass's probes have run.
    pub fn end_pass(&self) {
        let held = std::mem::take(&mut *self.deferred.borrow_mut());
        if !held.is_empty() {
            log(&format!("watchtower: conditions: {} filed this pass; left for the next: {}", self.max_file, held.join(", ")));
        }
    }
}

fn safe(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

fn dir(ctx: &Ctx, kind: &str, probe: &str) -> PathBuf {
    ctx.run.join("conditions").join(kind).join(safe(probe))
}

fn names(d: &Path) -> BTreeSet<String> {
    std::fs::read_dir(d).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

/// Ids of the unfinished beads holding `reference`; Err is the store's own text when it did
/// not answer. The store names which beads carry the reference (content); whether each is
/// unfinished is its lifecycle row's (incident beads are work beads).
pub(crate) fn open_beads_on(lc: &spira_store::lc_call::LcClient, reference: &str) -> Result<Vec<String>, String> {
    let ids = crate::lc::by_ref(lc, reference).map_err(|e| e.to_string())?;
    unfinished(lc, ids)
}

/// The ids whose lifecycle row still owes builder work (READY, WORKING, REWORK). A bead the
/// machine has no row for is not live work (it can never be claimed; CHECK-ROWLESS reports
/// it). Err when the machine did not answer: a control that cannot check must not read as
/// "nothing holds it".
pub(crate) fn unfinished(lc: &spira_store::lc_call::LcClient, ids: Vec<String>) -> Result<Vec<String>, String> {
    let rows = spira_store::lc_state::list_ids_on(lc, &ids)?;
    Ok(ids.into_iter().filter(|id| rows.iter().any(|r| &r.bead_id == id && !r.past_builder())).collect())
}

// Through the lifecycle machine (sp-3fue0j), never a raw bd close.
fn close_bead(ctx: &Ctx, id: &str, why: &str) -> bool {
    spira_store::lifecycle_row::close_with(ctx.lc_bin, id, &format!("OUTCOME: delivered\n{why}\n"), "watchtower", None).is_ok()
}

/// Logs a failed read once per condition and cause: the unread file holds the text last logged.
fn unreadable(unread: &Path, k: &str, probe: &str, what: &str, err: &str) -> bool {
    if std::fs::read_to_string(unread.join(k)).is_ok_and(|seen| seen == err) {
        return false;
    }
    let _ = std::fs::write(unread.join(k), err);
    log(&format!("watchtower: conditions: {what} — the store did not answer: {err} ({probe})"));
    true
}

pub fn reconcile(now: i64, ctx: &Ctx, probe: &str, reading: Reading) {
    let Reading::Standing(conds) = reading else {
        log(&format!("watchtower: conditions: {probe} could not read — nothing filed or cleared"));
        return;
    };
    let filed_dir = dir(ctx, "filed", probe);
    let pending_dir = dir(ctx, "pending", probe);
    let unread_dir = dir(ctx, "unread", probe);
    let _ = std::fs::create_dir_all(&unread_dir);
    let _ = std::fs::create_dir_all(&filed_dir);
    let _ = std::fs::create_dir_all(&pending_dir);
    let standing: BTreeSet<String> = conds.iter().map(|c| safe(&c.key)).collect();

    for c in &conds {
        let k = safe(&c.key);
        let stamp = pending_dir.join(&k);
        let since = std::fs::read_to_string(&stamp).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or_else(|| {
            let _ = std::fs::write(&stamp, now.to_string());
            now
        });
        if now - since < c.sustain_secs {
            continue;
        }
        match open_beads_on(ctx.lc, &c.reference) {
            Ok(ids) if ids.is_empty() => {
                let _ = std::fs::remove_file(unread_dir.join(&k));
                if !incident::is_usable(ctx.incident_sh) {
                    log(&format!("watchtower: conditions: {} is missing — {} not filed", ctx.incident_sh, c.key));
                    continue;
                }
                if ctx.filed.get() >= ctx.max_file {
                    ctx.deferred.borrow_mut().push(c.reference.clone());
                    continue;
                }
                let f = Finding::new(ctx.db, ctx.home_repo, &c.title, &c.body).priority(c.priority).reference(&c.reference).cause(probe);
                match (ctx.alarm)(ctx.incident_sh, &f) {
                    incident::AlarmOutcome::Noted => {
                        ctx.filed.set(ctx.filed.get() + 1);
                        let _ = std::fs::write(filed_dir.join(&k), &c.reference);
                        log(&format!("watchtower: conditions: filed {} ({probe})", c.reference));
                    }
                    incident::AlarmOutcome::Deduped => {
                        let _ = std::fs::write(filed_dir.join(&k), &c.reference);
                    }
                    incident::AlarmOutcome::Failed => {}
                }
            }
            Ok(_) => {
                let _ = std::fs::remove_file(unread_dir.join(&k));
            }
            Err(e) => {
                unreadable(&unread_dir, &k, probe, &format!("{} stands but could not be checked — not filed", c.reference), &e);
            }
        }
    }

    for k in names(&pending_dir).difference(&standing) {
        let _ = std::fs::remove_file(pending_dir.join(k));
    }
    for k in names(&filed_dir).difference(&standing) {
        let marker = filed_dir.join(k);
        let Ok(reference) = std::fs::read_to_string(&marker) else { continue };
        let ids = match open_beads_on(ctx.lc, reference.trim()) {
            Ok(ids) => ids,
            Err(e) => {
                let _ = unreadable(&unread_dir, k, probe, &format!("{} cleared but could not be checked — not closed", reference.trim()), &e);
                continue;
            }
        };
        let all_closed = ids.iter().all(|id| close_bead(ctx, id, &format!("The condition {} no longer holds; watchtower resolved this incident.", reference.trim())));
        if all_closed {
            let _ = std::fs::remove_file(&marker);
            let _ = std::fs::remove_file(unread_dir.join(k));
            log(&format!("watchtower: conditions: cleared {} ({probe})", reference.trim()));
        }
    }
    let still_filed = names(&filed_dir);
    for k in names(&unread_dir).difference(&standing).filter(|k| !still_filed.contains(*k)) {
        let _ = std::fs::remove_file(unread_dir.join(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lc::scripted::{scripted, scripted_by};

    fn stand(key: &str) -> Cond {
        Cond { key: key.into(), reference: format!("incident:{key}"), title: format!("t {key}"), body: "b".into(), priority: 2, sustain_secs: 0 }
    }

    fn recording_incident(d: &Path) -> String {
        let inc = d.join("inc.sh");
        std::fs::write(&inc, "#!/usr/bin/env bash\n").unwrap();
        inc.to_str().unwrap().to_string()
    }

    fn raised(d: &Path) -> Vec<String> {
        std::fs::read_to_string(d.join("raised.txt")).unwrap_or_default().lines().map(String::from).collect()
    }

    fn record(d: &Path) -> impl Fn(&str, &Finding) -> incident::AlarmOutcome + '_ {
        move |_, f| {
            let mut all = raised(d);
            all.push(f.reference.clone().unwrap_or_default());
            std::fs::write(d.join("raised.txt"), all.join("\n")).unwrap();
            incident::AlarmOutcome::Noted
        }
    }

    #[test_id(titpuuq)]
    #[test]
    fn beads_by_reference_come_through_content_ref_and_one_row_read() {
        let (t, lc) = scripted_by(&[("content ref", 0, r#"[{"id":"sp-a"},{"id":"sp-b"}]"#), ("list --ids", 0, r#"[{"bead_id":"sp-a","state":"READY"},{"bead_id":"sp-b","state":"LANDED"}]"#)]);
        assert_eq!(open_beads_on(&lc, "incident:x"), Ok(vec!["sp-a".to_string()]));
        let seen = t.seen.lock().unwrap();
        assert_eq!(seen[0], vec!["content", "ref", "incident:x", "--json"]);
        assert_eq!(seen.len(), 2, "one row read for all the ids");
        drop(seen);
        let down = scripted(2, "cannot tell").1;
        assert!(open_beads_on(&down, "incident:x").is_err(), "no answer is unknown, never no beads");
        assert!(open_beads_on(&scripted(3, "refused").1, "incident:x").is_err());
    }

    #[test_id(td8ibt2)]
    #[test]
    fn the_same_unread_error_logs_once_and_a_different_one_logs_again() {
        let d = testkit::TempDir::new("wt-cond-unread");
        assert!(unreadable(d.path(), "k", "p", "w", "refused"));
        assert!(!unreadable(d.path(), "k", "p", "w", "refused"));
        assert!(unreadable(d.path(), "k", "p", "w", "deadline"));
        assert!(unreadable(d.path(), "other", "p", "w", "deadline"), "per condition");
    }

    #[test_id(tvuhx8a)]
    #[test]
    fn an_unread_condition_leaves_its_error_until_it_reads_or_clears() {
        let d = testkit::TempDir::new("wt-cond-unread-file");
        let down = scripted(2, "cannot tell").1;
        let ctx = Ctx::new(d.path(), "db", "spira", "inc", &down, "bin", 5);
        reconcile(100, &ctx, "p", Reading::Standing(vec![stand("a")]));
        let marker = d.join("conditions/unread/p/a");
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "cannot tell");
        let up = scripted(0, "[]").1;
        let inc = recording_incident(d.path());
        let rec = record(d.path());
        let ctx = Ctx { alarm: &rec, ..Ctx::new(d.path(), "db", "spira", &inc, &up, "bin", 5) };
        reconcile(200, &ctx, "p", Reading::Standing(vec![stand("a")]));
        assert!(!marker.exists(), "a successful read removes it");
        reconcile(300, &Ctx::new(d.path(), "db", "spira", "inc", &down, "bin", 5), "p", Reading::Standing(vec![stand("a")]));
        assert!(marker.exists());
        reconcile(400, &ctx, "p", Reading::Standing(vec![]));
        assert!(!marker.exists(), "a cleared condition removes it");
    }

    #[test_id(th49tmj)]
    #[test]
    fn six_conditions_file_five_and_the_sixth_next_pass() {
        let d = testkit::TempDir::new("wt-cond-cap");
        let lc = scripted(0, "[]").1;
        let inc = recording_incident(d.path());
        let conds: Vec<Cond> = (1..=6).map(|i| stand(&format!("c{i}"))).collect();
        let rec = record(d.path());
        let ctx = Ctx { alarm: &rec, ..Ctx::new(d.path(), "db", "spira", &inc, &lc, "bin", 5) };
        reconcile(100, &ctx, "one", Reading::Standing(conds[..3].to_vec()));
        reconcile(100, &ctx, "two", Reading::Standing(conds[3..].to_vec()));
        assert_eq!(raised(d.path()).len(), 5, "{:?}", raised(d.path()));
        assert_eq!(*ctx.deferred.borrow(), ["incident:c6"]);
        let next = Ctx { alarm: &rec, ..Ctx::new(d.path(), "db", "spira", &inc, &lc, "bin", 5) };
        reconcile(200, &next, "two", Reading::Standing(conds[3..].to_vec()));
        assert_eq!(raised(d.path()).last().map(String::as_str), Some("incident:c6"));
    }

    #[test_id(tgcwb3y)]
    #[test]
    fn a_cleared_condition_whose_read_keeps_failing_logs_its_error_once() {
        let d = testkit::TempDir::new("wt-cond-clear-unread");
        let inc = recording_incident(d.path());
        let rec = record(d.path());
        let up = scripted(0, "[]").1;
        let ctx = Ctx { alarm: &rec, ..Ctx::new(d.path(), "db", "spira", &inc, &up, "bin", 5) };
        reconcile(100, &ctx, "p", Reading::Standing(vec![stand("a")]));
        assert!(d.join("conditions/filed/p/a").exists());
        let down = scripted(2, "cannot tell").1;
        let ctx = Ctx::new(d.path(), "db", "spira", &inc, &down, "bin", 5);
        reconcile(200, &ctx, "p", Reading::Standing(vec![]));
        let unread = d.join("conditions/unread/p/a");
        assert_eq!(std::fs::read_to_string(&unread).unwrap(), "cannot tell");
        let seen = std::fs::metadata(&unread).unwrap().modified().unwrap();
        reconcile(300, &ctx, "p", Reading::Standing(vec![]));
        assert_eq!(std::fs::metadata(&unread).unwrap().modified().unwrap(), seen, "the same error is not logged, nor rewritten, again");
    }

    #[test_id(tbw2x7g)]
    #[test]
    fn a_deduped_alarm_marks_the_condition_filed_without_using_the_pass_cap() {
        let d = testkit::TempDir::new("wt-cond-deduped");
        let lc = scripted(0, "[]").1;
        let inc = recording_incident(d.path());
        let deduped = |_: &str, _: &Finding| incident::AlarmOutcome::Deduped;
        let ctx = Ctx { alarm: &deduped, ..Ctx::new(d.path(), "db", "spira", &inc, &lc, "bin", 1) };
        reconcile(100, &ctx, "p", Reading::Standing(vec![stand("a"), stand("b")]));
        assert!(d.join("conditions/filed/p/a").exists() && d.join("conditions/filed/p/b").exists());
        assert_eq!(ctx.filed.get(), 0);
        assert!(ctx.deferred.borrow().is_empty());
    }

    #[test_id(t6xw4ba)]
    #[test]
    fn an_unsustained_condition_files_nothing() {
        let d = testkit::TempDir::new("wt-cond-sustain");
        let lc = scripted(0, "[]").1;
        let inc = recording_incident(d.path());
        let ctx = Ctx::new(d.path(), "db", "spira", &inc, &lc, "bin", 5);
        let mut c = stand("a");
        c.sustain_secs = 600;
        reconcile(100, &ctx, "p", Reading::Standing(vec![c]));
        assert!(raised(d.path()).is_empty());
    }
}
