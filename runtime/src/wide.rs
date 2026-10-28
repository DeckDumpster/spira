//! Wide events: one open key/value record per unit of work. Nothing lists the keys; a field
//! exists because some code on the path set it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// An OTLP attribute value: a scalar or an array of one scalar type.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Strs(Vec<String>),
    Ints(Vec<i64>),
    Floats(Vec<f64>),
    Bools(Vec<bool>),
}

macro_rules! from {
    ($($t:ty => |$v:ident| $e:expr),* $(,)?) => { $( impl From<$t> for Value { fn from($v: $t) -> Value { $e } } )* };
}

from! {
    &str => |v| Value::Str(v.into()),
    String => |v| Value::Str(v),
    i64 => |v| Value::Int(v),
    i32 => |v| Value::Int(v.into()),
    u32 => |v| Value::Int(v.into()),
    u64 => |v| Value::Int(i64::try_from(v).unwrap_or(i64::MAX)),
    usize => |v| Value::Int(i64::try_from(v).unwrap_or(i64::MAX)),
    f64 => |v| Value::Float(v),
    bool => |v| Value::Bool(v),
    Vec<String> => |v| Value::Strs(v),
    Vec<i64> => |v| Value::Ints(v),
    Vec<f64> => |v| Value::Floats(v),
    Vec<bool> => |v| Value::Bools(v),
}

pub type Fields = BTreeMap<String, Value>;

pub const REJECTED_KEYS: &str = "wide.rejected_keys";

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.split('.').all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
}

/// The record being built. Write-only to everything but the executor that opened it, so an
/// annotation can never change behaviour.
#[derive(Clone, Default)]
pub struct Wide(Arc<Mutex<Fields>>);

impl Wide {
    pub fn set(&self, key: &str, value: impl Into<Value>) {
        let mut f = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if valid_key(key) {
            f.insert(key.into(), value.into());
        } else if let Value::Strs(v) = f.entry(REJECTED_KEYS.into()).or_insert_with(|| Value::Strs(vec![])) {
            v.push(key.into());
        }
    }

    pub(crate) fn take(&self) -> Fields {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Receives each finished record, once. A sink never fails its caller.
pub trait WideSink: Send + Sync {
    fn emit(&self, fields: &Fields);
}

pub struct NullSink;

impl WideSink for NullSink {
    fn emit(&self, _: &Fields) {}
}

#[derive(Default)]
pub struct MemSink(Mutex<Vec<Fields>>);

impl MemSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn records(&self) -> Vec<Fields> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl WideSink for MemSink {
    fn emit(&self, fields: &Fields) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(fields.clone());
    }
}

/// Resident set size of this process in KiB; 0 where `/proc` is absent.
pub fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_outside_the_dotted_lowercase_form_is_recorded_not_stored() {
        let w = Wide::default();
        w.set("effect.attempt", 2u32);
        w.set("Bad Key", "x");
        let f = w.take();
        assert_eq!(f.get("effect.attempt"), Some(&Value::Int(2)));
        assert!(!f.contains_key("Bad Key"));
        assert_eq!(f.get(REJECTED_KEYS), Some(&Value::Strs(vec!["Bad Key".into()])));
    }
}
