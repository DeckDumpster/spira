//! Shells to `bd` for the non-lifecycle half of the aeon semantic layer (design §3.5):
//! titles, descriptions, dependencies, notes and filing stay bd's job (§3.3/3.4) — this
//! module never touches `spira_lifecycle`. Resolved the same way the harness's shell code
//! resolves them, `SPIRA_BD`/`SPIRA_DB`, read directly since a Rust binary cannot source
//! `conf.sh`.

use std::process::Command;

fn bd_bin() -> String {
    std::env::var("SPIRA_BD").unwrap_or_else(|_| "bd".to_string())
}

/// `spira-lc serve` runs as a long-lived service with no reason to expect any particular
/// working directory; a bare relative path like `spira/bead.sh` would resolve differently
/// (or not at all) depending on how it was launched. Resolved against `SPIRA_REPO` (the
/// same variable the harness's own shell code is rooted at) unless a caller overrides the
/// binary/script path outright.
fn repo_relative(env_override: &str, default_default: &str) -> String {
    if let Ok(p) = std::env::var(env_override) {
        return p;
    }
    let repo = std::env::var("SPIRA_REPO").unwrap_or_else(|_| ".".to_string());
    format!("{repo}/{default_default}")
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

pub fn note(bead_id: &str, text: &str) -> Result<String, String> {
    run(&["note", bead_id, text])
}

/// Files through `spira/bead.sh`'s contract (the labelling rules `bd create` alone does not
/// enforce), with `--parent` set by this layer, never the caller — see bead.sh's own
/// `--parent` doc for why an inherited `branch:` label made `groomer.sh split-piece` a
/// two-step dance that this avoids by filing with the right labels from the start.
pub fn file_child(title: &str, persona: &str, repo: &str, parent: &str) -> Result<String, String> {
    let bead_sh = repo_relative("SPIRA_BEAD_SH", "spira/bead.sh");
    let out = Command::new(&bead_sh)
        .args(["file", title, "--for", persona, "--repo", repo, "--parent", parent])
        .output()
        .map_err(|e| format!("running {bead_sh}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(stdout.trim().to_string())
}

/// `mail.sh send operator` — an ask carrying the channel back (design §3.5's `work
/// blocked`, and the supersede-by confirmation request). `--bead` ties the tracking
/// decision bead mail.sh files to the one this layer is bound to.
pub fn ask_operator(from: &str, subject: &str, default: &str, bead_id: &str, body: &str) -> Result<String, String> {
    let mail_sh = repo_relative("SPIRA_MAIL_SH", "spira/mail.sh");
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
