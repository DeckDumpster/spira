//! state-store-sqlite — the contract's Backend on SQLite. Not yet implemented.

use std::path::Path;
use std::sync::Arc;

use runtime::ports::Clock;
use state_store::{Backend, LogEntry, Result, Version, Write};

pub struct Options {
    pub max_read_ms: u64,
    pub readers: usize,
    pub clock: Arc<dyn Clock>,
    pub read_probe: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_read_ms: 1_000,
            readers: 4,
            clock: Arc::new(state_store::MonotonicClock::new()),
            read_probe: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub busy: bool,
    pub wal_frames: i64,
    pub checkpointed: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupStep {
    Copying { remaining: i32, pages: i32 },
    Done { rev: u64 },
}

#[derive(Clone)]
pub struct SqliteBackend;

impl SqliteBackend {
    pub fn open(_path: &Path, _opts: Options) -> Result<Self> {
        todo!()
    }
    pub fn pragmas(&self) -> Result<(String, i64)> {
        todo!()
    }
    pub fn checkpoint(&self) -> Result<Checkpoint> {
        todo!()
    }
    pub fn bulk_load(
        &self,
        _rev: u64,
        _items: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<()> {
        todo!()
    }
    pub fn backup_step(&self, _dest: &Path, _pages: i32) -> Result<BackupStep> {
        todo!()
    }
    pub fn set_compaction_floor(&self, _rev: u64) -> Result<()> {
        todo!()
    }
    pub fn compacted(&self) -> Result<u64> {
        todo!()
    }
}

impl Backend for SqliteBackend {
    fn head(&self) -> Result<u64> {
        todo!()
    }
    fn get(&self, _key: &str, _at: u64) -> Result<Option<Version>> {
        todo!()
    }
    fn range(
        &self,
        _p: &str,
        _a: Option<&str>,
        _l: usize,
        _at: u64,
    ) -> Result<Vec<(String, Version)>> {
        todo!()
    }
    fn commit(&self, _expect: u64, _batch: &[Write]) -> Result<u64> {
        todo!()
    }
    fn log(&self, _from: u64, _max: usize) -> Result<Vec<LogEntry>> {
        todo!()
    }
    fn compact(&self, _rev: u64) -> Result<()> {
        todo!()
    }
}
