//! `sim probe <world>`: the `SIM_PROBE` command. Prints one `bead_state` row per bead, as JSONL,
//! built from the world's own lifecycle (`spira-lc list`, run against the world's store only),
//! its `run/landstate/` ledger, and the world repository's git.
//!
//! The bead store's columns (`bd_status`, title, priority, labels, deps, close reason, body) are
//! left null here; a scenario that seeds beads has them merged in from `bd list` by `merge_bd`,
//! so `inv_closed_is_terminal` can fire only on such a scenario.

use crate::world::{is_world, lc_command, run, LANDING_BASE, PRODUCTION_LOCATORS};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CALL_DEADLINE: Duration = Duration::from_secs(60); // batch-job: one spira-lc list or git log against a world
const BRANCH_PREFIX: &str = "spira/";

/// The commits reachable from one ref, and their messages.
#[derive(Default, Clone)]
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
    /// The landing and origin lineages of the world's other repositories, by repository name;
    /// a repository that does not land on a local branch has its origin trunk as its landing lineage.
    pub others: BTreeMap<String, RepoLineages>,
}

#[derive(Default, Clone)]
pub struct RepoLineages {
    pub local_main: Lineage,
    pub origin_main: Lineage,
}

/// A repository of the world beyond its own, as its registry row names it.
pub struct ExtraRepo {
    pub name: String,
    pub work: PathBuf,
    pub origin: PathBuf,
    /// The trunk branch, and whether the repository lands on a local branch (`queue.local`).
    pub trunk: String,
    pub local: bool,
}

/// The registry rows after the world's own.
pub fn extra_repos(world: &Path) -> Vec<ExtraRepo> {
    let rows = std::fs::read_to_string(world.join("home/registry-rows")).unwrap_or_default();
    rows.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('|').collect();
            let (name, path, mode, base) = (f.first()?, f.get(1)?, f.get(2)?, f.get(3)?);
            let work = PathBuf::from(path);
            let origin = work.parent()?.join("origin.git");
            Some(ExtraRepo { name: name.to_string(), work, origin, trunk: base.rsplit('/').next()?.to_string(), local: *mode == "queue.local" })
        })
        .collect()
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
        let mains = match field("repo").and_then(|n| git.others.get(&n)) {
            Some(o) => (&o.local_main, &o.origin_main),
            None => (&git.local_main, &git.origin_main),
        };
        out.push(json!({
            "bead": id,
            "bd_status": null,
            "lc_state": field("state"),
            "lc_version": version,
            "holds": r.map(|r| holds(r.get("holds"))),
            "landstate": landstate(id),
            "lc_tip": lc_tip,
            "branch_tip": branch_tip,
            "on_local_main": mains.0.holds(id, tip),
            "on_origin_main": mains.1.holds(id, tip),
        }));
    }
    Ok(out)
}

/// Folds `bd list --json` into `bead_state` rows: the bead store's columns for a bead the rows
/// already carry, and a row of its own for a bead only the store knows.
pub fn merge_bd(beads: &mut Vec<Value>, bd_list: &str) -> Result<(), String> {
    let list: Value = serde_json::from_str(bd_list.trim()).map_err(|e| format!("bd list: not JSON: {e}"))?;
    let list = list.as_array().ok_or("bd list: not a JSON array")?;
    for r in list {
        let id = scalar(r.get("id")).ok_or_else(|| format!("bd list: a row has no id: {r}"))?;
        let strings = |k: &str| -> Vec<String> { r.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default() };
        let joined = |mut v: Vec<String>| {
            v.sort();
            v.join(",")
        };
        let deps = joined(r.get("dependencies").and_then(Value::as_array).map(|a| a.iter().filter_map(|d| d["depends_on_id"].as_str().map(str::to_string)).collect()).unwrap_or_default());
        let at = match beads.iter().position(|b| b["bead"] == id.as_str()) {
            Some(i) => i,
            None => {
                beads.push(crate::trace::bead_row(&json!({ "bead": id }))?);
                beads.len() - 1
            }
        };
        let row = beads[at].as_object_mut().ok_or("bead row is not an object")?;
        row.insert("bd_status".into(), scalar(r.get("status")).map_or(Value::Null, Value::String));
        row.insert("title".into(), scalar(r.get("title")).map_or(Value::Null, Value::String));
        row.insert("priority".into(), r.get("priority").and_then(Value::as_i64).map_or(Value::Null, |p| json!(p)));
        row.insert("labels".into(), json!(joined(strings("labels"))));
        row.insert("deps".into(), json!(deps));
        row.insert("close_reason".into(), scalar(r.get("close_reason")).map_or(Value::Null, Value::String));
        row.insert("body".into(), scalar(r.get("description")).map_or(Value::Null, Value::String));
    }
    Ok(())
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

/// Whether the world's own repository lands on its origin's trunk (every mode but `queue.local`),
/// so `GitFacts::local_main` is the lineage of the ref its beads land on.
pub fn lands_on_origin(world: &Path) -> bool {
    let rows = std::fs::read_to_string(world.join("home/registry-rows")).unwrap_or_default();
    rows.lines().next().and_then(|l| l.split('|').nth(2)).is_some_and(|mode| mode != "queue.local")
}

pub fn git_facts(world: &Path) -> Result<GitFacts, String> {
    let work = world.join("work");
    let mut branches = parse_branches(&git(&work, &["for-each-ref", "--format=%(refname) %(objectname)", &format!("refs/heads/{BRANCH_PREFIX}")])?);
    let mut others = BTreeMap::new();
    for x in extra_repos(world) {
        branches.extend(parse_branches(&git(&x.work, &["for-each-ref", "--format=%(refname) %(objectname)", &format!("refs/heads/{BRANCH_PREFIX}")])?));
        let origin_main = lineage(&x.origin, &format!("refs/heads/{}", x.trunk))?;
        let local_main = if x.local { lineage(&x.work, &format!("refs/heads/local/{}", x.trunk))? } else { origin_main.clone() };
        others.insert(x.name.clone(), RepoLineages { local_main, origin_main });
    }
    let landing = if lands_on_origin(world) { lineage(&world.join("origin.git"), "refs/heads/main")? } else { lineage(&work, &format!("refs/heads/{LANDING_BASE}"))? };
    Ok(GitFacts {
        branches,
        others,
        local_main: landing,
        origin_main: lineage(&world.join("origin.git"), "refs/heads/main")?,
    })
}

/// The world's private Dolt server port: `<db.fixture>/server.port`.
pub fn lc_port(world: &Path) -> Result<String, String> {
    let fixture = std::fs::read_to_string(world.join("db.fixture")).map_err(|e| format!("{}: no db.fixture: {e}", world.display()))?;
    let port = std::fs::read_to_string(Path::new(fixture.trim()).join("server.port")).map_err(|e| format!("fixture has no server.port: {e}"))?;
    Ok(port.trim().to_string())
}

pub fn landstate_of(world: &Path) -> impl Fn(&str) -> Option<String> {
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

/// Every ref of a repository's git dir, as `git for-each-ref` lists them (name order, a
/// symbolic ref as what it points at), read from the files git keeps them in.
pub fn read_refs(git_dir: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, String>, symbolic: &mut Vec<(String, String)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = format!("{prefix}{}", e.file_name().to_string_lossy());
            let path = e.path();
            if path.is_dir() {
                walk(&path, &format!("{name}/"), out, symbolic);
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                let text = text.trim();
                match text.strip_prefix("ref: ") {
                    Some(target) => symbolic.push((name, target.to_string())),
                    None if !text.is_empty() && !name.ends_with(".lock") => {
                        out.insert(name, text.to_string());
                    }
                    None => {}
                }
            }
        }
    }
    let mut refs = BTreeMap::new();
    let mut symbolic = Vec::new();
    for l in std::fs::read_to_string(git_dir.join("packed-refs")).unwrap_or_default().lines() {
        if let Some((sha, name)) = l.split_once(' ').filter(|(_, n)| n.starts_with("refs/")) {
            refs.insert(name.to_string(), sha.to_string());
        }
    }
    walk(&git_dir.join("refs"), "refs/", &mut refs, &mut symbolic);
    for (name, target) in symbolic {
        if let Some(sha) = refs.get(&target).cloned() {
            refs.insert(name, sha);
        }
    }
    refs
}

/// The probe without its processes: refs read from git's files, the lifecycle asked over its
/// socket, and a commit's history read once however many snapshots see it.
pub struct Observer {
    world: PathBuf,
    on_origin: bool,
    lineages: HashMap<(PathBuf, String), Lineage>,
}

pub struct Observed {
    pub refs: Vec<(String, String)>,
    pub tags: Vec<String>,
    pub rows: Vec<Value>,
}

impl Observer {
    pub fn new(world: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<Self, String> {
        let world = refuse_foreign(world, env)?;
        Ok(Observer { on_origin: lands_on_origin(&world), world, lineages: HashMap::new() })
    }

    fn lineage(&mut self, repo: &Path, sha: Option<&String>) -> Result<Lineage, String> {
        let Some(sha) = sha else { return Err(format!("{} has no ref to read", repo.display())) };
        let key = (repo.to_path_buf(), sha.clone());
        if !self.lineages.contains_key(&key) {
            self.lineages.insert(key.clone(), lineage(repo, sha)?);
        }
        Ok(self.lineages[&key].clone())
    }

    pub fn observe(&mut self) -> Result<Observed, String> {
        let work = read_refs(&self.world.join("work/.git"));
        let origin = read_refs(&self.world.join("origin.git"));
        let (home_work, home_origin) = (self.world.join("work"), self.world.join("origin.git"));
        let heads = |refs: &BTreeMap<String, String>| -> BTreeMap<String, String> { refs.iter().filter_map(|(r, sha)| r.strip_prefix(&format!("refs/heads/{BRANCH_PREFIX}")).map(|id| (id.to_string(), sha.clone()))).collect() };
        let mut branches = heads(&work);
        let mut others = BTreeMap::new();
        for x in extra_repos(&self.world) {
            let (xwork, xorigin) = (read_refs(&x.work.join(".git")), read_refs(&x.origin));
            branches.extend(heads(&xwork));
            let origin_main = self.lineage(&x.origin, xorigin.get(&format!("refs/heads/{}", x.trunk)))?;
            let local_main = if x.local { self.lineage(&x.work, xwork.get(&format!("refs/heads/local/{}", x.trunk)))? } else { origin_main.clone() };
            others.insert(x.name.clone(), RepoLineages { local_main, origin_main });
        }
        let landing = if self.on_origin { self.lineage(&home_origin, origin.get("refs/heads/main"))? } else { self.lineage(&home_work, work.get(&format!("refs/heads/{LANDING_BASE}")))? };
        let git = GitFacts { branches, others, local_main: landing, origin_main: self.lineage(&home_origin, origin.get("refs/heads/main"))? };
        let socket = crate::world::lc_socket(&self.world);
        let (code, list) = crate::world::ask_body(&socket, &["list"]).ok_or_else(|| format!("spira-lc list: no answer on {}", socket.display()))?;
        if code != 0 {
            return Err(format!("spira-lc list: exit {code}: {}", list.trim()));
        }
        let tags = |refs: &BTreeMap<String, String>| refs.keys().filter_map(|r| r.strip_prefix("refs/tags/").map(str::to_string)).collect::<Vec<_>>();
        let mut all_tags = tags(&work);
        all_tags.extend(tags(&origin));
        Ok(Observed {
            refs: work.into_iter().collect(),
            tags: all_tags,
            rows: rows(&list, &landstate_of(&self.world), &git)?,
        })
    }
}
