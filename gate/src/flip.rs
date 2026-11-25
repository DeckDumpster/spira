//! The registry write and fix-bead filing for a flipped test, shared by the round and the gate.

use std::fs;
use std::path::Path;
use std::process::Stdio;

const FILE_DEADLINE: &str = "60";

#[derive(Clone, Debug, PartialEq)]
pub struct DisabledRow {
    pub test: String,
    pub state: String,
    pub bead: String,
    pub until: i64,
}

pub fn bead_id(out: &str) -> Result<String, String> {
    let start = out
        .find('{')
        .ok_or_else(|| format!("bead.sh file: no JSON in output: {out}"))?;
    let v: serde_json::Value = serde_json::from_str(&out[start..])
        .map_err(|e| format!("bead.sh file: unparsed output: {e}"))?;
    let v = match v {
        serde_json::Value::Array(a) => a.into_iter().next().ok_or("bead.sh file: empty JSON array")?,
        o => o,
    };
    v.get("id")
        .and_then(|i| i.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("bead.sh file: no id in output: {out}"))
}

/// Files a bead for `batcher`-persona work in `repo` and returns its id, parsed from the JSON
/// (law-never-derive-an-id-from-output).
pub fn file_bead(tmp_dir: &Path, repo: &str, persona: &str, tag: &str, title: &str, body: &str) -> Result<String, String> {
    fs::create_dir_all(tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let now = spira_config::vtime::now_epoch();
    let tmp = tmp_dir.join(format!("{tag}-{repo}-{now}.txt"));
    fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    // FORK-EDGE: bead.sh is the operator tool for filing; it stays
    let out = spira_config::release_env::child_path_env_for_process().and_then(|path| {
        spira_config::bounded::bounded_for("bead.sh", FILE_DEADLINE)
            .envs(path)
            .args(["file", title, "--for", persona, "--repo", repo, "--harness-title", "--body-file"])
            .arg(&tmp)
            .arg("--json")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("bead.sh file: {e}"))
    });
    let _ = fs::remove_file(&tmp);
    let out = out?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "bead.sh file exited {}: {}",
            out.status.code().unwrap_or(-1),
            err.lines().last().unwrap_or("").trim()
        ));
    }
    bead_id(&String::from_utf8_lossy(&out.stdout))
}

/// `test-registry disable` of `test` for the registry's own hold, as `actor`.
pub fn registry_disable(bin: &str, home: &Path, test: &str, bead: &str, reason: &str, actor: &str) -> Result<DisabledRow, String> {
    // batch-job: a registry write, bounded by the registry's own busy timeout
    // FORK-EDGE: machine leg test-registry (disable a flipped test)
    let out = spira_config::bounded::bounded_for(bin, FILE_DEADLINE)
        .args(["disable", "--json", "--test", test, "--bead", bead, "--reason", reason, "--actor", actor])
        .env(crate::real::HOME_ENV, home)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{bin} disable: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!(
            "{bin} disable --test {test} exited {}: {}{}",
            out.status.code().unwrap_or(-1),
            said.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_disabled_row(&said).map_err(|e| format!("{bin} disable: {e}"))
}

/// The row in `test-registry disable --json`'s stdout.
pub fn parse_disabled_row(said: &str) -> Result<DisabledRow, String> {
    let v: serde_json::Value = serde_json::from_str(said).map_err(|e| format!("unparsed output: {e}"))?;
    let row = &v["row"];
    let text = |k: &str| row[k].as_str().map(str::to_string).ok_or_else(|| format!("no {k} in {said}"));
    Ok(DisabledRow {
        test: text("test")?,
        state: text("state")?,
        bead: text("bead")?,
        until: row["until"].as_i64().ok_or_else(|| format!("no until in {said}"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t4311jt)]
    #[test]
    fn the_row_is_written_by_the_named_actor_for_the_named_bead() {
        let dir = testkit::TempDir::new("gate-flip-registry");
        let log = dir.path().join("argv");
        let tool = dir.path().join("test-registry");
        testkit::write_exe(
            &tool,
            &format!("#!/bin/sh\necho \"$@\" > {}\necho '{{\"row\":{{\"test\":\"test:t0aaaaa\",\"state\":\"disabled\",\"bead\":\"sp-fix\",\"until\":1790000000}}}}'\n", log.display()),
        );
        let row = registry_disable(tool.to_str().unwrap(), dir.path(), "test:t0aaaaa", "sp-fix", "flipped", "gate").unwrap();
        assert_eq!((row.bead.as_str(), row.until), ("sp-fix", 1790000000));
        let argv = std::fs::read_to_string(&log).unwrap();
        assert!(argv.contains("--test test:t0aaaaa --bead sp-fix --reason flipped --actor gate"), "{argv}");
    }

    #[test_id(t9xp7ub)]
    #[test]
    fn a_refusing_registry_is_an_error_naming_its_exit() {
        let dir = testkit::TempDir::new("gate-flip-refused");
        let tool = dir.path().join("test-registry");
        testkit::write_exe(&tool, "#!/bin/sh\necho busy\nexit 3\n");
        let e = registry_disable(tool.to_str().unwrap(), dir.path(), "test:t0aaaaa", "sp-fix", "r", "gate").unwrap_err();
        assert!(e.contains("exited 3") && e.contains("busy"), "{e}");
    }
}
