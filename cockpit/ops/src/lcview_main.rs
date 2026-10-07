//! `lc-view once|loop [secs]` — the lifecycle-lens ops pane (sp-lpw5ol). Gathers from the built
//! tools only: `spira-lc list` (state), `work list --json` (titles, priority), the landing ref's
//! commits (drift), `world status`, and the aeon ceiling from config. Never runs `bd`.

use cockpit_ops::lcview::{own_ids, render, Meta, Row, Snapshot};
use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new("timeout")
        .arg("30")
        .arg(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{cmd} {}: exit {} {}", args.join(" "), out.status.code().unwrap_or(-1), err.lines().last().unwrap_or("")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn num(v: &serde_json::Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn gather() -> Snapshot {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let mut s = Snapshot { now, ..Default::default() };
    s.release = std::env::var("SPIRA_RELEASE")
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "?".into());

    match run("spira-lc", &["list"]).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| format!("spira-lc list: {e}"))) {
        Ok(v) => {
            for r in v.as_array().cloned().unwrap_or_default() {
                let holds: Vec<String> = r["holds"].as_str().and_then(|h| serde_json::from_str(h).ok()).unwrap_or_default();
                s.rows.push(Row {
                    id: r["bead_id"].as_str().unwrap_or("").into(),
                    state: r["state"].as_str().unwrap_or("").into(),
                    holder: r["holder"].as_str().map(String::from),
                    holds,
                    reason: r["reason"].as_str().map(String::from),
                    updated_at: num(&r["updated_at"]).unwrap_or(0),
                    since: num(&r["since"]).or_else(|| num(&r["updated_at"])).unwrap_or(0),
                    lease_until: num(&r["lease_until"]),
                });
            }
        }
        Err(e) => s.errors.push(e),
    }
    match run("work", &["list", "--json"]).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| format!("work list: {e}"))) {
        Ok(v) => {
            for b in v.as_array().cloned().unwrap_or_default() {
                if let Some(id) = b["id"].as_str() {
                    s.meta.insert(id.into(), Meta { title: b["title"].as_str().unwrap_or("").into(), priority: num(&b["priority"]) });
                }
            }
        }
        Err(e) => s.errors.push(e),
    }
    let (root, base) = (run("spira-config", &["repo", "root", "spira"]), run("spira-config", &["repo", "base", "spira"]));
    match (root, base) {
        (Ok(root), Ok(base)) => {
            s.base = base.trim().into();
            match run("git", &["-C", root.trim(), "log", "--format=%H %s", "-n", "4000", &format!("refs/heads/{}", s.base)]) {
                Ok(log) => {
                    let mut m = HashMap::new();
                    for l in log.lines() {
                        if let Some((h, subj)) = l.split_once(' ') {
                            for id in own_ids(subj) {
                                m.entry(id).or_insert_with(|| h.to_string());
                            }
                        }
                    }
                    s.on_base = m;
                }
                Err(e) => s.errors.push(e),
            }
        }
        (Err(e), _) | (_, Err(e)) => s.errors.push(e),
    }
    s.world = run("world", &["status"]).map(|t| t.lines().find(|l| l.contains("plane work")).unwrap_or("?").to_string()).unwrap_or_else(|e| {
        s.errors.push(e);
        "?".into()
    });
    s.ceiling = spira_config::process::cfg("SPIRA_MAX_LIVE_AEONS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    s
}

fn width() -> usize {
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(120)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("loop") => {
            let secs: u64 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(10);
            loop {
                let frame = render(&gather(), width());
                print!("\x1b[H\x1b[2J{}\n", frame.join("\n"));
                std::thread::sleep(Duration::from_secs(secs));
            }
        }
        Some("once") | None => println!("{}", render(&gather(), width()).join("\n")),
        Some(other) => {
            eprintln!("usage: lc-view once|loop [secs] (unknown: {other})");
            std::process::exit(2);
        }
    }
}
