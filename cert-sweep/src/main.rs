//! cert-sweep — continuous certification of the landing ref's tip (DESIGN.md).
//!
//!   cert-sweep pass --mode full|subset --tree DIR [--repo DIR] [--base REF] [--run DIR]
//!                   [--subset-div N] [--maxpar N] [--priority N] [--max-beads N]
//!                   [--branch REF] [--repo-name NAME]
//!   cert-sweep seed --results-dir DIR --commit SHA --round LABEL [--run DIR]
//!
//! `pass` runs the suites on the tip, records every verdict, and raises at most one event
//! per suite going red, flipping or losing its verdict. `seed` records a known-green run as
//! history so the first red has a last-green to point at.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use cert_sweep::{bead_body, bead_title, parse_result_file, parse_testenv_stdout, pick_subset, record, Event, Outcome, Row, EVENT_FAMILY, FAMILY};
use serde_json::Value;

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}
const LOCK_EX: i32 = 2;

const USAGE: &str = "usage: cert-sweep pass --mode full|subset --tree DIR [--repo DIR] [--base REF] [--run DIR] [--subset-div N] [--maxpar N] [--priority N] [--max-beads N] [--branch REF] [--repo-name NAME]\n   or: cert-sweep seed --results-dir DIR --commit SHA --round LABEL [--run DIR]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flags = match parse_flags(&args[args.len().min(1)..]) {
        Ok(f) => f,
        Err(e) => return usage(&e),
    };
    let r = match args.first().map(String::as_str) {
        Some("pass") => pass(&flags),
        Some("seed") => seed(&flags),
        _ => return usage("unknown command"),
    };
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cert-sweep: {e}");
            ExitCode::from(2)
        }
    }
}

fn usage(why: &str) -> ExitCode {
    eprintln!("cert-sweep: {why}\n{USAGE}");
    ExitCode::from(2)
}

type Flags = BTreeMap<String, String>;

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut m = Flags::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let k = a.strip_prefix("--").ok_or_else(|| format!("unexpected argument {a:?}"))?;
        let v = it.next().ok_or_else(|| format!("--{k} needs a value"))?;
        m.insert(k.to_string(), v.clone());
    }
    Ok(m)
}

fn flag<'a>(f: &'a Flags, k: &str) -> Option<&'a str> {
    f.get(k).map(String::as_str)
}

fn need<'a>(f: &'a Flags, k: &str) -> Result<&'a str, String> {
    flag(f, k).ok_or_else(|| format!("--{k} is required"))
}

fn num(f: &Flags, k: &str, default: u64) -> Result<u64, String> {
    flag(f, k).map_or(Ok(default), |v| v.parse().map_err(|_| format!("--{k} {v:?} is not a number")))
}

fn run_dir(f: &Flags) -> Result<PathBuf, String> {
    flag(f, "run").map(str::to_string).or_else(|| std::env::var("SPIRA_RUN").ok()).filter(|s| !s.is_empty()).map(PathBuf::from).ok_or("--run or SPIRA_RUN is required".into())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn host() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "unknown".into())
}

fn git(repo: &str, args: &[&str]) -> Result<String, String> {
    let o = Command::new("git").arg("-C").arg(repo).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !o.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn read_history(run: &Path) -> Vec<Row> {
    fs::read_to_string(tsd::family_path(run, FAMILY)).map(|t| t.lines().filter_map(Row::from_json).collect()).unwrap_or_default()
}

fn append(run: &Path, family: &str, fields: &[(String, Value)]) -> Result<(), String> {
    let path = tsd::family_path(run, family);
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| format!("{}: {e}", path.display()))?;
    let line = tsd::build_row(&tsd::iso_utc(now()), &host(), family, fields)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    unsafe { flock(f.as_raw_fd(), LOCK_EX) };
    f.write_all(format!("{line}\n").as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

fn lock_history(run: &Path) -> Result<fs::File, String> {
    let p = tsd::family_path(run, FAMILY).with_extension("lock");
    fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let f = OpenOptions::new().create(true).append(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    unsafe { flock(f.as_raw_fd(), LOCK_EX) };
    Ok(f)
}

fn emit(run: &Path, e: &Event) {
    println!("{}", e.render());
    let mut f = vec![("kind".to_string(), Value::String(e.kind().into())), ("text".to_string(), Value::String(e.render()))];
    if let Some(s) = e.suite() {
        f.push(("suite".to_string(), Value::String(s.into())));
    }
    if let Err(err) = append(run, EVENT_FAMILY, &f) {
        eprintln!("cert-sweep: event row not written: {err}");
    }
}

/// The id comes from the JSON bead.sh prints, never from scanning its prose.
fn file_bead(title: &str, body: &str, priority: u64, run: &Path) -> Result<String, String> {
    let dir = run.join("tmp");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!("cert-sweep-{}-{}.txt", std::process::id(), now()));
    fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = Command::new("bead.sh")
        .envs(spira_config::release_env::child_path_env_for_process())
        .args(["file", title, "--for", "builder", "--repo", "spira", "--priority", &priority.to_string(), "--json", "--body-file"])
        .arg(&tmp)
        .output();
    let _ = fs::remove_file(&tmp);
    let out = out.map_err(|e| format!("bead.sh: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        return Err(format!("bead.sh file: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let start = text.find(['{', '[']).ok_or("bead.sh file: no JSON in output")?;
    let v: Value = serde_json::from_str(&text[start..]).map_err(|e| format!("bead.sh file: {e}"))?;
    let v = if let Value::Array(a) = v { a.into_iter().next().unwrap_or(Value::Null) } else { v };
    v.get("id").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "bead.sh file: no id in output".into())
}

fn results_in(dir: &Path) -> Vec<Outcome> {
    let mut v: Vec<Outcome> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let suite = name.strip_suffix(".result")?.to_string();
            parse_result_file(&suite, &fs::read_to_string(e.path()).ok()?)
        })
        .collect();
    v.sort_by(|a, b| a.suite.cmp(&b.suite));
    v
}

fn seed(f: &Flags) -> Result<ExitCode, String> {
    let run = run_dir(f)?;
    let results = results_in(Path::new(need(f, "results-dir")?));
    if results.is_empty() {
        return Err("no .result files in --results-dir".into());
    }
    let _lock = lock_history(&run)?;
    let (rows, _) = record(&[], &results, need(f, "commit")?, need(f, "round")?, "seed");
    for r in &rows {
        append(&run, FAMILY, &r.fields())?;
    }
    println!("seeded {} results", rows.len());
    Ok(ExitCode::SUCCESS)
}

fn pass(f: &Flags) -> Result<ExitCode, String> {
    let run = run_dir(f)?;
    let mode = need(f, "mode")?;
    if mode != "full" && mode != "subset" {
        return Err("--mode must be full or subset".into());
    }
    let repo = flag(f, "repo").map(str::to_string).or_else(|| std::env::var("SPIRA_REPO").ok()).ok_or("--repo or SPIRA_REPO is required")?;
    let base = flag(f, "base").unwrap_or("local/main");
    let tip = git(&repo, &["rev-parse", base])?;
    let round = git(&repo, &["for-each-ref", "--points-at", &tip, "--format=%(refname:short)", "refs/archive/rounds"])?
        .lines()
        .next()
        .and_then(|r| r.rsplit('/').next())
        .unwrap_or("?")
        .to_string();
    let all: Vec<String> = git(&repo, &["ls-tree", "--name-only", &tip, "spira/"])?
        .lines()
        .filter_map(|p| p.strip_prefix("spira/"))
        .filter(|n| n.starts_with("test-") && n.ends_with(".sh"))
        .map(str::to_string)
        .collect();
    let picks = if mode == "full" { all } else { pick_subset(&all, num(f, "subset-div", 4)? as usize, now() ^ u64::from(std::process::id()) << 32) };
    let start = now();

    let outcomes = if mode == "full" { run_on_vm(f, &run, &tip, &picks, start)? } else { run_on_host(f, &repo, &tip, &picks)? };
    if outcomes.is_empty() {
        let e = Event::SweepFault { mode: mode.into(), round, why: "the runner returned no results".into() };
        emit(&run, &e);
        return Ok(ExitCode::from(1));
    }

    let _lock = lock_history(&run)?;
    let history = read_history(&run);
    let (rows, events) = record(&history, &outcomes, &tip, &round, mode);

    let mut held: Vec<&str> = Vec::new();
    let mut filed = 0u64;
    let (max_beads, priority) = (num(f, "max-beads", 5)?, num(f, "priority", 1)?);
    for e in &events {
        if let Some(title) = bead_title(e) {
            let suite = e.suite().unwrap_or_default();
            if filed >= max_beads {
                eprintln!("cert-sweep: {suite}: over the {max_beads}-bead bound; held for the next pass");
                held.push(suite);
                continue;
            }
            match file_bead(&title, &bead_body(e), priority, &run) {
                Ok(id) => {
                    filed += 1;
                    println!("filed {id} for {suite}");
                }
                Err(err) => {
                    eprintln!("cert-sweep: {suite}: {err}; held for the next pass");
                    held.push(suite);
                    continue;
                }
            }
        }
        emit(&run, e);
    }
    // A suite whose bead was not filed is left out of the history, so the next pass sees the
    // same transition and tries again.
    for r in rows.iter().filter(|r| !held.contains(&r.suite.as_str())) {
        append(&run, FAMILY, &r.fields())?;
    }
    let count = |v| outcomes.iter().filter(|o| o.verdict == v).count();
    emit(&run, &Event::Summary { mode: mode.into(), round, ran: outcomes.len(), red: count(cert_sweep::Verdict::Red), fault: count(cert_sweep::Verdict::Fault), wall_secs: now() - start });
    Ok(ExitCode::SUCCESS)
}

fn run_on_vm(f: &Flags, run: &Path, tip: &str, picks: &[String], start: u64) -> Result<Vec<Outcome>, String> {
    let tree = need(f, "tree")?;
    if Path::new(tree).join(".git").exists() {
        git(tree, &["checkout", "-q", "--detach", tip])?;
    } else {
        let repo = flag(f, "repo").map(str::to_string).or_else(|| std::env::var("SPIRA_REPO").ok()).ok_or("--repo or SPIRA_REPO is required")?;
        git(&repo, &["worktree", "add", "-q", "--detach", tree, tip])?;
    }
    let rd = run.join("cert-sweep").join(format!("{start}-full"));
    fs::create_dir_all(&rd).map_err(|e| format!("{}: {e}", rd.display()))?;
    let st = Command::new("round-vm")
        .args(["run", tree, "--maxpar", &num(f, "maxpar", 16)?.to_string(), "--suites", &picks.join(","), "--results-dir"])
        .arg(&rd)
        .stdout(fs::File::create(rd.with_extension("log")).map_err(|e| e.to_string())?)
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("round-vm: {e}"))?;
    eprintln!("cert-sweep: round-vm run exited {:?}", st.code());
    Ok(results_in(&rd))
}

fn run_on_host(f: &Flags, repo: &str, tip: &str, picks: &[String]) -> Result<Vec<Outcome>, String> {
    let branch = flag(f, "branch").unwrap_or("cert-sweep/tip");
    git(repo, &["branch", "-f", branch, tip])?;
    let mut child = Command::new("testenv")
        .args(["--suites", "-", branch, flag(f, "repo-name").unwrap_or("spira")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("testenv: {e}"))?;
    child.stdin.take().unwrap().write_all(picks.join("\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).map_err(|e| e.to_string())?;
    let st = child.wait().map_err(|e| e.to_string())?;
    eprintln!("cert-sweep: testenv exited {:?}", st.code());
    Ok(parse_testenv_stdout(&out))
}
