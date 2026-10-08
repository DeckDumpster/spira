//! `sim probe <world>`: the `SIM_PROBE` command. Prints one `bead_state` row per bead, as JSONL,
//! built from the world's own lifecycle (`spira-lc list`, run against the world's store only),
//! its `run/landstate/` ledger, and the world repository's git.
//!
//! `bd_status` is left null: the world's bead content store is read through `bd`, which the
//! probe does not call, so `inv_closed_is_terminal` cannot fire on probe rows.

use crate::world::{is_world, lc_command, run, LANDING_BASE, PRODUCTION_LOCATORS};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CALL_DEADLINE: Duration = Duration::from_secs(60); // batch-job: one spira-lc list or git log against a world
const BRANCH_PREFIX: &str = "spira/";

/// The commits reachable from one ref, and their messages.
#[derive(Default)]
pub struct Lineage {
    pub shas: HashSet<String>,
    pub messages: Vec<String>,
}

impl Lineage {
    /// Parses `git log --format=%H%x00%B%x1e <ref>`.
    pub fn parse(log: &str) -> Lineage {
        let mut l = Lineage::default();
        for rec in log.split('\x1e') {
            if let Some((sha, body)) = rec.trim_start_matches('\n').split_once('\0') {
                l.shas.insert(sha.trim().to_string());
                l.messages.push(body.to_string());
            }
        }
        l
    }

    /// The tip is on this ref, or a commit on it names the bead.
    pub fn holds(&self, bead: &str, tip: Option<&str>) -> bool {
        tip.is_some_and(|t| self.shas.contains(t)) || self.messages.iter().any(|m| names(m, bead))
    }
}

/// Whether `text` names `bead` as a whole id: `sp-ab` is not named by `sp-abc` or `sp-ab.1`.
pub fn names(text: &str, bead: &str) -> bool {
    let b = text.as_bytes();
    let id_char = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
    text.match_indices(bead).any(|(i, _)| {
        let end = i + bead.len();
        let before = i == 0 || !id_char(b[i - 1]);
        let after = match b.get(end) {
            None => true,
            Some(b'.') => !b.get(end + 1).is_some_and(|c| c.is_ascii_alphanumeric()),
            Some(&c) => !id_char(c),
        };
        before && after
    })
}

/// Everything git says about the world, read once per snapshot.
#[derive(Default)]
pub struct GitFacts {
    /// `spira/<id>` branch tips, by bead id.
    pub branches: BTreeMap<String, String>,
    pub local_main: Lineage,
    pub origin_main: Lineage,
}

/// Parses `git for-each-ref --format='%(refname) %(objectname)' refs/heads/spira/`.
pub fn parse_branches(out: &str) -> BTreeMap<String, String> {
    out.lines()
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(r, sha)| r.strip_prefix("refs/heads/").and_then(|b| b.strip_prefix(BRANCH_PREFIX)).map(|id| (id.to_string(), sha.trim().to_string())))
        .collect()
}

fn scalar(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Holds as one sorted, comma-joined string; `spira-lc` prints them as a JSON array or as its text.
fn holds(v: Option<&Value>) -> String {
    let arr = match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let set: BTreeSet<String> = arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
    set.into_iter().collect::<Vec<_>>().join(",")
}

/// The `bead_state` rows: one per lifecycle row, and one per `spira/<id>` branch the lifecycle
/// does not know. `landstate` answers the ledger's state word for a bead, if it has one.
pub fn rows(lc_list: &str, landstate: &dyn Fn(&str) -> Option<String>, git: &GitFacts) -> Result<Vec<Value>, String> {
    let list: Value = serde_json::from_str(lc_list.trim()).map_err(|e| format!("spira-lc list: not JSON: {e}"))?;
    let list = list.as_array().ok_or("spira-lc list: not a JSON array")?;
    let mut lc: BTreeMap<String, &Value> = BTreeMap::new();
    for r in list {
        let id = scalar(r.get("bead_id")).ok_or_else(|| format!("spira-lc list: a row has no bead_id: {r}"))?;
        lc.insert(id, r);
    }
    let ids: BTreeSet<&String> = lc.keys().chain(git.branches.keys()).collect();
    let mut out = Vec::new();
    for id in ids {
        let r = lc.get(id);
        let field = |k: &str| r.and_then(|r| scalar(r.get(k)));
        let lc_tip = field("tip");
        let branch_tip = git.branches.get(id).cloned();
        let version = match field("version") {
            Some(v) => Some(v.trim().parse::<i64>().map_err(|_| format!("spira-lc list: {id} has version {v:?}"))?),
            None => None,
        };
        let tip = lc_tip.as_deref().or(branch_tip.as_deref());
        out.push(json!({
            "bead": id,
            "bd_status": null,
            "lc_state": field("state"),
            "lc_version": version,
            "holds": r.map(|r| holds(r.get("holds"))),
            "landstate": landstate(id),
            "lc_tip": lc_tip,
            "branch_tip": branch_tip,
            "on_local_main": git.local_main.holds(id, tip),
            "on_origin_main": git.origin_main.holds(id, tip),
        }));
    }
    Ok(out)
}

/// Refuses unless `world` is a sim world and every production locator set in `env` points
/// inside it: the world's own `sim.env` sets `SPIRA_RUN` to `<world>/run`, which is fine,
/// and anything else is production (or some other store) and is refused.
fn resolve(p: &Path) -> PathBuf {
    let mut lexical = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            std::path::Component::CurDir => {}
            c => lexical.push(c),
        }
    }
    let mut tail = Vec::new();
    let mut head = lexical.as_path();
    loop {
        if let Ok(real) = head.canonicalize() {
            return tail.iter().rev().fold(real, |acc, c| acc.join(c));
        }
        match (head.parent(), head.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_owned());
                head = parent;
            }
            _ => return lexical,
        }
    }
}

pub fn refuse_foreign(world: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    if !is_world(world) {
        return Err(format!("{} is not a sim world", world.display()));
    }
    let world = world.canonicalize().map_err(|e| format!("{}: {e}", world.display()))?;
    let foreign: Vec<String> = PRODUCTION_LOCATORS
        .iter()
        .filter_map(|k| env(k).filter(|v| !v.is_empty()).map(|v| (k, v)))
        .filter(|(_, v)| {
            !resolve(Path::new(v)).starts_with(&world)
        })
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if foreign.is_empty() {
        Ok(world)
    } else {
        Err(format!("refusing to probe while a production locator points outside the world: {}", foreign.join(", ")))
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    run(Command::new("git").arg("-C").arg(repo).args(args), CALL_DEADLINE)
}

fn lineage(repo: &Path, r: &str) -> Result<Lineage, String> {
    Ok(Lineage::parse(&git(repo, &["log", "--format=%H%x00%B%x1e", r, "--"])?))
}

pub fn git_facts(world: &Path) -> Result<GitFacts, String> {
    let work = world.join("work");
    Ok(GitFacts {
        branches: parse_branches(&git(&work, &["for-each-ref", "--format=%(refname) %(objectname)", &format!("refs/heads/{BRANCH_PREFIX}")])?),
        local_main: lineage(&work, &format!("refs/heads/{LANDING_BASE}"))?,
        origin_main: lineage(&world.join("origin.git"), "refs/heads/main")?,
    })
}

/// The world's private Dolt server port: `<db.fixture>/server.port`.
pub fn lc_port(world: &Path) -> Result<String, String> {
    let fixture = std::fs::read_to_string(world.join("db.fixture")).map_err(|e| format!("{}: no db.fixture: {e}", world.display()))?;
    let port = std::fs::read_to_string(Path::new(fixture.trim()).join("server.port")).map_err(|e| format!("fixture has no server.port: {e}"))?;
    Ok(port.trim().to_string())
}

fn landstate_of(world: &Path) -> impl Fn(&str) -> Option<String> {
    let dir = world.join("run/landstate");
    move |id: &str| std::fs::read_to_string(dir.join(id)).ok().and_then(|s| s.split_whitespace().next().map(str::to_string))
}

/// The probe: JSONL, one `bead_state` row per line.
pub fn probe(world: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let world = refuse_foreign(world, env)?;
    let port = lc_port(&world)?;
    let list = run(lc_command(&world.join("release"), &world.join("config"), &port).arg("list"), CALL_DEADLINE).map_err(|e| format!("spira-lc list: {e}"))?;
    let rows = rows(&list, &landstate_of(&world), &git_facts(&world)?)?;
    let mut out = String::new();
    for r in rows {
        out.push_str(&r.to_string());
        out.push('\n');
    }
    Ok(out)
}
