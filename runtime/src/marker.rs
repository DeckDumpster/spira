//! The in-flight marker: the handler call in progress, written before the call and cleared
//! after it. A marker found at start is a call that never returned.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::event::{EventId, StreamKey};
use crate::{Result, RuntimeError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub event: EventId,
    pub handler: String,
    pub stream: StreamKey,
    /// Earlier restarts that happened inside this same call.
    pub stalls: u32,
}

pub trait Marker: Send + Sync {
    fn set(&self, m: &Mark) -> Result<()>;
    fn clear(&self) -> Result<()>;
    /// The marker left by a previous run, removed.
    fn take(&self) -> Result<Option<Mark>>;
}

pub struct NullMarker;

impl Marker for NullMarker {
    fn set(&self, _: &Mark) -> Result<()> {
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        Ok(())
    }
    fn take(&self) -> Result<Option<Mark>> {
        Ok(None)
    }
}

#[derive(Default)]
pub struct MemMarker(Mutex<Option<Mark>>);

impl Marker for MemMarker {
    fn set(&self, m: &Mark) -> Result<()> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(m.clone());
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
    fn take(&self) -> Result<Option<Mark>> {
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).take())
    }
}

/// One file, replaced atomically; it outlives the process, which is the point.
pub struct FileMarker(pub PathBuf);

fn io(e: std::io::Error) -> RuntimeError {
    RuntimeError::new(format!("marker: {e}"))
}

impl Marker for FileMarker {
    fn set(&self, m: &Mark) -> Result<()> {
        let tmp = self.0.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(m).map_err(|e| RuntimeError::new(e.to_string()))?).map_err(io)?;
        std::fs::rename(&tmp, &self.0).map_err(io)
    }

    fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.0) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(e)),
            _ => Ok(()),
        }
    }

    fn take(&self) -> Result<Option<Mark>> {
        let bytes = match std::fs::read(&self.0) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
        };
        self.clear()?;
        Ok(serde_json::from_slice(&bytes).ok())
    }
}
