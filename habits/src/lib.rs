//! The habit hooks' dispatcher. Reads one Claude Code hook JSON, runs the repo's
//! `.spira/habits.toml` checks and answers with the findings on the lines the edit touched.
//! Feedback only: every failure path returns `None` and the binary exits 0.

mod config;
mod diff;
mod exec;
pub mod glob;
mod parse;
mod state;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;
use spira_otel::metrics::{MetricsSink, Point};

use config::{Config, Habit};
use exec::Ran;
use parse::Finding;

const MAX_LINES_PER_HABIT: usize = 3;

#[derive(Deserialize)]
struct Input {
    session_id: Option<String>,
    cwd: Option<String>,
    hook_event_name: Option<String>,
    tool_input: Option<ToolInput>,
}

#[derive(Deserialize)]
struct ToolInput {
    file_path: Option<String>,
}

enum Outcome {
    Findings(Vec<Finding>),
    Skipped(String),
}

pub fn dispatch(raw: &str, state_dir: &Path, sink: &MetricsSink) -> Option<String> {
    let input: Input = serde_json::from_str(raw).ok()?;
    let cwd = PathBuf::from(input.cwd.as_deref()?);
    let root = repo_root(&cwd);
    let session = input.session_id.clone().unwrap_or_else(|| "nosession".into());
    let mut st = state::load(state_dir, &session);
    let repo = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let cfg = match config::load(&root) {
        Ok(Some(c)) => c,
        Ok(None) => return None,
        Err(e) => {
            let msg = format!("habits: .spira/habits.toml is unreadable ({e}); no habits run");
            let first = st.warned.insert("config".into());
            state::save(state_dir, &session, &st);
            return first.then(|| answer(input.hook_event_name.as_deref(), &msg));
        }
    };
    let text = if input.hook_event_name.as_deref() == Some("Stop") {
        stop(&cfg, &root, &repo, &mut st, sink)
    } else {
        let file = input.tool_input.and_then(|t| t.file_path)?;
        edited(&cfg, &root, &cwd, &file, &repo, state_dir, &mut st, sink)
    };
    state::save(state_dir, &session, &st);
    text.map(|t| answer(input.hook_event_name.as_deref(), &t))
}

fn answer(event: Option<&str>, text: &str) -> String {
    match event {
        Some("Stop") => json!({ "systemMessage": text }),
        _ => json!({ "hookSpecificOutput": { "hookEventName": "PostToolUse", "additionalContext": text } }),
    }
    .to_string()
}

fn repo_root(cwd: &Path) -> PathBuf {
    Command::new("timeout")
        .args(["5", "git", "-C"])
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_else(|| cwd.to_path_buf())
}

#[allow(clippy::too_many_arguments)]
fn edited(cfg: &Config, root: &Path, cwd: &Path, file: &str, repo: &str, tmp: &Path, st: &mut state::State, sink: &MetricsSink) -> Option<String> {
    let abs = if Path::new(file).is_absolute() { PathBuf::from(file) } else { cwd.join(file) };
    let abs = abs.canonicalize().unwrap_or(abs);
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let rel = abs.strip_prefix(&root).ok()?.to_string_lossy().into_owned();
    let habits: Vec<&Habit> = cfg.habit.iter().filter(|h| glob::matches(&h.glob, &rel)).collect();
    if habits.is_empty() {
        return None;
    }
    let touched = diff::touched(&root, &rel);
    let results: Vec<(&Habit, Outcome, u64)> = std::thread::scope(|s| {
        let jobs: Vec<_> = habits
            .iter()
            .map(|h| {
                let (root, abs, rel, touched) = (&root, &abs, &rel, &touched);
                s.spawn(move || {
                    let t = Instant::now();
                    let o = if touched.is_empty() { Outcome::Findings(Vec::new()) } else { run_habit(h, root, abs, rel, touched, tmp) };
                    (*h, o, t.elapsed().as_millis() as u64)
                })
            })
            .collect();
        jobs.into_iter().filter_map(|j| j.join().ok()).collect()
    });
    let mut lines = Vec::new();
    for (h, outcome, ms) in results {
        match outcome {
            Outcome::Skipped(why) => {
                if st.warned.insert(format!("{}:{}", h.name, why)) {
                    lines.push(format!("habits: {} skipped ({why})", h.name));
                }
            }
            Outcome::Findings(found) => {
                let slot = format!("{}\0{}", h.name, rel);
                let prev = st.seen.get(&slot).cloned().unwrap_or_default();
                let now: std::collections::BTreeSet<String> = found.iter().map(|f| f.key.clone()).collect();
                let resolved = prev.difference(&now).count() as u64;
                let shown: Vec<&Finding> = found.iter().filter(|f| h.format() != "complexity" || !prev.contains(&f.key)).collect();
                st.seen.insert(slot, now);
                push(sink, repo, &h.name, found.len() as u64, resolved, ms);
                if !shown.is_empty() {
                    lines.push(format!("habits: {} found {} on lines you edited in {rel}", h.name, shown.len()));
                    lines.extend(shown.iter().take(MAX_LINES_PER_HABIT).map(|f| format!("  {}", f.text)));
                    if shown.len() > MAX_LINES_PER_HABIT {
                        lines.push(format!("  (+{} more)", shown.len() - MAX_LINES_PER_HABIT));
                    }
                }
            }
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn push(sink: &MetricsSink, repo: &str, habit: &str, findings: u64, resolved: u64, ms: u64) {
    sink.push(
        &[
            Point::Count { name: "habit_findings_total", value: findings },
            Point::Count { name: "habit_resolved_total", value: resolved },
            Point::Gauge { name: "habit_runtime_ms", value: ms as f64 },
        ],
        &[("repo", repo), ("habit", habit)],
    );
}

fn run_habit(h: &Habit, root: &Path, abs: &Path, rel: &str, touched: &diff::Ranges, tmp: &Path) -> Outcome {
    let file = abs.to_string_lossy();
    let deadline = Instant::now() + Duration::from_millis(h.budget_ms);
    let go = |argv: &[String]| exec::run(argv, root, deadline.saturating_duration_since(Instant::now()));
    if h.format() == "complexity" {
        return complexity(h, root, abs, rel, touched, tmp, &go);
    }
    match go(&config::argv(&h.run, &[("file", &file)])) {
        Ran::Missing => Outcome::Skipped("tool not found".into()),
        Ran::Timeout => Outcome::Skipped(format!("over its {} ms budget", h.budget_ms)),
        Ran::Out { stdout, stderr } => {
            let out = if stdout.is_empty() { stderr } else { stdout };
            let all = if h.format() == "rustfmt" { parse::rustfmt(&out, abs) } else { parse::gcc_findings(&out, abs) };
            Outcome::Findings(all.into_iter().filter(|f| diff::hits(touched, f.start, f.end)).collect())
        }
    }
}

fn complexity(h: &Habit, root: &Path, abs: &Path, rel: &str, touched: &diff::Ranges, tmp: &Path, go: &dyn Fn(&[String]) -> Ran) -> Outcome {
    let limit = h.limit() as f64;
    let argv_for = |p: &Path| config::argv(&h.run, &[("file", &p.to_string_lossy())]);
    let prog = argv_for(abs).into_iter().next().unwrap_or_default();
    let probe = [prog, "-f".into(), "ERROR".into(), "-p".into(), abs.to_string_lossy().into_owned()];
    match go(&probe) {
        Ran::Missing => return Outcome::Skipped("tool not found".into()),
        Ran::Timeout => return Outcome::Skipped(format!("over its {} ms budget", h.budget_ms)),
        Ran::Out { stdout, .. } if stdout.contains("ERROR:") => return Outcome::Findings(Vec::new()),
        Ran::Out { .. } => {}
    }
    let scored = |p: &Path| match go(&argv_for(p)) {
        Ran::Out { stdout, .. } => parse::functions(&stdout).ok_or_else(|| "output not understood".to_string()),
        Ran::Missing => Err("tool not found".into()),
        Ran::Timeout => Err(format!("over its {} ms budget", h.budget_ms)),
    };
    let after = match scored(abs) {
        Ok(f) => f,
        Err(why) => return Outcome::Skipped(why),
    };
    let before = diff::head_source(root, rel).and_then(|src| {
        let ext = abs.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let p = tmp.join(format!("before-{}-{}.{ext}", std::process::id(), h.name));
        let _ = std::fs::create_dir_all(tmp);
        std::fs::write(&p, src).ok()?;
        let r = scored(&p);
        let _ = std::fs::remove_file(&p);
        r.ok()
    });
    let mut found = Vec::new();
    for f in after.iter().filter(|f| f.cognitive > limit && diff::hits(touched, f.start, f.end)) {
        let prior = before.as_ref().and_then(|b| b.iter().filter(|g| g.name == f.name).map(|g| g.cognitive).reduce(f64::max));
        let text = match prior {
            None => format!("line {}: new fn {} has cognitive complexity {} (limit {})", f.start, f.name, f.cognitive, limit),
            Some(b) if b <= limit => format!("line {}: fn {} went from cognitive {} to {} (limit {}); split it", f.start, f.name, b, f.cognitive, limit),
            Some(_) => continue,
        };
        found.push(Finding { start: f.start, end: f.end, key: format!("fn {}", f.name), text });
    }
    Outcome::Findings(found)
}

fn stop(cfg: &Config, root: &Path, repo: &str, st: &mut state::State, sink: &MetricsSink) -> Option<String> {
    if cfg.stop.is_empty() || st.stop_done {
        return None;
    }
    st.stop_done = true;
    let changed = diff::changed_files(root);
    let mut crates: Vec<String> = changed.iter().filter(|f| f.ends_with(".rs")).filter_map(|f| crate_of(root, f)).collect();
    crates.sort();
    crates.dedup();
    let mut lines = Vec::new();
    for s in &cfg.stop {
        for krate in &crates {
            let t = Instant::now();
            let argv = config::argv(&s.run, &[("crate", krate)]);
            match exec::run(&argv, root, Duration::from_millis(s.budget_ms)) {
                Ran::Missing => lines.push(format!("habits: {} skipped (tool not found)", s.name)),
                Ran::Timeout => lines.push(format!("habits: {} skipped (over its {} ms budget)", s.name, s.budget_ms)),
                Ran::Out { stdout, stderr } => {
                    let out = format!("{stdout}\n{stderr}");
                    let mut ranges = std::collections::BTreeMap::new();
                    let found: Vec<String> = parse::gcc(&out)
                        .into_iter()
                        .filter(|(p, line, _)| {
                            let r = ranges.entry(p.clone()).or_insert_with(|| diff::touched(root, p));
                            diff::hits(r, *line, *line)
                        })
                        .map(|(p, line, msg)| format!("  {p}:{line}: {}", msg.chars().take(110).collect::<String>()))
                        .collect();
                    push(sink, repo, &s.name, found.len() as u64, 0, t.elapsed().as_millis() as u64);
                    if !found.is_empty() {
                        lines.push(format!("habits: {} on {krate} found {} on lines you changed", s.name, found.len()));
                        lines.extend(found.iter().take(MAX_LINES_PER_HABIT + 2).cloned());
                    }
                }
            }
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn crate_of(root: &Path, rel: &str) -> Option<String> {
    let mut dir = root.join(rel);
    while dir.pop() {
        let manifest = dir.join("Cargo.toml");
        if let Ok(t) = std::fs::read_to_string(&manifest) {
            if let Some(name) = t.parse::<toml::Table>().ok().and_then(|v| v.get("package")?.get("name")?.as_str().map(str::to_string)) {
                return Some(name);
            }
        }
        if dir == root {
            break;
        }
    }
    None
}
