//! Shells to `bd` for the non-lifecycle half of the aeon semantic layer (design §3.5):
//! titles, descriptions, dependencies, notes and filing stay bd's job (§3.3/3.4) — this
//! module never touches `spira_lifecycle`. `SPIRA_BD`/`SPIRA_DB` are declared config
//! (spira/conf.d), resolved from `$SPIRA_TOML` (one source of config, per Ryan 2026-10-05) —
//! never this process's own environment.

use std::process::Command;

fn bd_bin() -> Result<String, String> {
    spira_config::process::cfg("SPIRA_BD")
}

fn run(args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(bd_bin()?);
    let db = spira_config::process::cfg("SPIRA_DB")?;
    if !db.is_empty() {
        cmd.args(["-C", &db]);
    }
    cmd.args(args);
    let out = cmd.output().map_err(|e| format!("running bd {args:?}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(stdout)
}

pub fn show(bead_id: &str) -> Result<String, String> {
    run(&["show", bead_id])
}

/// The bound bead's own `repo:<name>` label, so `file-followup`/`split` never ask the
/// aeon for a repo it could get wrong — the layer reads it from the bead it was summoned
/// for, the same one every filing must land in.
pub fn repo_label(bead_id: &str) -> Result<Option<String>, String> {
    let out = run(&["show", bead_id, "--json"])?;
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("bd show --json: {e}"))?;
    let doc = match &parsed {
        serde_json::Value::Array(a) => a.first().cloned().unwrap_or(serde_json::Value::Null),
        v => v.clone(),
    };
    let labels = doc.get("labels").and_then(|l| l.as_array()).cloned().unwrap_or_default();
    Ok(labels.iter().filter_map(|l| l.as_str()).find_map(|l| l.strip_prefix("repo:").map(str::to_string)))
}

/// The live [`crate::callers::Bd`]: bd itself, resolved as [`run`] resolves it.
pub struct LiveBd;

impl crate::callers::Bd for LiveBd {
    fn issue_type(&mut self, id: &str) -> Result<String, String> {
        let out = run(&["show", id, "--json"])?;
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("bd show --json: {e}"))?;
        let doc = match &parsed {
            serde_json::Value::Array(a) => a.first().cloned().unwrap_or(serde_json::Value::Null),
            v => v.clone(),
        };
        doc.get("issue_type").and_then(|t| t.as_str()).map(str::to_string).ok_or_else(|| format!("bd show {id}: no issue_type"))
    }
    fn close(&mut self, id: &str, reason: &str) -> Result<(), String> {
        // --force: the lifecycle row is already terminal when `close` gets here, so the store
        // must follow it; a refused store close would leave the two disagreeing again.
        run(&["close", id, "--force", "--reason", reason]).map(|_| ())
    }
    fn closed(&mut self, ids: &[String]) -> Result<Vec<(String, String)>, String> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let out = run(&["list", "--id", &ids.join(","), "--status", "closed", "--limit", "0", "--json"])?;
        let start = out.find(['[', '{']).ok_or("bd list: no JSON")?;
        let v: serde_json::Value = serde_json::from_str(&out[start..]).map_err(|e| format!("bd list --json: {e}"))?;
        let rows = match v {
            serde_json::Value::Array(a) => a,
            o => vec![o],
        };
        let text = |r: &serde_json::Value, k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        Ok(rows.iter().filter(|r| text(r, "status") == "closed").map(|r| (text(r, "id"), text(r, "close_reason"))).filter(|(id, _)| !id.is_empty()).collect())
    }
}

pub fn note(bead_id: &str, text: &str) -> Result<String, String> {
    run(&["note", bead_id, text])
}

/// Files through `spira/bead.sh`'s contract (the labelling rules `bd create` alone does not
/// enforce), with `--parent` set by this layer, never the caller — see bead.sh's own
/// `--parent` doc for why an inherited `branch:` label made `groomer split-piece` a
/// two-step dance that this avoids by filing with the right labels from the start.
pub fn file_child(title: &str, persona: &str, repo: &str, parent: &str) -> Result<String, String> {
    // bead.sh, by name on the launcher's PATH (sp-gypjk) — never repository-relative.
    let bead_sh = "bead.sh";
    let out = Command::new(bead_sh)
        .args(["file", title, "--for", persona, "--repo", repo, "--parent", parent])
        .output()
        .map_err(|e| format!("running {bead_sh}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(stdout.trim().to_string())
}

/// `mail send operator` — an ask carrying the channel back (design §3.5's `work
/// blocked`, and the supersede-by confirmation request). `--bead` ties the tracking
/// decision bead mail files to the one this layer is bound to.
pub fn ask_operator(from: &str, subject: &str, default: &str, bead_id: &str, body: &str) -> Result<String, String> {
    // mail, by name on the launcher's PATH (sp-gypjk); SPIRA_MAIL_SH is the harness-wide
    // binary-override seam.
    let mail_sh = std::env::var("SPIRA_MAIL_SH").unwrap_or_else(|_| "mail".to_string());
    let mut child = Command::new(&mail_sh)
        .args(["send", "operator", "--from", from, "--subject", subject, "--kind", "question", "--default", default, "--bead", bead_id])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("running {mail_sh}: {e}"))?;
    use std::io::Write;
    child.stdin.take().expect("piped stdin").write_all(body.as_bytes()).map_err(|e| format!("writing to {mail_sh}: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("waiting on {mail_sh}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(stdout)
}

/// `bd <args>` with `stdin` piped in when given — the lane verbs' one door to bd
/// (sp-st0mm). Same `SPIRA_BD`/`SPIRA_DB` resolution as [`run`].
pub fn run_stdin(args: &[String], stdin: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new(bd_bin()?);
    let db = spira_config::process::cfg("SPIRA_DB")?;
    if !db.is_empty() {
        cmd.args(["-C", &db]);
    }
    cmd.args(args);
    let (code, out) = spawn(cmd, stdin, "bd");
    if code == 0 {
        Ok(out)
    } else {
        Err(out)
    }
}

/// A harness tool by bare name on the launcher's PATH (sp-gypjk), run under `timeout` with
/// the caller's persona as `SPIRA_FAYTH` — the czar fence in `queue` and `bdq` reads it, and
/// the broker's own identity is never the persona that asked. Returns the tool's own exit code
/// (stdout; stdout plus stderr when it failed).
pub fn tool(program: &str, args: &[String], stdin: Option<&str>, actor: &str, secs: u64) -> (i32, String) {
    tool_env(program, args, stdin, actor, secs, &[])
}

/// [`tool`], with `env` set on the child as well.
pub fn tool_env(program: &str, args: &[String], stdin: Option<&str>, actor: &str, secs: u64, env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new("timeout");
    cmd.arg(secs.to_string()).arg(program).args(args).env("SPIRA_FAYTH", actor);
    for (k, v) in env {
        cmd.env(k, v);
    }
    spawn(cmd, stdin, program)
}

/// [`tool_env`] for a tool whose stderr is part of its answer even on success (mail's
/// "routed to the concierge" notice): nothing it says is dropped.
pub fn tool_env_noisy(program: &str, args: &[String], stdin: Option<&str>, actor: &str, secs: u64, env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new("timeout");
    cmd.arg(secs.to_string()).arg(program).args(args).env("SPIRA_FAYTH", actor);
    for (k, v) in env {
        cmd.env(k, v);
    }
    spawn_with(cmd, stdin, program, true)
}

fn spawn(cmd: Command, stdin: Option<&str>, what: &str) -> (i32, String) {
    spawn_with(cmd, stdin, what, false)
}

fn spawn_with(mut cmd: Command, stdin: Option<&str>, what: &str, keep_stderr: bool) -> (i32, String) {
    use std::io::Write;
    use std::process::Stdio;
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (2, format!("running {what}: {e}")),
    };
    if let Some(text) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            if let Err(e) = pipe.write_all(text.as_bytes()) {
                return (2, format!("writing to {what}: {e}"));
            }
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return (2, format!("waiting on {what}: {e}")),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    match out.status.code() {
        Some(0) if keep_stderr => (0, format!("{stdout}{}", String::from_utf8_lossy(&out.stderr))),
        Some(0) => (0, stdout),
        Some(c) => (c, format!("{stdout}{}", String::from_utf8_lossy(&out.stderr))),
        None => (2, format!("{what} was killed by a signal: {stdout}{}", String::from_utf8_lossy(&out.stderr))),
    }
}

#[cfg(test)]
mod noisy_tests {
    use super::*;

    fn sh(script: &str) -> (i32, String) {
        tool_env_noisy("sh", &["-c".to_string(), script.to_string()], None, "t", 10, &[])
    }

    #[test]
    fn a_refused_ask_exits_nonzero_with_the_reason() {
        let (code, out) = sh("echo 'mail: lint: unknown kind x' >&2; exit 1");
        assert_eq!(code, 1);
        assert!(out.contains("unknown kind x"), "{out}");
    }

    #[test]
    fn a_successful_ask_still_shows_the_stderr_notice() {
        let (code, out) = sh("echo 'mail: routed to the concierge' >&2");
        assert_eq!(code, 0);
        assert!(out.contains("routed to the concierge"), "{out}");
        assert!(!tool("sh", &["-c".to_string(), "echo hidden >&2".to_string()], None, "t", 10).1.contains("hidden"));
    }
}
