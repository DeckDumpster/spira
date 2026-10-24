//! Once a day, `release acceptance` phase A runs against a fresh worktree of the landing ref.
//! A green run records its commit and verdict and files nothing. A red run files one builder
//! bead per distinct failing check (at most `MAX_BEADS`), appending to the bead already open
//! for that check. A run that never produced a verdict is an alarm, never a quiet day.

use std::collections::BTreeMap;

pub mod real;
#[cfg(test)]
mod tests;

pub const MAX_BEADS: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Pass,
    Fail(Vec<String>),
    NotRun(String),
}

/// `acceptance-local.sh` exits 0 on a pass, 1 on a failed phase and 2 (or a timeout) when no run happened.
pub fn verdict(code: i32, out: &str) -> Verdict {
    match code {
        0 => Verdict::Pass,
        1 => {
            let mut lines: Vec<String> = out.lines().map(str::trim).filter(|l| l.starts_with("FAIL ")).map(String::from).collect();
            if lines.is_empty() {
                lines.push("FAIL  (the run exited 1 and printed no FAIL line)".into());
            }
            Verdict::Fail(lines)
        }
        c => Verdict::NotRun(format!("acceptance-local.sh exited {c}: {}", tail(out, 20))),
    }
}

fn tail(out: &str, n: usize) -> String {
    let lines: Vec<&str> = out.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The check a `FAIL  <check>: <reason>` line names; the reason varies between days, the check does not.
pub fn failure_key(line: &str) -> String {
    let rest = line.trim().strip_prefix("FAIL").unwrap_or(line).trim();
    let name = rest.split(':').next().unwrap_or(rest);
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Failing lines grouped by check in order of appearance, folded into `MAX_BEADS` groups.
pub fn groups(lines: &[String]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for l in lines {
        let k = failure_key(l);
        match out.iter_mut().find(|(g, _)| *g == k) {
            Some((_, v)) => v.push(l.clone()),
            None => out.push((k, vec![l.clone()])),
        }
    }
    while out.len() > MAX_BEADS {
        let (_, extra) = out.pop().unwrap();
        out.last_mut().unwrap().1.extend(extra);
    }
    out
}

pub fn title(key: &str, commit: &str) -> String {
    format!("fresh-install acceptance fails at {key:?} on {}", short(commit))
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

fn report(commit: &str, lines: &[String], forensics: &str) -> String {
    let fails: String = lines.iter().map(|l| format!("    {l}\n")).collect();
    format!("Commit: {commit}\nForensics: {forensics}\n\nFailing lines, verbatim from the run:\n\n{fails}")
}

pub fn new_bead_body(commit: &str, lines: &[String], forensics: &str) -> String {
    format!(
        "The daily fresh-install acceptance run (acceptance-local.sh, no predecessor) failed against the landing ref.\n\n{}\n## Build\n\nFix the failure above in this repo and land it; `acceptance-local.sh <tree>` reproduces the run and the next daily run confirms it.\n",
        report(commit, lines, forensics)
    )
}

pub fn append_body(day: &str, commit: &str, lines: &[String], forensics: &str) -> String {
    format!("## Still failing on {day}\n\n{}", report(commit, lines, forensics))
}

pub trait World {
    /// True while a landing round holds a gate-worker slot on this host.
    fn round_busy(&self) -> bool;
    fn sleep(&self);
    fn day(&self) -> String;
    /// The commit the landing ref names.
    fn head(&self) -> Result<String, String>;
    /// Checks `commit` out into a fresh worktree; `release_tree` removes it.
    fn prepare_tree(&self, commit: &str) -> Result<(), String>;
    fn release_tree(&self);
    fn forensics(&self, commit: &str) -> String;
    /// Runs the acceptance on the prepared tree: exit code and combined output.
    fn run(&self, forensics: &str) -> (i32, String);
    fn is_open(&self, id: &str) -> Result<bool, String>;
    fn file(&self, title: &str, body: &str) -> Result<String, String>;
    fn append(&self, id: &str, body: &str, note: &str) -> Result<(), String>;
    fn record(&self, commit: &str, v: &Verdict);
}

/// The bead each failing check is filed under.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct State {
    pub beads: BTreeMap<String, String>,
}

impl State {
    pub fn parse(raw: &str) -> State {
        let mut s = State::default();
        for l in raw.lines() {
            if let ["bead", key, id] = l.split('\t').collect::<Vec<_>>().as_slice() {
                s.beads.insert(key.to_string(), id.to_string());
            }
        }
        s
    }

    pub fn render(&self) -> String {
        self.beads.iter().map(|(k, i)| format!("bead\t{k}\t{i}\n")).collect()
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub commit: String,
    pub verdict: Option<Verdict>,
    pub filed: Vec<String>,
    pub amended: Vec<String>,
    /// Each names what stopped the day's verdict or its filing.
    pub failed: Vec<String>,
}

pub fn pass(w: &dyn World, state: &mut State, save: &mut dyn FnMut(&State) -> Result<(), String>, max_waits: u32) -> Outcome {
    let mut o = Outcome::default();
    let mut waited = 0;
    while w.round_busy() {
        if waited >= max_waits {
            o.failed.push("a landing round was still certifying when the wait ran out; no acceptance run today".into());
            return o;
        }
        w.sleep();
        waited += 1;
    }
    let commit = match w.head() {
        Ok(c) => c,
        Err(e) => {
            o.failed.push(format!("cannot read the landing ref: {e}"));
            return o;
        }
    };
    o.commit = commit.clone();
    if let Err(e) = w.prepare_tree(&commit) {
        o.failed.push(format!("cannot prepare the worktree for {}: {e}", short(&commit)));
        return o;
    }
    let forensics = w.forensics(&commit);
    let (code, out) = w.run(&forensics);
    w.release_tree();
    let v = verdict(code, &out);
    w.record(&commit, &v);
    match &v {
        Verdict::Pass => {}
        Verdict::NotRun(why) => o.failed.push(format!("acceptance could not run on {}: {why}", short(&commit))),
        Verdict::Fail(lines) => file_reds(w, &commit, lines, &forensics, state, save, &mut o),
    }
    o.verdict = Some(v);
    o
}

fn file_reds(w: &dyn World, commit: &str, lines: &[String], forensics: &str, state: &mut State, save: &mut dyn FnMut(&State) -> Result<(), String>, o: &mut Outcome) {
    for (key, group) in groups(lines) {
        let r = match state.beads.get(&key) {
            Some(id) if w.is_open(id).unwrap_or(false) => {
                let id = id.clone();
                w.append(&id, &append_body(&w.day(), commit, &group, forensics), &format!("acceptance-daily: still failing on {}", short(commit))).map(|_| o.amended.push(id))
            }
            _ => w.file(&title(&key, commit), &new_bead_body(commit, &group, forensics)).and_then(|id| {
                state.beads.insert(key.clone(), id.clone());
                o.filed.push(id);
                save(state)
            }),
        };
        if let Err(e) = r {
            o.failed.push(format!("{key}: {e}"));
        }
    }
}
