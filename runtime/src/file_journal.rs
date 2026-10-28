//! A journal in a directory, shared by processes: every operation runs under an exclusive
//! file lock on one state file, replaced atomically. Test-only: it rewrites the whole state
//! per write and exists so two runtime processes can share one journal and one lease table.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventId, StreamKey};
use crate::journal::{fence, grant, release, Journal, Lease, LeaseTable, Offset, Poison, ScheduleRow};
use crate::{Result, RuntimeError};

#[derive(Default, Serialize, Deserialize)]
struct State {
    events: Vec<Event>,
    applied: BTreeSet<(String, String)>,
    cursor: Offset,
    schedules: BTreeMap<String, (ScheduleRow, bool)>,
    poison: BTreeMap<String, Poison>,
    leases: BTreeMap<String, Lease>,
}

impl State {
    fn append(&mut self, ev: &Event) -> Offset {
        if let Some(i) = self.events.iter().position(|e| e.id == ev.id) {
            return i as Offset;
        }
        self.events.push(ev.clone());
        (self.events.len() - 1) as Offset
    }
}

pub struct FileJournal {
    dir: PathBuf,
}

fn io(what: &str, e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::new(format!("file journal {what}: {e}"))
}

impl FileJournal {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|e| io("mkdir", e))?;
        Ok(FileJournal { dir })
    }

    fn with<T>(&self, write: bool, f: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(self.dir.join("lock")).map_err(|e| io("lock", e))?;
        lock.lock().map_err(|e| io("lock", e))?;
        let path = self.dir.join("state.json");
        let mut st: State = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| io("parse", e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(io("read", e)),
        };
        let out = f(&mut st)?;
        if write {
            let tmp = self.dir.join("state.json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&st).map_err(|e| io("encode", e))?).map_err(|e| io("write", e))?;
            std::fs::rename(&tmp, &path).map_err(|e| io("rename", e))?;
        }
        Ok(out)
    }
}

impl Journal for FileJournal {
    fn append(&self, ev: &Event) -> Result<Offset> {
        self.with(true, |s| Ok(s.append(ev)))
    }

    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        self.with(false, |s| Ok(s.events.iter().enumerate().skip(from as usize).take(limit).map(|(i, e)| (i as Offset, e.clone())).collect()))
    }

    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        self.with(false, |s| Ok(s.applied.contains(&(handler.to_string(), id.0.clone()))))
    }

    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        self.with(true, |s| {
            s.applied.insert((handler.to_string(), id.0.clone()));
            Ok(())
        })
    }

    fn cursor(&self) -> Result<Offset> {
        self.with(false, |s| Ok(s.cursor))
    }

    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.with(true, |s| {
            s.cursor = next;
            Ok(())
        })
    }

    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        self.with(true, |s| {
            s.schedules.entry(row.id.clone()).or_insert((row.clone(), false));
            Ok(())
        })
    }

    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        self.with(false, |s| Ok(s.schedules.values().filter(|(_, fired)| !fired).map(|(r, _)| r.clone()).collect()))
    }

    fn mark_fired(&self, row_id: &str) -> Result<()> {
        self.with(true, |s| {
            if let Some(r) = s.schedules.get_mut(row_id) {
                r.1 = true;
            }
            Ok(())
        })
    }

    fn poison(&self, p: &Poison) -> Result<()> {
        self.with(true, |s| {
            s.poison.entry(p.stream.0.clone()).or_insert_with(|| p.clone());
            Ok(())
        })
    }

    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.with(false, |s| Ok(s.poison.get(&stream.0).cloned()))
    }

    fn leases(&self) -> Option<&dyn LeaseTable> {
        Some(self)
    }
}

impl LeaseTable for FileJournal {
    fn acquire(&self, stream: &StreamKey, holder: &str, ttl_ms: u64, now: u64) -> Result<Option<Lease>> {
        self.with(true, |s| {
            let g = grant(s.leases.get(&stream.0), stream, holder, ttl_ms, now);
            if let Some(l) = &g {
                s.leases.insert(stream.0.clone(), l.clone());
            }
            Ok(g)
        })
    }

    fn release(&self, stream: &StreamKey, holder: &str) -> Result<()> {
        self.with(true, |s| {
            if let Some(l) = release(s.leases.get(&stream.0), holder) {
                s.leases.insert(stream.0.clone(), l);
            }
            Ok(())
        })
    }

    fn current(&self, stream: &StreamKey) -> Result<Option<Lease>> {
        self.with(false, |s| Ok(s.leases.get(&stream.0).cloned()))
    }

    fn append_fenced(&self, ev: &Event, holder: &str, epoch: u64) -> Result<Offset> {
        self.with(true, |s| {
            fence(s.leases.get(&ev.stream.0), &ev.stream, holder, epoch)?;
            Ok(s.append(ev))
        })
    }
}
