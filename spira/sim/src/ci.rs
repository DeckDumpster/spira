//! `sim ci run`: a hosted runner for the `run:` steps of one workflow job, against sim gh. The
//! steps are the workflow file's own text, unmodified; what differs from GitHub is only what a
//! runner provides: a clean environment, `gh` and `unzip` shims, a `cargo` that "builds" the
//! one package the cut needs by copying it from the world's release, and the `GITHUB_*` files.

use crate::world::run_capture;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const STEP_DEADLINE: Duration = Duration::from_secs(120); // batch-job: one workflow step against a world
const SYSTEM_PATH: &str = "/usr/bin:/bin";
pub const CARGO_FROM_ENV: &str = "SIM_CARGO_FROM";
const UNZIP: &str = "#!/bin/sh\nexec python3 -m zipfile -e \"$2\" \"$4\"\n";

#[derive(Debug, PartialEq)]
pub struct Step {
    pub name: String,
    pub env: Vec<(String, String)>,
    /// `None` for a step that does not run a script (`uses:`).
    pub run: Option<String>,
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner.to_string();
        }
    }
    v.to_string()
}

/// The steps of `job`, in order. Only the layout the workflows here use is read: a step starts
/// `      - name:`, its keys sit at eight spaces, an `env:` map and a `run: |` block at ten.
pub fn steps(yml: &str, job: &str) -> Result<Vec<Step>, String> {
    let lines: Vec<&str> = yml.lines().collect();
    let head = format!("  {job}:");
    let start = lines.iter().position(|l| *l == head).ok_or_else(|| format!("no job {job:?}"))?;
    let end = lines[start + 1..].iter().position(|l| indent(l) <= 2 && !l.trim().is_empty() && !l.trim_start().starts_with('#')).map_or(lines.len(), |i| start + 1 + i);
    let body = &lines[start + 1..end];
    let first = body.iter().position(|l| *l == "    steps:").ok_or_else(|| format!("job {job:?} has no steps"))?;
    let mut out: Vec<Step> = Vec::new();
    let mut i = first + 1;
    while i < body.len() {
        let l = body[i];
        i += 1;
        if let Some(name) = l.strip_prefix("      - name:") {
            out.push(Step { name: unquote(name), env: Vec::new(), run: None });
            continue;
        }
        let Some(cur) = out.last_mut() else { continue };
        if l == "        env:" {
            while i < body.len() && (body[i].trim().is_empty() || indent(body[i]) >= 10) {
                let e = body[i];
                i += 1;
                if indent(e) == 10 && !e.trim_start().starts_with('#') {
                    if let Some((k, v)) = e.trim().split_once(':') {
                        cur.env.push((k.to_string(), unquote(v)));
                    }
                }
            }
        } else if l.starts_with("        run: |") {
            let mut script = Vec::new();
            while i < body.len() && (body[i].trim().is_empty() || indent(body[i]) >= 10) {
                script.push(body[i].get(10..).unwrap_or(""));
                i += 1;
            }
            cur.run = Some(script.join("\n") + "\n");
        } else if let Some(one) = l.strip_prefix("        run: ") {
            cur.run = Some(format!("{}\n", unquote(one)));
        }
    }
    if out.is_empty() {
        return Err(format!("job {job:?} has no named steps"));
    }
    Ok(out)
}

pub struct Context<'a> {
    pub sha: &'a str,
    pub workspace: &'a Path,
}

/// Replaces every `${{ ... }}` the runner knows; any other expression is refused rather than left
/// for the shell to read as a literal.
pub fn expand(text: &str, cx: &Context) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("${{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 3..];
        let j = after.find("}}").ok_or("unterminated ${{ expression")?;
        let value = match after[..j].trim() {
            "github.sha" => cx.sha.to_string(),
            "github.workspace" => cx.workspace.display().to_string(),
            "github.head_ref" => String::new(),
            "github.repository" => "sim/sim".to_string(),
            "steps.app-tok.outputs.token" | "secrets.GITHUB_TOKEN" => "sim-token".to_string(),
            other => return Err(format!("the sim runner does not know the expression ${{{{ {other} }}}}")),
        };
        out.push_str(&value);
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn read_lines(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
}

pub struct Job<'a> {
    pub steps: &'a [Step],
    pub skip: &'a [String],
    pub cx: Context<'a>,
    pub scratch: &'a Path,
    pub gh_dir: &'a Path,
    pub release_bin: &'a Path,
    /// Directories already on the runner's PATH before any step, as a runner image has its tools.
    pub path: &'a [PathBuf],
    /// Variables already set in the runner's environment before any step, as its image sets them.
    pub env: &'a [(String, String)],
}

/// Runs each script step in order, stopping at the first that fails; its exit code, or 0.
pub fn run_job(job: &Job, sim: &Path, mut log: impl FnMut(&str)) -> Result<i32, String> {
    let shim = job.scratch.join("shim");
    std::fs::create_dir_all(&shim).map_err(|e| e.to_string())?;
    for tool in ["gh", "cargo"] {
        let at = shim.join(tool);
        let _ = std::fs::remove_file(&at);
        std::os::unix::fs::symlink(sim, &at).map_err(|e| format!("{}: {e}", at.display()))?;
    }
    write_exe(&shim.join("unzip"), UNZIP)?;
    let (gh_path, gh_env, gh_out) = (job.scratch.join("github_path"), job.scratch.join("github_env"), job.scratch.join("github_output"));
    for f in [&gh_path, &gh_env, &gh_out] {
        std::fs::write(f, "").map_err(|e| e.to_string())?;
    }
    let home = job.scratch.join("home");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let mut path_front: Vec<String> = job.path.iter().rev().map(|p| p.display().to_string()).collect();
    let mut extra_env: Vec<(String, String)> = Vec::new();
    for step in job.steps {
        let Some(run) = &step.run else { continue };
        if job.skip.contains(&step.name) {
            log(&format!("--- skipped: {}\n", step.name));
            continue;
        }
        let script_file = job.scratch.join("step.sh");
        std::fs::write(&script_file, expand(run, &job.cx)?).map_err(|e| e.to_string())?;
        let path = path_front.iter().map(String::as_str).chain([shim.to_str().ok_or("shim path")?, SYSTEM_PATH]).collect::<Vec<_>>().join(":");
        let mut cmd = Command::new("/bin/bash");
        cmd.arg("-e").arg(&script_file).current_dir(job.cx.workspace).env_clear().env("PATH", path).env("HOME", &home);
        cmd.env("CI", "true").env("GITHUB_ACTIONS", "true").env("GITHUB_WORKSPACE", job.cx.workspace).env("GITHUB_SHA", job.cx.sha);
        cmd.env("GITHUB_REPOSITORY", "sim/sim").env("GITHUB_PATH", &gh_path).env("GITHUB_ENV", &gh_env).env("GITHUB_OUTPUT", &gh_out);
        cmd.env("RUNNER_TEMP", job.scratch).env(crate::gh::STATE_ENV, job.gh_dir).env(CARGO_FROM_ENV, job.release_bin);
        for (k, v) in job.env {
            cmd.env(k, v);
        }
        let mut step_env = Vec::new();
        for (k, v) in &step.env {
            step_env.push((k.clone(), expand(v, &job.cx)?));
        }
        for (k, v) in extra_env.iter().chain(&step_env) {
            cmd.env(k, v);
        }
        log(&format!("--- step: {}\n", step.name));
        let started = std::time::Instant::now();
        let (status, out, err) = run_capture(&mut cmd, STEP_DEADLINE)?;
        log(&format!("{out}{err}--- {}: {status} in {}ms\n", step.name, started.elapsed().as_millis()));
        if !status.success() {
            return Ok(status.code().unwrap_or(1));
        }
        for p in read_lines(&gh_path) {
            if !path_front.contains(&p) {
                path_front.insert(0, p);
            }
        }
        for l in read_lines(&gh_env) {
            if let Some((k, v)) = l.split_once('=') {
                extra_env.retain(|(n, _)| n != k);
                extra_env.push((k.to_string(), v.to_string()));
            }
        }
    }
    Ok(0)
}

fn write_exe(p: &Path, body: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, body).map_err(|e| format!("{}: {e}", p.display()))?;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())
}

/// `cargo build [--release] [--locked] -p spira-config`, answered by copying the world's release
/// binary into `./target/release`. A hosted runner builds; a world has no toolchain and no minutes.
pub fn cargo_shim(args: &[String], cwd: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let mut it = args.iter();
    if it.next().map(String::as_str) != Some("build") {
        return Err("the sim cargo answers only `build`".into());
    }
    let mut pkgs = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--release" | "--locked" => {}
            "-p" => pkgs.push(it.next().ok_or("-p needs a package")?.clone()),
            other => return Err(format!("the sim cargo does not take {other}")),
        }
    }
    if pkgs != ["spira-config"] {
        return Err(format!("the sim cargo builds only spira-config, not {pkgs:?}"));
    }
    let from = env(CARGO_FROM_ENV).filter(|v| !v.is_empty()).ok_or_else(|| format!("{CARGO_FROM_ENV} is not set: the sim cargo answers only inside a sim runner"))?;
    let src = PathBuf::from(from).join("spira-config");
    let dest = cwd.join("target/release");
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    std::fs::copy(&src, dest.join("spira-config")).map_err(|e| format!("{}: {e}", src.display()))?;
    Ok(())
}
