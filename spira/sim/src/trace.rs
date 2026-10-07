use crate::world::run;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const QUERY_DEADLINE: Duration = Duration::from_secs(60); // interactive: loading and querying a trace of a few thousand rows
const INVARIANTS: &str = include_str!("../invariants.sql");
const BEAD_STATE_FIELDS: &[&str] = &["bead", "bd_status", "lc_state", "lc_version", "holds", "landstate", "lc_tip", "branch_tip", "on_local_main", "on_origin_main"];

pub fn duckdb_bin() -> String {
    std::env::var("SPIRA_SIM_DUCKDB").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "duckdb".to_string())
}

pub struct Event {
    pub seq: u64,
    pub vtime: u64,
    pub kind: &'static str,
    pub actor: Option<String>,
    pub started: Option<u64>,
    pub completed: Option<u64>,
    pub exit: Option<i32>,
    pub coalesced: bool,
    pub seed: u64,
}

impl Event {
    pub fn to_json(&self) -> Value {
        json!({"seq": self.seq, "vtime": self.vtime, "kind": self.kind, "actor": self.actor, "started": self.started,
               "completed": self.completed, "exit": self.exit, "coalesced": self.coalesced, "seed": self.seed})
    }
}

#[derive(Default)]
pub struct Snapshot {
    pub beads: Vec<Value>,
    pub refs: Vec<(String, String)>,
    pub tags: Vec<String>,
}

/// Rejects a probe row that names no bead or an unknown field, so a misspelt column cannot read as an absent one.
pub fn bead_row(v: &Value) -> Result<Value, String> {
    let o = v.as_object().ok_or_else(|| format!("probe row is not an object: {v}"))?;
    if let Some(k) = o.keys().find(|k| !BEAD_STATE_FIELDS.contains(&k.as_str())) {
        return Err(format!("probe row has unknown field {k:?}: {v}"));
    }
    if !o.get("bead").is_some_and(Value::is_string) {
        return Err(format!("probe row has no bead: {v}"));
    }
    let mut out = Map::new();
    for f in BEAD_STATE_FIELDS {
        out.insert((*f).to_string(), o.get(*f).cloned().unwrap_or(Value::Null));
    }
    Ok(Value::Object(out))
}

pub struct Trace {
    dir: PathBuf,
}

impl Trace {
    pub fn dir_of(world: &Path) -> PathBuf {
        world.join("trace")
    }

    pub fn db_of(world: &Path) -> PathBuf {
        world.join("trace.duckdb")
    }

    pub fn create(world: &Path) -> Result<Self, String> {
        let dir = Self::dir_of(world);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for t in TABLES {
            std::fs::write(dir.join(format!("{t}.jsonl")), "").map_err(|e| e.to_string())?;
        }
        Ok(Trace { dir })
    }

    pub fn open(world: &Path) -> Result<Self, String> {
        let dir = Self::dir_of(world);
        if !dir.join("events.jsonl").is_file() {
            return Err(format!("{} has no trace", world.display()));
        }
        Ok(Trace { dir })
    }

    fn append(&self, table: &str, rows: &[Value]) -> Result<(), String> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut f = std::fs::OpenOptions::new().append(true).open(self.dir.join(format!("{table}.jsonl"))).map_err(|e| e.to_string())?;
        let mut buf = String::new();
        for r in rows {
            buf.push_str(&r.to_string());
            buf.push('\n');
        }
        f.write_all(buf.as_bytes()).map_err(|e| e.to_string())
    }

    pub fn event(&self, e: &Event) -> Result<(), String> {
        self.append("events", &[e.to_json()])
    }

    pub fn snapshot(&self, seq: u64, s: &Snapshot) -> Result<(), String> {
        let with_seq = |mut v: Value| {
            v.as_object_mut().map(|o| o.insert("seq".into(), json!(seq)));
            v
        };
        self.append("bead_state", &s.beads.iter().cloned().map(with_seq).collect::<Vec<_>>())?;
        self.append("refs", &s.refs.iter().map(|(r, sha)| json!({"seq": seq, "ref": r, "sha": sha})).collect::<Vec<_>>())?;
        self.append("tags", &s.tags.iter().map(|t| json!({"seq": seq, "tag": t})).collect::<Vec<_>>())
    }

    pub fn events(&self) -> Result<Vec<Value>, String> {
        let p = self.dir.join("events.jsonl");
        std::fs::read_to_string(&p)
            .map_err(|e| format!("{}: {e}", p.display()))?
            .lines()
            .map(|l| serde_json::from_str(l).map_err(|e| format!("{}: {e}", p.display())))
            .collect()
    }

    fn load_script(&self) -> String {
        let src = |t: &str, cols: &str| {
            format!("CREATE TABLE {t} AS SELECT * FROM read_json('{}', format='newline_delimited', columns={{{cols}}});\n", self.dir.join(format!("{t}.jsonl")).display())
        };
        let mut s = String::new();
        s += &src("events", "seq:'BIGINT', vtime:'BIGINT', kind:'VARCHAR', actor:'VARCHAR', started:'BIGINT', completed:'BIGINT', exit:'INTEGER', coalesced:'BOOLEAN', seed:'UBIGINT'");
        s += &src("bead_state", "seq:'BIGINT', bead:'VARCHAR', bd_status:'VARCHAR', lc_state:'VARCHAR', lc_version:'BIGINT', holds:'VARCHAR', landstate:'VARCHAR', lc_tip:'VARCHAR', branch_tip:'VARCHAR', on_local_main:'BOOLEAN', on_origin_main:'BOOLEAN'");
        s += &src("refs", "seq:'BIGINT', ref:'VARCHAR', sha:'VARCHAR'");
        s += &src("tags", "seq:'BIGINT', tag:'VARCHAR'");
        s += INVARIANTS;
        s
    }

    /// Rebuilds `<world>/trace.duckdb` from the JSONL, which stays the record of truth.
    pub fn load(&self, world: &Path) -> Result<PathBuf, String> {
        let db = Self::db_of(world);
        let _ = std::fs::remove_file(&db);
        duckdb(&db, &self.load_script(), false)?;
        Ok(db)
    }
}

const TABLES: &[&str] = &["events", "bead_state", "refs", "tags"];

fn duckdb(db: &Path, sql: &str, json_out: bool) -> Result<String, String> {
    let mut cmd = Command::new(duckdb_bin());
    if json_out {
        cmd.arg("-json");
    }
    cmd.arg(db).arg("-c").arg(sql);
    run(&mut cmd, QUERY_DEADLINE)
}

#[derive(Debug, PartialEq)]
pub struct Violation {
    pub view: String,
    pub seq: u64,
    pub row: String,
}

pub fn invariant_views() -> Vec<String> {
    INVARIANTS
        .lines()
        .filter_map(|l| l.strip_prefix("CREATE OR REPLACE VIEW "))
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

pub fn violations(db: &Path) -> Result<Vec<Violation>, String> {
    let parts: Vec<String> = invariant_views().iter().map(|v| format!("SELECT '{v}' AS view, seq, CAST(to_json(t) AS VARCHAR) AS row FROM {v} t")).collect();
    let sql = format!("{} ORDER BY seq, view;", parts.join(" UNION ALL "));
    let out = duckdb(db, &sql, true)?;
    if out.trim().is_empty() {
        return Ok(vec![]);
    }
    let rows: Vec<Value> = serde_json::from_str(&out).map_err(|e| format!("duckdb output: {e}"))?;
    rows.iter()
        .map(|r| {
            Ok(Violation {
                view: r["view"].as_str().ok_or("no view")?.to_string(),
                seq: r["seq"].as_u64().ok_or("no seq")?,
                row: r["row"].as_str().ok_or("no row")?.to_string(),
            })
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub struct Divergence {
    pub seq: u64,
    pub recorded: Option<String>,
    pub replayed: Option<String>,
}

/// The first event at which two traces differ, compared on everything the simulator itself decides.
pub fn first_divergence(recorded: &[Value], replayed: &[Value]) -> Option<Divergence> {
    let key = |v: &Value| {
        let mut o = v.as_object().cloned().unwrap_or_default();
        o.remove("exit");
        Value::Object(o).to_string()
    };
    for i in 0..recorded.len().max(replayed.len()) {
        let (a, b) = (recorded.get(i), replayed.get(i));
        if a.map(key) != b.map(key) {
            let seq = a.or(b).and_then(|v| v["seq"].as_u64()).unwrap_or(i as u64 + 1);
            return Some(Divergence { seq, recorded: a.map(Value::to_string), replayed: b.map(Value::to_string) });
        }
    }
    None
}
