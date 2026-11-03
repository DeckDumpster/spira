//! The open batch of a queue.forge repository. Until the batch machine is on the control store it
//! is the file `<queue_dir>/<repo>/open`; once it is, it is the batch's own record,
//! `batches/<repo>/<id>`, whose `forge.phase` field says how far `batch open` got: `opening` (the
//! record is written and its PR may not be), `open` (the PR is recorded) and `armed` (auto-merge is
//! armed). Every reader and writer of the open batch goes through here.

use std::path::PathBuf;

use super::{Ctx, World};
use crate::batches::BatchRecord;
use crate::records::{self, write_atomic, Kv};

/// The record's field naming how far `batch open` got.
pub const PHASE: &str = "forge.phase";
pub const OPENING: &str = "opening";
pub const OPEN: &str = "open";
pub const ARMED: &str = "armed";

const FILE: &str = "open";
/// Where the verdict leaves its last look at the forge while the open batch is the file.
pub const FORGE_STATE: &str = "forge-state";
/// The prefix of the archive a batch closed while it was the file is moved to.
const CLOSED: &str = "closed-pr";
/// Why the queue ended a batch the machine has not, as the round records name it.
const ENDED: &str = "ended";

/// Where the open batch's record lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum At {
    File(PathBuf),
    /// The batch id of its `batches/<repo>/<id>` record.
    Store(String),
}

/// The open batch: its record as the queue reads it, and where that record is kept.
#[derive(Debug, Clone)]
pub struct Open {
    pub kv: Kv,
    pub at: At,
}

/// Whether the open batch is a record on the control store rather than the `open` file.
pub fn on_store(w: &World) -> bool {
    w.lc.rounds_door().is_some()
}

/// The repo's forge batch records that answer as open: written by `batch open`, not ended by the
/// machine, and not ended by the queue; oldest first. Err: the store could not say.
pub fn live(w: &World, c: &Ctx) -> Result<Vec<BatchRecord>, String> {
    Ok(w.lc
        .open_batches(&c.r.name)?
        .into_iter()
        .filter(|b| b.kv.get(PHASE).is_some() && b.kv.get(ENDED).is_none())
        .collect())
}

/// The open batch, whatever its phase; `Ok(None)` when there is none. Err: cannot tell.
///
/// On the store, an `open` file still present is a batch the queue opened before the move. It is
/// the open batch until it is settled, which removes the file; install refuses to finish while
/// one names a batch the machine has not ended, so no record outlives the drain.
pub fn read(w: &World, c: &Ctx) -> Result<Option<Open>, String> {
    let f = c.queue_file(FILE);
    if let Some(kv) = records::read_kv(&f)? {
        return Ok(Some(Open { kv, at: At::File(f) }));
    }
    if !on_store(w) {
        return Ok(None);
    }
    Ok(live(w, c)?.into_iter().next().map(|b| Open {
        kv: b.kv,
        at: At::Store(b.id),
    }))
}

/// [`read`], where a record that cannot be read is no open batch: for the readers that only
/// report on one (a status line, a refusal's list), never for one that acts on its absence.
pub fn read_or_none(w: &World, c: &Ctx) -> Option<Open> {
    read(w, c).ok().flatten()
}

/// The open batch as a reader outside the queue sees it (`batch show`): the record, carrying the
/// verdict's last look at the forge as `forge.<field>` lines. On the store those are the record's
/// own; while the batch is the file they come from [`FORGE_STATE`], when it names this batch.
pub fn shown(c: &Ctx, open: &Open) -> Result<Kv, String> {
    let mut kv = open.kv.clone();
    if let At::File(_) = open.at {
        let Some(state) = records::read_kv(&c.queue_file(FORGE_STATE))? else {
            return Ok(kv);
        };
        if state.get("batch_id").is_none() || state.get("batch_id") != kv.get("batch_id") {
            return Ok(kv);
        }
        for (k, v) in state.lines.iter().filter(|(k, _)| k != "batch_id" && k != "pr") {
            kv.push(&format!("forge.{k}"), v);
        }
    }
    Ok(kv)
}

/// The repo's newest `n` ended forge batches, newest first by `opened`: on the store, the records
/// the machine or the queue ended; and the `closed-pr*` archives a batch closed before the move
/// left in the repo's queue directory, until install removes them.
pub fn ended(w: &World, c: &Ctx, n: usize) -> Result<Vec<Kv>, String> {
    let mut out: Vec<Kv> = Vec::new();
    if on_store(w) {
        out.extend(
            w.lc.batches(&c.r.name)?
                .into_iter()
                .filter(|b| b.kv.get(PHASE).is_some() && (b.is_terminal() || b.kv.get(ENDED).is_some()))
                .map(|b| with_id(&b.kv, &b.id)),
        );
    }
    let dir = c.queue_file("");
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with(CLOSED) {
                    if let Some(kv) = records::read_kv(&e.path())? {
                        out.push(kv);
                    }
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    }
    let opened = |kv: &Kv| kv.get("opened").and_then(|o| o.parse::<i64>().ok()).unwrap_or(0);
    out.sort_by_key(|kv| std::cmp::Reverse(opened(kv)));
    out.truncate(n);
    Ok(out)
}

impl Open {
    /// `opening`: the record is written and `batch open` has not yet recorded its PR.
    pub fn is_opening(&self) -> bool {
        self.kv.get(PHASE) == Some(OPENING)
    }

    /// Where the record is, for a message.
    pub fn place(&self) -> String {
        match &self.at {
            At::File(f) => f.display().to_string(),
            At::Store(id) => format!("batch record {id}"),
        }
    }

    /// The record as it stands now.
    pub fn reread(&self, w: &World, c: &Ctx) -> Result<Kv, String> {
        match &self.at {
            At::File(f) => Ok(records::read_kv(f)?.unwrap_or_default()),
            At::Store(id) => w
                .lc
                .batch(&c.r.name, id)?
                .map(|b| b.kv)
                .ok_or_else(|| format!("the control store holds no batch {id} for {}", c.r.name)),
        }
    }

    /// Writes `kv` as the whole record.
    pub fn save(&self, w: &World, c: &Ctx, kv: &Kv) -> Result<(), String> {
        match &self.at {
            At::File(f) => write_atomic(f, &kv.render()),
            At::Store(id) => w.lc.save_batch(&c.r.name, &with_id(kv, id)),
        }
    }

    /// Re-reads the record, drops the fields `drop` and appends `add`.
    pub fn rewrite(&self, w: &World, c: &Ctx, drop: &[&str], add: &[(&str, &str)]) -> Result<(), String> {
        let mut kv = self.reread(w, c)?;
        for k in drop {
            kv.remove(k);
        }
        for (k, v) in add {
            kv.push(k, v);
        }
        self.save(w, c, &kv)
    }

    /// The batch answers no reader from here on, for `cause`. The file is removed; on the store
    /// the machine's end already says so, and when the machine has not ended it the record's
    /// `ended` field does.
    pub fn remove(&self, w: &World, c: &Ctx, cause: &str) -> Result<(), String> {
        match &self.at {
            At::File(f) => match std::fs::remove_file(f) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", f.display())),
                _ => Ok(()),
            },
            At::Store(id) => {
                let Some(b) = w.lc.batch(&c.r.name, id)? else {
                    return Ok(());
                };
                if b.is_terminal() || b.kv.get(ENDED).is_some() {
                    return Ok(());
                }
                let mut kv = b.kv;
                kv.push(ENDED, cause);
                kv.push("ended_at", &w.clock.now().to_string());
                w.lc.save_batch(&c.r.name, &kv)
            }
        }
    }

    /// Closes the batch with `archived` as its last record: the file is moved to `archive` (in the
    /// repo's queue directory); on the store the record keeps `archived`'s fields and ends.
    pub fn archive(&self, w: &World, c: &Ctx, archived: &Kv, archive: &str, cause: &str) -> Result<(), String> {
        match &self.at {
            At::File(f) => {
                write_atomic(f, &archived.render())?;
                std::fs::rename(f, c.queue_file(archive)).map_err(|e| format!("{}: {e}", f.display()))
            }
            At::Store(_) => {
                self.save(w, c, archived)?;
                self.remove(w, c, cause)
            }
        }
    }
}

/// `kv` naming batch `id`, as a store write needs it to.
fn with_id(kv: &Kv, id: &str) -> Kv {
    let mut out = kv.clone();
    out.remove("batch_id");
    out.lines.insert(0, ("batch_id".into(), id.into()));
    out
}
