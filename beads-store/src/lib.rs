//! beads-store — commit a beads Dolt store's dirty tables through the real `dolt` engine.
//! Contract, scar and scope: DESIGN.md.

use std::path::{Path, PathBuf};
use std::process::Command;

/// How to reach the store's real dolt engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineKind {
    /// A local on-disk data directory (embedded, or production's `SPIRA_DOLT_DATA`).
    DataDir(PathBuf),
    /// A live `dolt sql-server`, reached over the wire like any other client.
    Tcp {
        host: String,
        port: u16,
        user: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Engine {
    pub kind: EngineKind,
    pub dbname: String,
}

fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_metadata(db: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(db.join(".beads/metadata.json")).ok()?;
    serde_json::from_str(&text).ok()
}

fn meta_str(meta: &Option<serde_json::Value>, key: &str) -> Option<String> {
    meta.as_ref()?.get(key)?.as_str().map(str::to_string)
}

fn meta_port(meta: &Option<serde_json::Value>) -> Option<u16> {
    let v = meta.as_ref()?.get("dolt_server_port")?;
    v.as_u64().and_then(|n| u16::try_from(n).ok())
}

/// Resolve the store's real dolt engine (DESIGN.md §4). `spira_dolt_data` is the caller's
/// `SPIRA_DOLT_DATA` env var, passed in rather than read here so this is testable without
/// touching process environment.
pub fn resolve(db: &Path, spira_dolt_data: Option<&str>) -> Result<Engine, String> {
    let meta = read_metadata(db);

    // 1. SPIRA_DOLT_DATA: production's configured local data directory.
    if let Some(dd) = spira_dolt_data {
        let p = Path::new(dd);
        if p.is_dir() {
            let dbname = meta_str(&meta, "dolt_database").unwrap_or_else(|| "spira".to_string());
            return Ok(Engine {
                kind: EngineKind::DataDir(p.to_path_buf()),
                dbname,
            });
        }
    }

    // 2. Embedded: a private on-disk store under .beads/embeddeddolt/<name>.
    let emb = db.join(".beads/embeddeddolt");
    if emb.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&emb)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| !n.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        if let Some(name) = names.into_iter().next() {
            return Ok(Engine {
                kind: EngineKind::DataDir(emb.join(&name)),
                dbname: name,
            });
        }
    }

    // 3. Server mode: a live sql-server, named by metadata.json or the port sidecar file
    // testenv testdb also writes.
    let port = meta_port(&meta)
        .or_else(|| read_trim(&db.join(".beads/dolt-server.port")).and_then(|s| s.parse().ok()));
    if let Some(port) = port {
        let host = meta_str(&meta, "dolt_server_host").unwrap_or_else(|| "127.0.0.1".to_string());
        let user = meta_str(&meta, "dolt_server_user").unwrap_or_else(|| "root".to_string());
        let dbname = meta_str(&meta, "dolt_database").unwrap_or_else(|| "spira".to_string());
        return Ok(Engine {
            kind: EngineKind::Tcp { host, port, user },
            dbname,
        });
    }

    Err(format!(
        "cannot determine the store's dolt engine under {}: no SPIRA_DOLT_DATA directory, \
         no .beads/embeddeddolt, and no server port in metadata.json or dolt-server.port",
        db.display()
    ))
}

/// Escape a value for a single-quoted SQL string literal (doubled single quotes).
fn sql_quote(s: &str) -> String {
    s.replace('\'', "''")
}

impl Engine {
    fn command(&self, dolt_bin: &str) -> Command {
        let mut cmd = Command::new(dolt_bin);
        match &self.kind {
            EngineKind::DataDir(dir) => {
                cmd.arg("--data-dir").arg(dir);
            }
            EngineKind::Tcp { host, port, user } => {
                cmd.args([
                    "--host",
                    host,
                    "--port",
                    &port.to_string(),
                    "-u",
                    user,
                    "-p",
                    "",
                    "--no-tls",
                ]);
            }
        }
        cmd.args(["--use-db", &self.dbname]);
        cmd
    }

    /// One SQL statement, run through the real engine. Returns stdout (CSV) on success.
    pub fn sql(&self, dolt_bin: &str, query: &str) -> Result<String, String> {
        let out = self
            .command(dolt_bin)
            .args(["sql", "-q", query, "-r", "csv"])
            .output()
            .map_err(|e| format!("spawning {dolt_bin}: {e}"))?;
        if !out.status.success() {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return Err(tail_lines(&combined, 2));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    pub fn dirty_count(&self, dolt_bin: &str) -> Result<u32, String> {
        let out = self.sql(dolt_bin, "select count(*) as n from dolt_status;")?;
        parse_single_number(&out)
            .ok_or_else(|| format!("could not parse dirty count from: {}", out.trim()))
    }

    pub fn commit(&self, dolt_bin: &str, message: &str) -> Result<(), String> {
        let q = format!("CALL DOLT_COMMIT('-Am', '{}');", sql_quote(message));
        self.sql(dolt_bin, &q).map(|_| ())
    }

    pub fn label(&self) -> String {
        match &self.kind {
            EngineKind::DataDir(dir) => format!("data-dir:{}", dir.display()),
            EngineKind::Tcp { host, port, .. } => format!("tcp:{host}:{port}"),
        }
    }
}

/// The last row of a `-r csv` single-column result (skips the header, trims whitespace),
/// parsed as an unsigned integer.
fn parse_single_number(csv: &str) -> Option<u32> {
    csv.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| l.chars().all(|c| c.is_ascii_digit()))
        .next_back()
        .and_then(|l| l.parse().ok())
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" ")
}

/// The whole pre-push commit step (DESIGN.md §3): count dirty tables, commit if any are
/// dirty, and confirm the commit actually cleared them. Returns the number of dirty
/// tables that were committed (0 for an already-clean store — a verified no-op, never an
/// empty commit).
pub fn run_commit(engine: &Engine, dolt_bin: &str, message: &str) -> Result<u32, String> {
    let before = engine.dirty_count(dolt_bin).map_err(|e| {
        format!(
            "cannot determine whether the store has uncommitted changes (engine: {}): {e}",
            engine.label()
        )
    })?;
    if before == 0 {
        return Ok(0);
    }
    engine
        .commit(dolt_bin, message)
        .map_err(|e| format!("pre-push commit failed: {e}"))?;
    // A COMMIT THAT CLEARS NOTHING IS A FAILED COMMIT (2026-09-15 scar, sp-e1l1): `bd dolt
    // commit` returned 0 and "Nothing to commit." for a working set the real engine could
    // see. Re-reading the count through the SAME engine that just committed is what
    // catches that; taking the commit call's exit status alone is what let the statute
    // book sit uncommitted for weeks.
    let after = engine.dirty_count(dolt_bin).map_err(|e| {
        format!("committed, but could not re-read the working set to confirm it cleared: {e}")
    })?;
    if after != 0 {
        return Err(format!(
            "pre-push commit did not clear the working set: {before} dirty table(s) before, \
             {after} after (engine: {})",
            engine.label()
        ));
    }
    Ok(before)
}

/// The last non-header, non-empty cell of a single-column csv result.
fn last_cell(csv: &str) -> Option<String> {
    let rows: Vec<&str> = csv
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    rows.get(1..)?.last().map(|s| s.to_string())
}

/// Push the active branch to `remote` and prove it by the remote's own head: the push,
/// the tracking-ref refresh and both head reads all go through `engine`, so the store that
/// was pushed is the store that is verified. Returns the head both sides now share.
pub fn run_push(engine: &Engine, dolt_bin: &str, remote: &str) -> Result<String, String> {
    let r = sql_quote(remote);
    let branch = engine
        .sql(dolt_bin, "select active_branch() as b;")
        .ok()
        .and_then(|o| last_cell(&o))
        .ok_or_else(|| format!("cannot read the active branch (engine: {})", engine.label()))?;
    let b = sql_quote(&branch);
    engine
        .sql(dolt_bin, &format!("CALL DOLT_PUSH('{r}', '{b}');"))
        .map_err(|e| format!("push failed: {e}"))?;
    engine
        .sql(dolt_bin, &format!("CALL DOLT_FETCH('{r}');"))
        .map_err(|e| format!("pushed, but could not refresh the remote tracking ref: {e}"))?;
    let head = |rev: String| {
        engine
            .sql(
                dolt_bin,
                &format!(
                    "select commit_hash from dolt_log('{}') limit 1;",
                    sql_quote(&rev)
                ),
            )
            .ok()
            .and_then(|o| last_cell(&o))
    };
    let local = head(branch.clone());
    let remote_head = head(format!("remotes/{remote}/{branch}"));
    match (local, remote_head) {
        (Some(l), Some(r)) if l == r => Ok(r),
        (Some(l), Some(r)) => Err(format!(
            "push reported complete but the remote did not move: local {l}, remote {r} (engine: {})",
            engine.label()
        )),
        (l, r) => Err(format!(
            "pushed, but could not read both heads to verify it (local={l:?} remote={r:?})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testkit::{write_exe, TempDir};

    fn write_metadata(db: &Path, json: &str) {
        std::fs::create_dir_all(db.join(".beads")).unwrap();
        std::fs::write(db.join(".beads/metadata.json"), json).unwrap();
    }

    // ---- resolve() ---------------------------------------------------------------

    #[test]
    fn resolve_prefers_spira_dolt_data_when_it_is_a_directory() {
        let db = TempDir::new("bs-db");
        let dd = TempDir::new("bs-dd");
        write_metadata(&db, r#"{"dolt_database":"spira"}"#);
        let e = resolve(&db, Some(dd.path().to_str().unwrap())).unwrap();
        assert_eq!(e.kind, EngineKind::DataDir(dd.path().to_path_buf()));
        assert_eq!(e.dbname, "spira");
    }

    #[test]
    fn resolve_ignores_spira_dolt_data_when_the_directory_does_not_exist() {
        let db = TempDir::new("bs-db");
        write_metadata(&db, r#"{"dolt_server_port": 5555}"#);
        let e = resolve(&db, Some("/no/such/directory/here")).unwrap();
        assert_eq!(
            e.kind,
            EngineKind::Tcp {
                host: "127.0.0.1".into(),
                port: 5555,
                user: "root".into()
            }
        );
    }

    #[test]
    fn resolve_finds_embedded_store() {
        let db = TempDir::new("bs-db");
        std::fs::create_dir_all(db.join(".beads/embeddeddolt/sp1234")).unwrap();
        let e = resolve(&db, None).unwrap();
        assert_eq!(
            e.kind,
            EngineKind::DataDir(db.join(".beads/embeddeddolt/sp1234"))
        );
        assert_eq!(e.dbname, "sp1234");
    }

    #[test]
    fn resolve_reads_server_fields_from_metadata_json() {
        let db = TempDir::new("bs-db");
        write_metadata(
            &db,
            r#"{"dolt_database":"sptest","dolt_mode":"server","dolt_server_host":"127.0.0.1","dolt_server_port":46361}"#,
        );
        let e = resolve(&db, None).unwrap();
        assert_eq!(
            e.kind,
            EngineKind::Tcp {
                host: "127.0.0.1".into(),
                port: 46361,
                user: "root".into()
            }
        );
        assert_eq!(e.dbname, "sptest");
    }

    #[test]
    fn resolve_falls_back_to_the_port_sidecar_file_without_metadata() {
        let db = TempDir::new("bs-db");
        std::fs::create_dir_all(db.join(".beads")).unwrap();
        std::fs::write(db.join(".beads/dolt-server.port"), "9911\n").unwrap();
        let e = resolve(&db, None).unwrap();
        assert_eq!(
            e.kind,
            EngineKind::Tcp {
                host: "127.0.0.1".into(),
                port: 9911,
                user: "root".into()
            }
        );
        assert_eq!(e.dbname, "spira");
    }

    #[test]
    fn resolve_refuses_when_nothing_names_an_engine() {
        let db = TempDir::new("bs-db");
        let err = resolve(&db, None).unwrap_err();
        assert!(err.contains("cannot determine"), "{err}");
    }

    // ---- parse_single_number -------------------------------------------------------

    #[test]
    fn parse_single_number_skips_the_header_row() {
        assert_eq!(parse_single_number("n\n1\n"), Some(1));
        assert_eq!(parse_single_number("n\n0\n"), Some(0));
        assert_eq!(parse_single_number("n\n"), None);
        assert_eq!(parse_single_number(""), None);
    }

    // ---- run_commit(), against a stub `dolt` ---------------------------------------
    //
    // The stub reads/writes a small state file so successive invocations (before-count,
    // commit, after-count) can simulate a real session without a live server.

    fn stub_engine(bin_dir: &Path) -> Engine {
        Engine {
            kind: EngineKind::DataDir(bin_dir.join("data")),
            dbname: "spira".into(),
        }
    }

    fn write_dolt_stub(bin: &Path, script: &str) {
        write_exe(bin.join("dolt"), script);
    }

    #[test]
    fn run_commit_is_a_noop_on_a_clean_store() {
        let bin = TempDir::new("bs-bin");
        write_dolt_stub(
            &bin,
            r#"#!/usr/bin/env bash
echo "n"; echo "0"
"#,
        );
        let engine = stub_engine(&bin);
        let dolt = bin.join("dolt");
        let got = run_commit(&engine, dolt.to_str().unwrap(), "msg").unwrap();
        assert_eq!(got, 0);
    }

    #[test]
    fn run_commit_commits_and_clears_a_dirty_store() {
        let bin = TempDir::new("bs-bin");
        let state = bin.join("calls");
        std::fs::write(&state, "0").unwrap();
        write_dolt_stub(
            &bin,
            &format!(
                r#"#!/usr/bin/env bash
STATE="{state}"
n="$(cat "$STATE")"
echo $((n+1)) > "$STATE"
if [[ "$*" == *DOLT_COMMIT* ]]; then
    echo "hash"; echo "abc123"
elif [ "$n" = "0" ]; then
    echo "n"; echo "1"
else
    echo "n"; echo "0"
fi
"#,
                state = state.display()
            ),
        );
        let engine = stub_engine(&bin);
        let dolt = bin.join("dolt");
        let got = run_commit(&engine, dolt.to_str().unwrap(), "beads-push: test").unwrap();
        assert_eq!(got, 1);
    }

    #[test]
    fn run_commit_reports_the_exact_scar_a_commit_that_does_not_clear() {
        let bin = TempDir::new("bs-bin");
        write_dolt_stub(
            &bin,
            r#"#!/usr/bin/env bash
if [[ "$*" == *DOLT_COMMIT* ]]; then
    echo "message"; echo "Nothing to commit."
else
    echo "n"; echo "1"
fi
"#,
        );
        let engine = stub_engine(&bin);
        let dolt = bin.join("dolt");
        let err = run_commit(&engine, dolt.to_str().unwrap(), "msg").unwrap_err();
        assert!(err.contains("did not clear the working set"), "{err}");
        assert!(err.contains("1 dirty table(s) before, 1 after"), "{err}");
    }

    #[test]
    fn run_commit_surfaces_a_failed_commit_call() {
        let bin = TempDir::new("bs-bin");
        write_dolt_stub(
            &bin,
            r#"#!/usr/bin/env bash
if [[ "$*" == *DOLT_COMMIT* ]]; then
    echo "conflict" >&2
    exit 1
else
    echo "n"; echo "1"
fi
"#,
        );
        let engine = stub_engine(&bin);
        let dolt = bin.join("dolt");
        let err = run_commit(&engine, dolt.to_str().unwrap(), "msg").unwrap_err();
        assert!(err.contains("pre-push commit failed"), "{err}");
    }

    #[test]
    fn run_commit_surfaces_an_unreadable_dirty_count() {
        let bin = TempDir::new("bs-bin");
        write_dolt_stub(
            &bin,
            r#"#!/usr/bin/env bash
echo "not a number"
"#,
        );
        let engine = stub_engine(&bin);
        let dolt = bin.join("dolt");
        let err = run_commit(&engine, dolt.to_str().unwrap(), "msg").unwrap_err();
        assert!(
            err.contains("cannot determine whether the store has uncommitted changes"),
            "{err}"
        );
    }

    #[test]
    fn run_push_succeeds_only_when_heads_match() {
        let bin = TempDir::new("bs-bin");
        write_dolt_stub(
            &bin,
            r#"#!/usr/bin/env bash
case "$*" in
  *active_branch*) echo b; echo main ;;
  *remotes/*) echo commit_hash; echo "${REMOTE_HEAD:-h1}" ;;
  *dolt_log*) echo commit_hash; echo h1 ;;
  *) echo ok ;;
esac
"#,
        );
        let engine = stub_engine(&bin);
        let dolt = bin.join("dolt");
        assert_eq!(
            run_push(&engine, dolt.to_str().unwrap(), "beads").unwrap(),
            "h1"
        );
    }

    #[test]
    fn run_push_fails_when_the_remote_head_differs() {
        let bin = TempDir::new("bs-bin");
        write_dolt_stub(
            &bin,
            r#"#!/usr/bin/env bash
case "$*" in
  *active_branch*) echo b; echo main ;;
  *remotes/*) echo commit_hash; echo stale ;;
  *dolt_log*) echo commit_hash; echo h1 ;;
  *) echo ok ;;
esac
"#,
        );
        let engine = stub_engine(&bin);
        let dolt = bin.join("dolt");
        let err = run_push(&engine, dolt.to_str().unwrap(), "beads").unwrap_err();
        assert!(err.contains("did not move"), "{err}");
    }
}
