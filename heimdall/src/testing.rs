//! An in-memory `Conditions` store that fails on demand, shared by the tests that drive alerts
//! and whole passes.

use std::sync::Mutex;

use crate::conditions::{Condition, Conditions, Failure, R};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Up,
    /// Every call fails: the store is unreachable or timing out.
    Down(Failure),
    /// Reads answer, writes fail.
    WriteFails(Failure),
}

pub struct Fake {
    rows: Mutex<Vec<Condition>>,
    calls: Mutex<Vec<String>>,
    mode: Mutex<Mode>,
    messages: Mutex<Vec<(String, String)>>,
}

impl Default for Fake {
    fn default() -> Fake {
        Fake::in_mode(Mode::Up)
    }
}

impl Fake {
    pub fn in_mode(mode: Mode) -> Fake {
        Fake { rows: Mutex::default(), calls: Mutex::default(), mode: Mutex::new(mode), messages: Mutex::default() }
    }
    pub fn set_mode(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    pub fn row(&self, key: &str) -> Option<Condition> {
        self.rows.lock().unwrap().iter().find(|c| c.key == key).cloned()
    }
    pub fn status(&self, key: &str) -> Option<String> {
        self.row(key).map(|c| c.status)
    }
    pub fn count_of_kind(&self, kind: &str) -> usize {
        self.rows.lock().unwrap().iter().filter(|c| c.kind == kind).count()
    }
    /// The message of the latest successful `set` for `key`.
    pub fn message(&self, key: &str) -> Option<String> {
        self.messages.lock().unwrap().iter().rev().find(|(k, _)| k == key).map(|(_, m)| m.clone())
    }
    /// The operator clearing an alert by hand: not firing, flap count untouched.
    pub fn clear_by_hand(&self, key: &str) {
        if let Some(c) = self.rows.lock().unwrap().iter_mut().find(|c| c.key == key) {
            c.status = "False".into();
        }
    }
    fn gate(&self, read: bool) -> R<()> {
        match *self.mode.lock().unwrap() {
            Mode::Up => Ok(()),
            Mode::Down(f) => Err(f),
            Mode::WriteFails(f) if !read => Err(f),
            Mode::WriteFails(_) => Ok(()),
        }
    }
}

impl Conditions for Fake {
    fn list(&self) -> R<Vec<Condition>> {
        self.calls.lock().unwrap().push("list".into());
        self.gate(true)?;
        Ok(self.rows.lock().unwrap().clone())
    }
    fn set(&self, key: &str, kind: &str, _reason: &str, message: &str, firing: bool) -> R<()> {
        self.calls.lock().unwrap().push(format!("set {key} {kind} {firing}"));
        self.gate(false)?;
        self.messages.lock().unwrap().push((key.into(), message.into()));
        let mut rows = self.rows.lock().unwrap();
        let status = if firing { "True" } else { "False" }.to_string();
        match rows.iter_mut().find(|c| c.key == key) {
            Some(c) => {
                c.flaps += u64::from(firing && !c.firing());
                c.status = status;
            }
            None => rows.push(Condition { key: key.into(), kind: kind.into(), status, flaps: u64::from(firing) }),
        }
        Ok(())
    }
    fn clear(&self, key: &str, _reason: &str, _message: &str) -> R<()> {
        self.calls.lock().unwrap().push(format!("clear {key}"));
        self.gate(false)?;
        if let Some(c) = self.rows.lock().unwrap().iter_mut().find(|c| c.key == key) {
            c.status = "False".into();
        }
        Ok(())
    }
}
