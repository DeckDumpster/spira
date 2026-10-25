//! The world's `run/` directory as trace tables: one table per family of files the harness keeps
//! there. A state family (`run_verdicts`, `run_queue`, `run_ejected`, `run_locks`) is the whole
//! family at each snapshot; a log family (`run_gate_log`, `run_landing_log`) is each line once,
//! at the snapshot that first saw it.

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub struct Table {
    pub name: &'static str,
    pub columns: &'static [(&'static str, &'static str)],
    pub log: bool,
}

pub const TABLES: &[Table] = &[
    Table {
        name: "run_verdicts",
        columns: &[("key", "VARCHAR"), ("repo", "VARCHAR"), ("branch", "VARCHAR"), ("stamped", "BIGINT"), ("suites", "VARCHAR"), ("compose", "VARCHAR"), ("certified_by", "VARCHAR")],
        log: false,
    },
    Table { name: "run_queue", columns: &[("repo", "VARCHAR"), ("file", "VARCHAR"), ("key", "VARCHAR"), ("value", "VARCHAR")], log: false },
    Table { name: "run_ejected", columns: &[("bead", "VARCHAR"), ("suites", "VARCHAR")], log: false },
    Table { name: "run_locks", columns: &[("name", "VARCHAR"), ("holder", "VARCHAR"), ("held", "BOOLEAN")], log: false },
    Table {
        name: "run_gate_log",
        columns: &[("line", "BIGINT"), ("ts", "VARCHAR"), ("repo", "VARCHAR"), ("branch", "VARCHAR"), ("rc", "INTEGER"), ("text", "VARCHAR")],
        log: true,
    },
    Table {
        name: "run_landing_log",
        columns: &[("file", "VARCHAR"), ("line", "BIGINT"), ("ts", "VARCHAR"), ("event", "VARCHAR"), ("repo", "VARCHAR"), ("text", "VARCHAR")],
        log: true,
    },
];

pub fn table(name: &str) -> Option<&'static Table> {
    TABLES.iter().find(|t| t.name == name)
}

/// Rejects a row naming a field its table lacks, so a misspelt column cannot read as an absent one.
pub fn normalise(t: &Table, row: &Value) -> Result<Value, String> {
    let o = row.as_object().ok_or_else(|| format!("{}: row is not an object: {row}", t.name))?;
    if let Some(k) = o.keys().find(|k| !t.columns.iter().any(|(c, _)| c == k)) {
        return Err(format!("{}: unknown field {k:?}: {row}", t.name));
    }
    let mut out = Map::new();
    for (c, _) in t.columns {
        out.insert((*c).to_string(), o.get(*c).cloned().unwrap_or(Value::Null));
    }
    Ok(Value::Object(out))
}

pub fn load_sql(dir: &Path) -> String {
    TABLES
        .iter()
        .map(|t| {
            let cols = t.columns.iter().map(|(c, ty)| format!("{c}:'{ty}'")).collect::<Vec<_>>().join(", ");
            let file = dir.join(format!("{}.jsonl", t.name));
            format!("CREATE TABLE {} AS SELECT * FROM read_json('{}', format='newline_delimited', columns={{seq:'BIGINT', {cols}}});\n", t.name, file.display())
        })
        .collect()
}

pub fn kv(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect()
}

fn lookup<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

pub fn verdict_row(key: &str, text: &str) -> Value {
    let p = kv(text);
    let get = |k: &str| lookup(&p, k).map(str::to_string);
    json!({"key": key, "repo": get("repo"), "branch": get("branch"), "stamped": get("at").and_then(|a| a.parse::<i64>().ok()),
           "suites": get("suites"), "compose": get("compose"), "certified_by": get("by")})
}

/// `<ts> <repo> <branch> waited=.. ran=.. rc=N ...`
pub fn gate_log_row(line_no: usize, line: &str) -> Value {
    let mut w = line.split_whitespace();
    let (ts, repo, branch) = (w.next(), w.next(), w.next());
    let rc = line.split_whitespace().find_map(|t| t.strip_prefix("rc=")).and_then(|r| r.parse::<i64>().ok());
    json!({"line": line_no, "ts": ts, "repo": repo, "branch": branch, "rc": rc, "text": line})
}

/// `<ts> <repo>: <text>` (repo absent when the line names none), or a queue record,
/// `QUEUE <EVENT> <epoch> key=value...`, which carries its repo as `repo=`.
pub fn landing_log_row(file: &str, line_no: usize, line: &str) -> Value {
    if let Some(rest) = line.strip_prefix("QUEUE ") {
        let (event, text) = rest.split_once(' ').unwrap_or((rest, ""));
        let repo = text.split_whitespace().find_map(|t| t.strip_prefix("repo="));
        return json!({"file": file, "line": line_no, "ts": null, "event": event, "repo": repo, "text": text});
    }
    let (ts, rest) = line.split_once(' ').unwrap_or((line, ""));
    let (repo, text) = match rest.split_once(": ") {
        Some((r, t)) if !r.is_empty() && !r.contains(' ') => (Some(r), t),
        _ => (None, rest),
    };
    json!({"file": file, "line": line_no, "ts": ts, "event": null, "repo": repo, "text": text})
}

pub fn is_lock_name(name: &str) -> bool {
    name == "lock" || name.ends_with(".lock")
}

/// The inodes the kernel lists as holding an flock, from `/proc/locks`.
pub fn flocked_inodes(proc_locks: &str) -> BTreeSet<u64> {
    proc_locks
        .lines()
        .filter(|l| l.split_whitespace().nth(1) == Some("FLOCK"))
        .filter_map(|l| l.split_whitespace().nth(5))
        .filter_map(|id| id.rsplit(':').next())
        .filter_map(|ino| ino.parse().ok())
        .collect()
}

fn holder_of(text: &str) -> Option<String> {
    let line = text.lines().next()?;
    let pid = line.split_whitespace().find_map(|t| t.strip_prefix("pid="));
    pid.or_else(|| line.split_whitespace().next()).map(str::to_string)
}

fn files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    v.sort();
    v
}

fn dirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    v.sort();
    v
}

fn lines_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_string).collect()
}

/// Every table's rows (without `seq`) as the world's `run/` holds them now.
pub fn read(run: &Path, proc_locks: &str) -> Vec<(&'static str, Value)> {
    let mut out: Vec<(&'static str, Value)> = Vec::new();
    for (key, path) in files(&run.join("verdicts")) {
        out.push(("run_verdicts", verdict_row(&key, &std::fs::read_to_string(path).unwrap_or_default())));
    }
    let mut lock_files: Vec<(String, PathBuf)> = Vec::new();
    let mut queue_dirs = vec![(String::new(), run.join("queue"))];
    queue_dirs.extend(dirs(&run.join("queue")));
    for (repo, dir) in queue_dirs {
        for (name, path) in files(&dir) {
            if is_lock_name(&name) {
                lock_files.push((format!("queue/{}{}{name}", repo, if repo.is_empty() { "" } else { "/" }), path));
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            for l in text.lines().filter(|l| !l.trim().is_empty()) {
                let (k, v) = l.split_once('=').map_or((None, l), |(k, v)| (Some(k.trim()), v.trim()));
                out.push(("run_queue", json!({"repo": repo, "file": name, "key": k, "value": v})));
            }
        }
    }
    for (bead, path) in files(&run.join("ejected")) {
        out.push(("run_ejected", json!({"bead": bead, "suites": std::fs::read_to_string(path).unwrap_or_default().trim()})));
    }
    for (name, path) in files(run).into_iter().filter(|(n, _)| is_lock_name(n)).chain(
        files(&run.join("worktree")).into_iter().filter(|(n, _)| is_lock_name(n)),
    ) {
        lock_files.push((name, path));
    }
    let held = flocked_inodes(proc_locks);
    for (name, path) in lock_files {
        use std::os::unix::fs::MetadataExt;
        let is_held = std::fs::metadata(&path).is_ok_and(|m| held.contains(&m.ino()));
        let holder = holder_of(&std::fs::read_to_string(&path).unwrap_or_default());
        out.push(("run_locks", json!({"name": name, "holder": holder, "held": is_held})));
    }
    for (i, l) in lines_of(&run.join("gate.log")).iter().enumerate() {
        out.push(("run_gate_log", gate_log_row(i + 1, l)));
    }
    for f in ["landing.log", "landing-pass.log"] {
        for (i, l) in lines_of(&run.join(f)).iter().enumerate() {
            out.push(("run_landing_log", landing_log_row(f, i + 1, l)));
        }
    }
    out
}

/// What a snapshot of the world's `run/` holds, read from the world's own directory and the
/// kernel's lock list.
pub fn observe(world: &Path) -> Vec<(&'static str, Value)> {
    read(&world.join("run"), &std::fs::read_to_string("/proc/locks").unwrap_or_default())
}
