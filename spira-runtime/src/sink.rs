//! Wide events as OTLP log records through the collector's file receiver. Nothing here names a field.

use runtime::{Fields, Value, WideSink};
use spira_otel::logs::{Attr, LogSink};

pub struct OtlpSink(pub LogSink);

fn attr(v: &Value) -> Attr {
    match v {
        Value::Str(s) => Attr::Str(s.clone()),
        Value::Int(i) => Attr::Int(*i),
        Value::Float(f) => Attr::Float(*f),
        Value::Bool(b) => Attr::Bool(*b),
        Value::Strs(v) => Attr::Strs(v.clone()),
        Value::Ints(v) => Attr::Ints(v.clone()),
        Value::Floats(v) => Attr::Floats(v.clone()),
        Value::Bools(v) => Attr::Bools(v.clone()),
    }
}

impl WideSink for OtlpSink {
    fn emit(&self, fields: &Fields) {
        let unit = match fields.get("unit") {
            Some(Value::Str(u)) => u.as_str(),
            _ => "unit",
        };
        let attrs: Vec<(String, Attr)> = fields.iter().map(|(k, v)| (k.clone(), attr(v))).collect();
        self.0.emit(unit, &attrs);
    }
}
