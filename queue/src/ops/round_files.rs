//! The round files, `<queue_dir>/<repo>/round` and `round.<batch>`: the record of the open rounds
//! until the batch machine is on the control store, after which no round is read from them and
//! install removes them. The only reader of those files; nothing else names them.

use std::fs;
use std::path::{Path, PathBuf};

use super::round::{finish, worktree_of, End};
use super::{Ctx, World};
use crate::records::{self, write_atomic, Kv};

const RECORD: &str = "round";
const EXTRA_PREFIX: &str = "round.";

/// Every open round of the repo, oldest first. The oldest sits in `round`, which the tools
/// outside the queue read; any other is `round.<batch>`.
fn open_records(c: &Ctx) -> Vec<(PathBuf, Kv)> {
    let mut extras: Vec<(u64, PathBuf, Kv)> = fs::read_dir(c.queue_file(""))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(EXTRA_PREFIX))
        })
        .filter_map(|e| {
            records::read_kv(&e.path()).ok().flatten().map(|kv| {
                (
                    kv.get("opened").and_then(|v| v.parse().ok()).unwrap_or(0),
                    e.path(),
                    kv,
                )
            })
        })
        .collect();
    extras.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let primary = c.queue_file(RECORD);
    records::read_kv(&primary)
        .ok()
        .flatten()
        .map(|kv| (primary, kv))
        .into_iter()
        .chain(extras.into_iter().map(|(_, p, kv)| (p, kv)))
        .collect()
}

/// A round whose batch row is terminal has ended; its record answers no reader. A row that
/// cannot be read is not an ended round.
fn lifecycle_dead(w: &World, kv: &Kv) -> bool {
    w.lc.batch_state(kv.get("batch_id").unwrap_or(""))
        .is_some_and(|(s, _)| {
            lifecycle::batch::BatchState::from_str(&s)
                .is_some_and(lifecycle::batch::BatchState::is_terminal)
        })
}

/// The rounds the lifecycle still holds, oldest first: every reader of the round records asks
/// this, never [`open_records`]. `ended` is handed each record whose round has ended.
pub(super) fn live_records(
    w: &World,
    c: &Ctx,
    mut ended: impl FnMut(&Path, &Kv),
) -> Vec<(PathBuf, Kv)> {
    open_records(c)
        .into_iter()
        .filter(|(p, kv)| {
            let dead = lifecycle_dead(w, kv);
            if dead {
                ended(p, kv);
            }
            !dead
        })
        .collect()
}

/// For a reader that leaves the records as they are.
pub(super) fn keep(_: &Path, _: &Kv) {}

/// Closes out the rounds that ended without a verb closing them (record, worktree, running
/// marker), but `spare`'s, and hands `round` to the oldest live one. The caller holds the
/// queue lock.
pub(super) fn sweep(w: &World, c: &Ctx, spare: Option<&str>) {
    let live = live_records(w, c, |p, kv| {
        let batch = kv.get("batch_id").unwrap_or("");
        if Some(batch) == spare {
            return;
        }
        let _ = fs::remove_file(p);
        let wt = worktree_of(kv);
        if let (Some(repo), false) = (&c.r.path, wt.as_os_str().is_empty()) {
            w.git.worktree_remove(repo, &wt);
        }
        let state = w.lc.batch_state(batch).map(|(s, _)| s).unwrap_or_default();
        finish(
            w,
            c,
            batch,
            End::Abandoned("swept"),
            &format!("ended: {state} on the lifecycle; record swept"),
        );
    });
    promote(c, &live);
}

fn promote(c: &Ctx, live: &[(PathBuf, Kv)]) {
    if !c.queue_file(RECORD).exists() {
        if let Some((p, _)) = live.first() {
            let _ = fs::rename(p, c.queue_file(RECORD));
        }
    }
}

pub(super) fn record_of(w: &World, c: &Ctx, batch: &str) -> Option<(PathBuf, Kv)> {
    live_records(w, c, keep)
        .into_iter()
        .find(|(_, kv)| kv.get("batch_id") == Some(batch))
}

pub(super) fn save_record(w: &World, c: &Ctx, kv: &Kv) -> Result<(), String> {
    let batch = kv.get("batch_id").unwrap_or("");
    let path = match record_of(w, c, batch) {
        Some((p, _)) => p,
        None if c.queue_file(RECORD).exists() => c.queue_file(&format!("{EXTRA_PREFIX}{batch}")),
        None => c.queue_file(RECORD),
    };
    write_atomic(&path, &kv.render())
}

/// Removes the round's record, live or ended, with every other ended one; the oldest live
/// round left takes over `round`. The caller holds the queue lock.
pub(super) fn drop_record(w: &World, c: &Ctx, batch: &str) {
    let live: Vec<(PathBuf, Kv)> = live_records(w, c, |p, _| {
        let _ = fs::remove_file(p);
    })
    .into_iter()
    .filter(|(p, kv)| {
        let ours = kv.get("batch_id") == Some(batch);
        if ours {
            let _ = fs::remove_file(p);
        }
        !ours
    })
    .collect();
    promote(c, &live);
}

/// The record of round `batch` whose round has ended, left for the verb that finishes closing it.
pub(super) fn ended_record(w: &World, c: &Ctx, batch: &str) -> Option<Kv> {
    let mut found = None;
    let _ = live_records(w, c, |_, kv| {
        if kv.get("batch_id") == Some(batch) {
            found = Some(kv.clone());
        }
    });
    found
}
