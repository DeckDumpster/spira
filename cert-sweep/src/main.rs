//! cert-sweep — continuous certification of the landing ref's tip (DESIGN.md).
//!
//!   cert-sweep pass --mode full|subset --tree DIR [--repo DIR] [--base REF] [--run DIR]
//!                   [--subset-div N] [--maxpar N] [--priority N] [--max-beads N]
//!                   [--branch REF] [--repo-name NAME] [--reruns N]
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

use cert_sweep::{
    bead_body, bead_title, bisect, culprit_body, culprit_title, filing_kind, first_fail_line, judge, open_duplicate, parse_members, parse_result_file,
    parse_testenv_stdout, pick_subset, record, red_body, Culprit, Event, Judgement, Member, Outcome, Row, Verdict, EVENT_FAMILY, FAMILY,
};
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
    let rt = Rt { f, run: &run, repo: &repo, mode, seq: std::cell::Cell::new(0) };

    let outcomes = rt.run(&tip, &picks, start)?.0;
    if outcomes.is_empty() {
        let e = Event::SweepFault { mode: mode.into(), round, why: "the runner returned no results".into() };
        emit(&run, &e);
        return Ok(ExitCode::from(1));
    }

    let _lock = lock_history(&run)?;
    let history = read_history(&run);
    let (rows, events) = record(&history, &outcomes, &tip, &round, mode);

    let mut held: Vec<&str> = Vec::new();
    let mut extra: Vec<Row> = Vec::new();
    let mut filed = 0u64;
    let (max_beads, priority) = (num(f, "max-beads", 5)?, num(f, "priority", 1)?);
    for e in &events {
        let suite = e.suite().unwrap_or_default();
        let mut e = e.clone();
        let mut filing = bead_title(&e).map(|t| (t, bead_body(&e)));
        if matches!(e, Event::NewRed { .. } | Event::RedUnbounded { .. }) {
            match rt.attribute(&e, &tip, &round) {
                Ok(a) => {
                    extra.extend(a.rows);
                    e = a.event;
                    filing = a.filing;
                }
                Err(err) => {
                    eprintln!("cert-sweep: {suite}: {err}; held for the next pass");
                    held.push(suite);
                    continue;
                }
            }
        }
        if let (Some((title, body)), Some(kind)) = (&filing, filing_kind(&e)) {
            if filed >= max_beads {
                eprintln!("cert-sweep: {suite}: over the {max_beads}-bead bound; held for the next pass");
                held.push(suite);
                continue;
            }
            let dup = rt.open_beads().map(|open| open_duplicate(&open, suite, kind).map(str::to_string));
            match dup {
                Err(err) => {
                    eprintln!("cert-sweep: {suite}: {err}; held for the next pass");
                    held.push(suite);
                    continue;
                }
                Ok(Some(id)) => println!("{suite}: {id} is already open; not filed again"),
                Ok(None) => match file_bead(title, body, priority, &run) {
                    Ok(id) => {
                        filed += 1;
                        println!("filed {id} for {suite}");
                    }
                    Err(err) => {
                        eprintln!("cert-sweep: {suite}: {err}; held for the next pass");
                        held.push(suite);
                        continue;
                    }
                },
            }
        }
        emit(&run, &e);
    }
    // A suite whose bead was not filed is left out of the history, so the next pass sees the
    // same transition and tries again.
    for r in rows.iter().chain(&extra).filter(|r| !held.contains(&r.suite.as_str())) {
        append(&run, FAMILY, &r.fields())?;
    }
    let count = |v| outcomes.iter().filter(|o| o.verdict == v).count();
    emit(&run, &Event::Summary { mode: mode.into(), round, ran: outcomes.len(), red: count(cert_sweep::Verdict::Red), fault: count(cert_sweep::Verdict::Fault), wall_secs: now() - start });
    Ok(ExitCode::SUCCESS)
}

struct Attribution {
    event: Event,
    filing: Option<(String, String)>,
    rows: Vec<Row>,
}

struct Rt<'a> {
    f: &'a Flags,
    run: &'a Path,
    repo: &'a str,
    mode: &'a str,
    seq: std::cell::Cell<u64>,
}

impl Rt<'_> {
    /// The pass's own runner, so a rerun sees the environment the red was seen in.
    fn run(&self, commit: &str, picks: &[String], start: u64) -> Result<(Vec<Outcome>, String), String> {
        if self.mode == "full" {
            run_on_vm(self.f, self.run, self.repo, commit, picks, start, self.seq.replace(self.seq.get() + 1))
        } else {
            run_on_host(self.f, self.repo, commit, picks)
        }
    }

    fn probe(&self, commit: &str, suite: &str) -> Option<(Verdict, String)> {
        let (out, text) = self.run(commit, &[suite.to_string()], now()).ok()?;
        let v = out.iter().find(|o| o.suite == suite)?.verdict;
        matches!(v, Verdict::Ok | Verdict::Red).then_some((v, text))
    }

    fn open_beads(&self) -> Result<Vec<(String, String)>, String> {
        let db = std::env::var("SPIRA_DB").ok().filter(|s| !s.is_empty()).ok_or("SPIRA_DB is required to look for an open duplicate")?;
        let bd = std::env::var("SPIRA_BD").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "bd".into());
        let out = Command::new(&bd)
            .args(["-C", &db, "list", "--status", "open,in_progress,blocked,deferred", "--limit", "0", "--brief", "--json"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{bd}: {e}"))?;
        if !out.status.success() {
            return Err(format!("{bd} list: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("{bd} list: {e}"))?;
        let rows = v.as_array().ok_or_else(|| format!("{bd} list: not a JSON array"))?;
        Ok(rows
            .iter()
            .filter_map(|r| Some((r.get("id")?.as_str()?.to_string(), r.get("title")?.as_str()?.to_string())))
            .collect())
    }

    /// A red is rerun on its own commit: a green among the reruns is a flip. A red that
    /// reproduces is run at the last green commit and bisected through the window's members.
    fn attribute(&self, e: &Event, tip: &str, round: &str) -> Result<Attribution, String> {
        let (suite, bound) = match e {
            Event::NewRed { suite, last_green_commit, .. } => (suite.as_str(), Some(last_green_commit.as_str())),
            Event::RedUnbounded { suite, .. } => (suite.as_str(), None),
            _ => return Err("not a red".into()),
        };
        let mk = |verdict, mode: &str| Row { commit: tip.into(), round: round.into(), suite: suite.into(), verdict, secs: None, mode: mode.into() };
        let mut verdicts = Vec::new();
        let mut text = String::new();
        for _ in 0..num(self.f, "reruns", 3)? {
            if let Some((v, t)) = self.probe(tip, suite) {
                if v == Verdict::Red && text.is_empty() {
                    text = t;
                }
                verdicts.push(v);
            }
        }
        let rows: Vec<Row> = verdicts.iter().map(|&v| mk(v, "rerun")).collect();
        match judge(&verdicts) {
            Judgement::Inconclusive => Err("no rerun produced a verdict".into()),
            Judgement::Flaky => {
                let ev = Event::Flip { suite: suite.into(), round: round.into(), commit: tip.into() };
                Ok(Attribution { filing: bead_title(&ev).map(|t| (t, bead_body(&ev))), event: ev, rows })
            }
            Judgement::Reproducible => {
                let fail = first_fail_line(&text);
                let plain = |note: &str| Attribution { filing: bead_title(e).map(|t| (t, red_body(e, fail.as_deref(), note))), event: e.clone(), rows: rows.clone() };
                let Some(green) = bound else { return Ok(plain("Reproducible, and no green round is known to bisect from.")) };
                match self.probe(green, suite) {
                    Some((Verdict::Ok, _)) => {}
                    Some(_) => return Ok(plain("Also red at the last green round when rerun: the history's green does not hold here, so no window can be named.")),
                    None => return Err("the last green round gave no verdict".into()),
                }
                let log = git(self.repo, &["log", "--reverse", "--format=%H%x09%s", &format!("{green}..{tip}")])?;
                let members = parse_members(&log);
                let Some(i) = bisect(members.len(), |i| self.probe(&members[i].commit, suite).map(|(v, _)| v == Verdict::Red)) else {
                    return Ok(plain("Reproducible; the bisect over the window's members could not finish."));
                };
                let member: Member = members[i].clone();
                let culprit_round = git(self.repo, &["for-each-ref", "--contains", &member.commit, "--sort=committerdate", "--format=%(refname)", "refs/archive/rounds"])
                    .ok()
                    .and_then(|r| r.lines().next().and_then(|l| l.rsplit('/').next().map(str::to_string)))
                    .unwrap_or_else(|| round.to_string());
                let c = Culprit { suite: suite.into(), round: culprit_round, member, first_fail: fail };
                Ok(Attribution { filing: Some((culprit_title(&c), culprit_body(&c, e))), event: e.clone(), rows })
            }
        }
    }
}

fn run_on_vm(f: &Flags, run: &Path, repo: &str, tip: &str, picks: &[String], start: u64, seq: u64) -> Result<(Vec<Outcome>, String), String> {
    let tree = need(f, "tree")?;
    if Path::new(tree).join(".git").exists() {
        git(tree, &["checkout", "-q", "--detach", tip])?;
    } else {
        git(repo, &["worktree", "add", "-q", "--detach", tree, tip])?;
    }
    let rd = run.join("cert-sweep").join(format!("{start}-full-{seq}"));
    fs::create_dir_all(&rd).map_err(|e| format!("{}: {e}", rd.display()))?;
    let log = rd.with_extension("log");
    let st = Command::new("round-vm")
        .args(["run", tree, "--maxpar", &num(f, "maxpar", 16)?.to_string(), "--suites", &picks.join(","), "--results-dir"])
        .arg(&rd)
        .stdout(fs::File::create(&log).map_err(|e| e.to_string())?)
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("round-vm: {e}"))?;
    eprintln!("cert-sweep: round-vm run exited {:?}", st.code());
    Ok((results_in(&rd), fs::read_to_string(&log).unwrap_or_default()))
}

fn run_on_host(f: &Flags, repo: &str, tip: &str, picks: &[String]) -> Result<(Vec<Outcome>, String), String> {
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
    Ok((parse_testenv_stdout(&out), out))
}
