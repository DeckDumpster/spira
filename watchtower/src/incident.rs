//! The one seam every check and the sweep use to hand a finding to `incident.sh` — out of
//! this bead's scope (DESIGN.md §3), called exactly as the bash called it: `bash <inc> file
//! <title> -` with the body on stdin and the same `SPIRA_INCIDENT_*` environment.

use std::io::Write;
use std::process::{Command, Stdio};

/// Resolves `$SPIRA_INCIDENT_SH`, falling back to `incident.sh` on PATH — the same
/// `${SPIRA_INCIDENT_SH:-$(command -v incident.sh)}` every bash call site used.
pub fn resolve(env_override: Option<String>) -> Option<String> {
    if let Some(p) = env_override {
        if !p.is_empty() {
            return Some(p);
        }
    }
    which("incident.sh")
}

/// `command -v <name>` — a PATH lookup, nothing more.
pub fn which(name: &str) -> Option<String> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// True when the resolved path exists and is either executable or readable — the bash's
/// `[ -x "$INC" ] || [ -r "$INC" ]` guard, checked right before a call, never cached.
pub fn is_usable(path: &str) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode();
    mode & 0o111 != 0 || mode & 0o444 != 0
}

#[derive(Debug, Clone, Default)]
pub struct Finding {
    pub db: String,
    pub incident_type: String,
    pub priority: u32,
    pub actor: String,
    pub sin_exempt: bool,
    pub repo: String,
    pub reference: Option<String>,
    pub cause: Option<String>,
    pub delivers_action: bool,
    pub title: String,
    pub body: String,
}

impl Finding {
    pub fn new(db: &str, repo: &str, title: &str, body: &str) -> Self {
        Finding {
            db: db.to_string(),
            incident_type: "task".to_string(),
            priority: 2,
            actor: "watchtower".to_string(),
            sin_exempt: true,
            repo: repo.to_string(),
            reference: None,
            cause: None,
            delivers_action: false,
            title: title.to_string(),
            body: body.to_string(),
        }
    }
    pub fn priority(mut self, p: u32) -> Self {
        self.priority = p;
        self
    }
    pub fn reference(mut self, r: impl Into<String>) -> Self {
        self.reference = Some(r.into());
        self
    }
    pub fn cause(mut self, c: impl Into<String>) -> Self {
        self.cause = Some(c.into());
        self
    }
    pub fn delivers_action(mut self) -> Self {
        self.delivers_action = true;
        self
    }
}

/// Runs `bash <incident_sh> file <title> -`, body on stdin, discarding stdout — the exact
/// shape of `bash "$INC" file "..." - >/dev/null || true` at every bash call site: a failed
/// filing is logged by the caller, never fatal to the check that found the thing.
pub fn file(incident_sh: &str, f: &Finding) -> bool {
    let mut cmd = Command::new("bash");
    cmd.arg(incident_sh).arg("file").arg(&f.title).arg("-");
    cmd.env("SPIRA_DB", &f.db);
    cmd.env("SPIRA_INCIDENT_TYPE", &f.incident_type);
    cmd.env("SPIRA_INCIDENT_PRIORITY", f.priority.to_string());
    cmd.env("SPIRA_INCIDENT_ACTOR", &f.actor);
    if f.sin_exempt {
        cmd.env("SPIRA_SIN_EXEMPT", "1");
    }
    cmd.env("SPIRA_INCIDENT_REPO", &f.repo);
    if let Some(r) = &f.reference {
        cmd.env("SPIRA_INCIDENT_REF", r);
    }
    if let Some(c) = &f.cause {
        cmd.env("SPIRA_INCIDENT_CAUSE", c);
    }
    if f.delivers_action {
        cmd.env("SPIRA_INCIDENT_DELIVERS", "action");
    }
    let child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(f.body.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_the_override() {
        assert_eq!(
            resolve(Some("/x/incident.sh".to_string())),
            Some("/x/incident.sh".to_string())
        );
    }

    #[test]
    fn resolve_falls_back_to_path_lookup_when_override_is_empty() {
        // An empty override (unset env var) must not short-circuit to Some("").
        let r = resolve(Some(String::new()));
        assert_ne!(r, Some(String::new()));
    }

    #[test]
    fn is_usable_true_for_a_readable_file() {
        let d = testkit::TempDir::new("wt-inc");
        let p = d.join("inc.sh");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        assert!(is_usable(p.to_str().unwrap()));
    }

    #[test]
    fn is_usable_false_for_a_missing_path() {
        assert!(!is_usable("/does/not/exist/incident.sh"));
    }

    #[test]
    fn file_writes_the_env_and_stdin_a_fake_incident_sh_expects() {
        let d = testkit::TempDir::new("wt-inc-file");
        let inc = d.join("inc.sh");
        let capture = d.join("capture.txt");
        std::fs::write(
            &inc,
            format!(
                "#!/usr/bin/env bash\n{{ echo \"$1\"; echo \"$2\"; echo \"REF=$SPIRA_INCIDENT_REF\"; echo \"PRI=$SPIRA_INCIDENT_PRIORITY\"; cat; }} > {}\n",
                capture.display()
            ),
        )
        .unwrap();
        let f = Finding::new("db", "spira", "TITLE HERE", "the body\nsecond line\n")
            .priority(1)
            .reference("incident:x-1");
        assert!(file(inc.to_str().unwrap(), &f));
        let got = std::fs::read_to_string(&capture).unwrap();
        assert!(got.contains("file\nTITLE HERE\nREF=incident:x-1\nPRI=1\nthe body\nsecond line"));
    }
}
