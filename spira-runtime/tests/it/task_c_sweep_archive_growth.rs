//! A transcript that grows between passes is stored by the Rust sweep as appended frames, and the
//! operator CLI (`archive.sh`) reads the result back byte for byte.

use std::path::{Path, PathBuf};
use std::process::Command;

use runtime::ports::SysProc;
use spira_runtime::tasks::c::sweep_archive::{archive, Store};

const REL: &str = "p/a.jsonl";

fn line(i: usize) -> Vec<u8> {
    format!("{{\"type\":\"assistant\",\"timestamp\":\"2026-11-24T10:{:02}:{:02}Z\",\"sessionId\":\"s1\",\"bridgeSessionId\":\"b1\",\"pad\":\"{}\"}}\n", i / 60 % 60, i % 60, "x".repeat(990)).into_bytes()
}

/// `archive.sh` in an explicit environment: its config loader is marked loaded and `spira_require` is the one function it asks of it.
fn cli(dir: &Path, args: &[&str]) -> (bool, Vec<u8>) {
    let shim = dir.join("shim.sh");
    std::fs::write(&shim, "spira_require() { command -v \"$1\" >/dev/null 2>&1; }\n").unwrap();
    let script = testkit::manifest_dir().join("../spira/archive.sh");
    let out = Command::new("bash")
        .arg(script)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("BASH_ENV", &shim)
        .env("SPIRA_CONF_LOADED", "1")
        .env("SPIRA_ARCHIVE", dir.join("archive"))
        .env("SPIRA_TOKEN_PROJECTS", dir.join("projects"))
        .output()
        .unwrap();
    (out.status.success(), out.stdout)
}

#[test_id(t6432y8)]
#[test]
fn a_transcript_stored_grown_and_stored_again_restores_verifies_and_queries_as_one_row() {
    let tmp = testkit::TempDir::new("sweep-archive-growth");
    let dir: PathBuf = tmp.path().to_path_buf();
    std::fs::create_dir_all(dir.join("projects/p")).unwrap();
    let p = archive::Paths { archive: dir.join("archive"), projects: dir.join("projects"), default_projects: dir.join("home") };
    let job = Store { rel: REL.into(), size: 0, mtime: 0 };
    let mut data: Vec<u8> = (0..9_000).flat_map(line).collect();
    assert!(data.len() > 8 << 20, "more than one frame");
    std::fs::write(p.projects.join(REL), &data).unwrap();
    archive::store(&p, &SysProc, &job, 1_800_000_000).unwrap();
    archive::index(&p).unwrap();
    let partial = line(9_000);
    data.extend_from_slice(&partial[..100]);
    std::fs::write(p.projects.join(REL), &data).unwrap();
    archive::store(&p, &SysProc, &job, 1_800_000_100).unwrap();
    archive::index(&p).unwrap();
    data.extend_from_slice(&partial[100..]);
    data.extend(line(9_001));
    std::fs::write(p.projects.join(REL), &data).unwrap();
    archive::store(&p, &SysProc, &job, 1_800_000_200).unwrap();
    archive::index(&p).unwrap();

    let (ok, restored) = cli(&dir, &["restore", REL]);
    assert!(ok && restored == data, "restore returns the source bytes");
    let (ok, verified) = cli(&dir, &["verify"]);
    assert!(ok, "{}", String::from_utf8_lossy(&verified));
    let (ok, rows) = cli(&dir, &["query", "--json"]);
    assert!(ok);
    let rows = String::from_utf8(rows).unwrap();
    assert_eq!(rows.lines().count(), 1, "{rows}");
    assert!(rows.contains(&format!("\"bytes\": {}", data.len())));

    let index_bytes = || std::fs::read(p.index()).unwrap();
    let kept = index_bytes();
    let (ok, out) = cli(&dir, &["sweep"]);
    assert!(ok && String::from_utf8_lossy(&out).contains("0 stored") && String::from_utf8_lossy(&out).contains("index untouched"), "{}", String::from_utf8_lossy(&out));
    assert_eq!(index_bytes(), kept, "a sweep that stored nothing leaves the index untouched");

    data.extend(line(9_002));
    std::fs::write(p.projects.join(REL), &data).unwrap();
    let (ok, out) = cli(&dir, &["sweep"]);
    assert!(ok && String::from_utf8_lossy(&out).contains("1 stored"), "{}", String::from_utf8_lossy(&out));
    let grown = index_bytes();
    assert!(grown.starts_with(&kept) && grown.len() > kept.len(), "the sweep appended its row to the index");
    assert_eq!(String::from_utf8(grown.clone()).unwrap().lines().count(), 2, "the compacted row and the one the sweep appended");
    let (ok, restored) = cli(&dir, &["restore", REL]);
    assert!(ok && restored == data);
    let (ok, verified) = cli(&dir, &["verify"]);
    assert!(ok, "{}", String::from_utf8_lossy(&verified));

    std::fs::remove_file(p.projects.join(REL)).unwrap();
    let (ok, out) = cli(&dir, &["sweep"]);
    assert!(ok && String::from_utf8_lossy(&out).contains("1 row(s) in the index"), "{}", String::from_utf8_lossy(&out));
    assert_eq!(index_bytes(), grown, "a row whose source is gone is kept");
    let (ok, rows) = cli(&dir, &["query", "--json"]);
    assert!(ok && String::from_utf8(rows).unwrap().lines().count() == 1);
}
