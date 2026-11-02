//! The export of the dolt lifecycle store: every table, as one JSONL file. Each line is one row
//! with its own checksum, and the last line is the manifest of every table's row count and
//! checksum, so a truncated file has no manifest and a changed row names itself.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::db::Conn;

pub const FORMAT: &str = "spira-lc-archive/1";

/// One table of the export: the columns that name a row, and the column a large table is paged
/// on, so no single query outlives the connection's deadline.
pub struct Table {
    pub name: &'static str,
    pub key: &'static [&'static str],
    pub seq: bool,
}

pub const TABLES: &[Table] = &[
    Table { name: "bead", key: &["bead_id"], seq: false },
    Table { name: "delivery", key: &["bead_id"], seq: false },
    Table { name: "batch", key: &["batch_id"], seq: false },
    Table { name: "batch_member", key: &["batch_id", "bead_id"], seq: false },
    Table { name: "ask", key: &["ask_id"], seq: false },
    Table { name: "mending", key: &["batch_id", "pass", "suite"], seq: false },
    Table { name: "mend", key: &["seq"], seq: true },
    Table { name: "event", key: &["seq"], seq: true },
    Table { name: "runtime_event", key: &["seq"], seq: true },
    Table { name: "runtime_applied", key: &["handler", "event_id"], seq: false },
    Table { name: "runtime_cursor", key: &["id"], seq: false },
    Table { name: "runtime_schedule", key: &["row_id"], seq: false },
    Table { name: "runtime_poison", key: &["stream"], seq: false },
    Table { name: "bead_dep", key: &["bead_id", "depends_on"], seq: false },
];

const PAGE: usize = 5_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sum {
    pub rows: usize,
    pub sha256: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub tables: BTreeMap<String, Sum>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub row: Value,
}

/// The rows of every table, in the order they were exported, and the manifest they matched.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Archive {
    pub tables: BTreeMap<String, Vec<Row>>,
    pub manifest: Manifest,
}

impl Archive {
    pub fn rows(&self, table: &str) -> &[Row] {
        self.tables.get(table).map_or(&[], Vec::as_slice)
    }
}

pub fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn row_sha(row: &Value) -> String {
    hex(&serde_json::to_vec(row).unwrap_or_default())
}

/// What names a row in a failure: its key columns, joined by `/`.
pub fn row_key(t: &Table, row: &Value) -> String {
    t.key
        .iter()
        .map(|c| match row.get(*c) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(v) => v.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// A table's checksum: over its rows' checksums, in archive order.
pub fn table_sum<'a>(shas: impl IntoIterator<Item = &'a str>) -> Sum {
    let mut h = Sha256::new();
    let mut rows = 0;
    for s in shas {
        h.update(s.as_bytes());
        h.update(b"\n");
        rows += 1;
    }
    Sum {
        rows,
        sha256: h.finalize().iter().map(|b| format!("{b:02x}")).collect(),
    }
}

fn line(table: &str, key: &str, row: &Value) -> Value {
    json!({ "table": table, "key": key, "sha256": row_sha(row), "row": row })
}

fn select(conn: &Conn, t: &Table) -> Result<Vec<Value>, String> {
    let q = |sql: String| conn.query(&sql).map_err(|e| format!("{}: {e:?}", t.name));
    if !t.seq {
        return q(format!("SELECT * FROM {} ORDER BY {}", t.name, t.key.join(", ")));
    }
    let mut out = Vec::new();
    let mut after = -1_i64;
    loop {
        let page = q(format!(
            "SELECT * FROM {} WHERE seq > {after} ORDER BY seq LIMIT {PAGE}",
            t.name
        ))?;
        let n = page.len();
        if let Some(last) = page.last() {
            after = crate::rows::number(last, "seq")
                .ok_or_else(|| format!("{}: a row without a seq: {last}", t.name))?;
        }
        out.extend(page);
        if n < PAGE {
            return Ok(out);
        }
    }
}

/// Reads every table over `conn` and writes it to `path`, which must not exist yet.
pub fn export(conn: &Conn, path: &Path) -> Result<Manifest, String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut w = BufWriter::new(file);
    let mut manifest = Manifest {
        format: FORMAT.into(),
        tables: BTreeMap::new(),
    };
    let io = |e: std::io::Error| format!("{}: {e}", path.display());
    for t in TABLES {
        let rows = select(conn, t)?;
        let mut shas = Vec::with_capacity(rows.len());
        for row in &rows {
            let l = line(t.name, &row_key(t, row), row);
            shas.push(l["sha256"].as_str().unwrap_or_default().to_string());
            serde_json::to_writer(&mut w, &l).map_err(|e| e.to_string())?;
            w.write_all(b"\n").map_err(io)?;
        }
        manifest
            .tables
            .insert(t.name.into(), table_sum(shas.iter().map(String::as_str)));
    }
    serde_json::to_writer(&mut w, &json!({ "manifest": manifest })).map_err(|e| e.to_string())?;
    w.write_all(b"\n").map_err(io)?;
    let file = w.into_inner().map_err(|e| io(e.into_error()))?;
    file.sync_all().map_err(io)?;
    Ok(manifest)
}

/// The archive at `path`, or every way it fails its own checksums: a row that no longer
/// matches its checksum, by table and key; a table whose count or checksum is not the
/// manifest's; a table the manifest does not list.
pub fn read(path: &Path) -> Result<Archive, Vec<String>> {
    let file = std::fs::File::open(path).map_err(|e| vec![format!("{}: {e}", path.display())])?;
    parse(BufReader::new(file))
}

pub fn parse(r: impl BufRead) -> Result<Archive, Vec<String>> {
    let mut failures = Vec::new();
    let mut tables: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    let mut shas: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut manifest = None;
    for (n, l) in r.lines().enumerate() {
        let l = l.map_err(|e| vec![format!("archive line {}: {e}", n + 1)])?;
        if manifest.is_some() {
            failures.push(format!("archive line {}: a line after the manifest", n + 1));
            continue;
        }
        let v: Value = match serde_json::from_str(&l) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("archive line {}: not JSON: {e}", n + 1));
                continue;
            }
        };
        if let Some(m) = v.get("manifest") {
            match serde_json::from_value::<Manifest>(m.clone()) {
                Ok(m) => manifest = Some(m),
                Err(e) => failures.push(format!("archive manifest: {e}")),
            }
            continue;
        }
        let (Some(table), Some(key), Some(sha), Some(row)) = (
            v["table"].as_str(),
            v["key"].as_str(),
            v["sha256"].as_str(),
            v.get("row"),
        ) else {
            failures.push(format!("archive line {}: not a row", n + 1));
            continue;
        };
        if row_sha(row) != sha {
            failures.push(format!("archive: {table} row {key} fails its checksum"));
        }
        shas.entry(table.into()).or_default().push(sha.into());
        tables.entry(table.into()).or_default().push(Row {
            key: key.into(),
            row: row.clone(),
        });
    }
    let Some(manifest) = manifest else {
        failures.push("archive: no manifest: the export did not finish".into());
        return Err(failures);
    };
    if manifest.format != FORMAT {
        failures.push(format!("archive: format {:?}, not {FORMAT}", manifest.format));
    }
    for t in TABLES {
        let Some(want) = manifest.tables.get(t.name) else {
            failures.push(format!("archive: the manifest lists no {} table", t.name));
            continue;
        };
        let got = table_sum(shas.get(t.name).into_iter().flatten().map(String::as_str));
        if got.rows != want.rows {
            failures.push(format!(
                "archive: {} has {} rows, the manifest says {}",
                t.name, got.rows, want.rows
            ));
        } else if got.sha256 != want.sha256 {
            failures.push(format!("archive: {} fails its manifest checksum", t.name));
        }
    }
    for name in tables.keys().filter(|n| !TABLES.iter().any(|t| t.name == *n)) {
        failures.push(format!("archive: rows of {name}, a table the export does not know"));
    }
    if failures.is_empty() {
        Ok(Archive { tables, manifest })
    } else {
        Err(failures)
    }
}

/// `rows` as an archive's lines, manifest last: what `export` writes for the same rows.
#[cfg(test)]
pub fn render(rows: &BTreeMap<&str, Vec<Value>>) -> String {
    let mut out = String::new();
    let mut manifest = Manifest {
        format: FORMAT.into(),
        tables: BTreeMap::new(),
    };
    for t in TABLES {
        let mut shas = Vec::new();
        for row in rows.get(t.name).into_iter().flatten() {
            let l = line(t.name, &row_key(t, row), row);
            shas.push(l["sha256"].as_str().unwrap_or_default().to_string());
            out.push_str(&l.to_string());
            out.push('\n');
        }
        manifest
            .tables
            .insert(t.name.into(), table_sum(shas.iter().map(String::as_str)));
    }
    out.push_str(&json!({ "manifest": manifest }).to_string());
    out.push('\n');
    out
}
