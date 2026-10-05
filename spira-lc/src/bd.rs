//! Shells to `bd` for the non-lifecycle half of the aeon semantic layer (design §3.5):
//! titles, descriptions, dependencies, notes and filing stay bd's job (§3.3/3.4) — this
//! module never touches `spira_lifecycle`. Resolved the same way the harness's shell code
//! resolves them, `SPIRA_BD`/`SPIRA_DB`, read directly since a Rust binary cannot source
//! `conf.sh`.

use std::process::Command;

fn bd_bin() -> String {
    std::env::var("SPIRA_BD").unwrap_or_else(|_| "bd".to_string())
}

fn run(args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(bd_bin());
    if let Ok(db) = std::env::var("SPIRA_DB") {
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
    fn unclaim(&mut self, id: &str, actor: &str) -> Result<(), String> {
        run(&["unclaim", id, "--if-assignee", actor]).map(|_| ())
    }
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
        run(&["close", id, "--reason", reason]).map(|_| ())
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
        .env(spira_config::LIFECYCLE_ENFORCE_ENV, "0")
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
    let mut cmd = Command::new(bd_bin());
    if let Ok(db) = std::env::var("SPIRA_DB") {
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
/// the broker's own identity is never the persona that asked. `enforce_off` sets the
/// lifecycle switch off in the child, exactly as [`file_child`] does for `bead.sh`, for a
/// tool whose lifecycle half this broker performs itself. Returns the tool's own exit code
/// (stdout; stdout plus stderr when it failed).
pub fn tool(program: &str, args: &[String], stdin: Option<&str>, actor: &str, enforce_off: bool, secs: u64) -> (i32, String) {
    let mut cmd = Command::new("timeout");
    cmd.arg(secs.to_string()).arg(program).args(args).env("SPIRA_FAYTH", actor);
    if enforce_off {
        cmd.env(spira_config::LIFECYCLE_ENFORCE_ENV, "0");
    }
    spawn(cmd, stdin, program)
}

fn spawn(mut cmd: Command, stdin: Option<&str>, what: &str) -> (i32, String) {
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
        Some(0) => (0, stdout),
        Some(c) => (c, format!("{stdout}{}", String::from_utf8_lossy(&out.stderr))),
        None => (2, format!("{what} was killed by a signal: {stdout}{}", String::from_utf8_lossy(&out.stderr))),
    }
}
