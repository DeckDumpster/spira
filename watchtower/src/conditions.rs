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
use std::process::Command;

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
    /// The sanctioned bd wrapper, which reads the store from the config.
    pub bdq: &'a str,
    /// The lifecycle machine (`spira-lc`): an incident bead is a work bead, so whether it is
    /// still unfinished is its lifecycle row's answer, never bd's status (sp-jgjvh).
    pub lc_bin: &'a str,
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

/// Ids of the unfinished beads holding `reference`; `None` when the store or the lifecycle
/// machine did not answer. bd supplies only which beads carry the reference (content); whether
/// each is unfinished is its lifecycle row's (sp-jgjvh: incident beads are work beads).
fn open_beads(ctx: &Ctx, reference: &str) -> Option<Vec<String>> {
    let out = crate::deadline::output(
        "beads by ref",
        Command::new(ctx.bdq).args(["list", "--external-ref", reference, "--all", "--json", "--limit", "0", "--brief"]),
    )
    .ok()
    .filter(|o| o.status.success())?;
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).ok()?;
    let ids = rows.iter().filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(String::from)).collect();
    unfinished(ctx.lc_bin, ids)
}

/// The ids whose lifecycle row still owes builder work (READY, WORKING, REWORK) — what bd's
/// open/in_progress meant for an incident. A bead the machine has no row for is not live work
/// (it can never be claimed; CHECK-ROWLESS reports it). `None` when the machine did not
/// answer: a control that cannot check must not read as "nothing holds it".
pub(crate) fn unfinished(lc_bin: &str, ids: Vec<String>) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for id in ids {
        match spira_config::lc_state::row_with(lc_bin, &id) {
            Ok(Some(r)) if !r.past_builder() => out.push(id),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    Some(out)
}

// Through the lifecycle machine (sp-3fue0j), never a raw bd close.
fn close_bead(ctx: &Ctx, id: &str, why: &str) -> bool {
    spira_config::lifecycle_row::close_with(ctx.lc_bin, id, &format!("OUTCOME: delivered\n{why}\n"), "watchtower", None).is_ok()
}

pub fn reconcile(now: i64, ctx: &Ctx, probe: &str, reading: Reading) {
    let Reading::Standing(conds) = reading else {
        log(&format!("watchtower: conditions: {probe} could not read — nothing filed or cleared"));
        return;
    };
    let filed_dir = dir(ctx, "filed", probe);
    let pending_dir = dir(ctx, "pending", probe);
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
        match open_beads(ctx, &c.reference) {
            Some(ids) if ids.is_empty() => {
                if !incident::is_usable(ctx.incident_sh) {
                    log(&format!("watchtower: conditions: {} is missing — {} not filed", ctx.incident_sh, c.key));
                    continue;
                }
                let f = Finding::new(ctx.db, ctx.home_repo, &c.title, &c.body).priority(c.priority).reference(&c.reference).cause(probe);
                if incident::alarm(ctx.incident_sh, &f) {
                    let _ = std::fs::write(filed_dir.join(&k), &c.reference);
                    log(&format!("watchtower: conditions: filed {} ({probe})", c.reference));
                }
            }
            _ => {}
        }
    }

    for k in names(&pending_dir).difference(&standing) {
        let _ = std::fs::remove_file(pending_dir.join(k));
    }
    for k in names(&filed_dir).difference(&standing) {
        let marker = filed_dir.join(k);
        let Ok(reference) = std::fs::read_to_string(&marker) else { continue };
        let Some(ids) = open_beads(ctx, reference.trim()) else { continue };
        let all_closed = ids.iter().all(|id| close_bead(ctx, id, &format!("The condition {} no longer holds; watchtower resolved this incident.", reference.trim())));
        if all_closed {
            let _ = std::fs::remove_file(&marker);
            log(&format!("watchtower: conditions: cleared {} ({probe})", reference.trim()));
        }
    }
}
