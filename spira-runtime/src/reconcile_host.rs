//! The reconciler's reads, in this process. Systemd answers over D-Bus from one snapshot per
//! pass, disk usage comes from statvfs, the store over the lc socket, the forge over HTTP, the
//! runes and the repo map from `spira_config`. A read with no in-process form is not here.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reconciler::{Host, Mail, Ref};
use runtime::socket::lc_call;
use spira_config::repos::Registry;
use spira_config::runes;
use unit_run::dbus::{bus_path, Conn, Val};

use crate::forge::{slug, Forge};
use crate::world::World;

const SYSTEMD: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
/// A landing ref changes when its repository map row does, or when someone deletes the branch; either way a stale answer is wrong for at most this long.
const LANDREF_TTL: Duration = Duration::from_secs(600);

/// Landing refs by repo name, kept across passes: resolving one runs git.
#[derive(Default)]
pub struct Landrefs(Mutex<BTreeMap<String, (Instant, String)>>);

struct Units {
    active: BTreeMap<String, String>,
    files: BTreeMap<String, String>,
}

fn read_units(bus: Option<&str>) -> Result<Units, String> {
    let mut conn = Conn::connect(&bus.map_or_else(bus_path, |b| Ok(b.to_string()))?)?;
    let mut active = BTreeMap::new();
    let listed = conn.call(SYSTEMD, MANAGER_PATH, MANAGER, "ListUnits", None)?;
    let Some(Val::Arr(units)) = listed.first() else { return Err("ListUnits: unexpected reply".into()) };
    for u in units {
        if let Val::Struct(f) = u {
            if let (Some(name), Some(state)) = (f.first().and_then(Val::as_str), f.get(3).and_then(Val::as_str)) {
                active.insert(name.to_string(), state.to_string());
            }
        }
    }
    let mut files = BTreeMap::new();
    let listed = conn.call(SYSTEMD, MANAGER_PATH, MANAGER, "ListUnitFiles", None)?;
    let Some(Val::Arr(entries)) = listed.first() else { return Err("ListUnitFiles: unexpected reply".into()) };
    for e in entries {
        if let Val::Struct(f) = e {
            if let (Some(path), Some(state)) = (f.first().and_then(Val::as_str), f.get(1).and_then(Val::as_str)) {
                files.insert(path.rsplit('/').next().unwrap_or(path).to_string(), state.to_string());
            }
        }
    }
    Ok(Units { active, files })
}

pub struct RuntimeHost {
    pub world: World,
    pub forge: Forge,
    pub mail: Option<Arc<mail::env::Env>>,
    pub home: PathBuf,
    pub runes: String,
    pub stranded_min_age: u64,
    pub now: u64,
    /// The installer's enabled set; fixed for the life of the process, so computed once.
    pub enabled: String,
    pub landrefs: Arc<Landrefs>,
    /// The user manager's bus socket; `None` finds it from the environment.
    pub bus: Option<String>,
    units: OnceLock<Result<Units, String>>,
}

impl RuntimeHost {
    #[allow(clippy::too_many_arguments)]
    pub fn new(world: World, forge: Forge, mail: Option<Arc<mail::env::Env>>, home: PathBuf, runes: String, stranded_min_age: u64, now: u64, enabled: String, landrefs: Arc<Landrefs>) -> RuntimeHost {
        RuntimeHost { world, forge, mail, home, runes, stranded_min_age, now, enabled, landrefs, bus: None, units: OnceLock::new() }
    }

    fn units(&self) -> Result<&Units, String> {
        self.units.get_or_init(|| read_units(self.bus.as_deref())).as_ref().map_err(Clone::clone)
    }

    /// The repository registry from the map and home repo this service pinned at start, not from a
    /// second resolution of the config: `Registry::new` reads exactly the values it is handed.
    fn registry(&self) -> Registry {
        let text = fs::read_to_string(&self.world.repo_map).ok();
        let env = BTreeMap::from([("SPIRA_HOME_REPO".to_string(), self.world.home_repo.clone())]);
        Registry::new(text.as_deref(), &env, &self.home)
    }

    fn landref(&self, name: &str) -> String {
        let mut cache = self.landrefs.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((_, v)) = cache.get(name).filter(|(at, _)| at.elapsed() < LANDREF_TTL) {
            return v.clone();
        }
        let value = spira_config::repos::landref(&self.registry(), name).map(|v| format!("{v}\n")).unwrap_or_default();
        cache.insert(name.to_string(), (Instant::now(), value.clone()));
        value
    }
}

/// Free percent of a filesystem the way `df -kP` reports it: available over total, in 1 KiB blocks.
pub fn free_pct(path: &str) -> Option<i64> {
    let c = CString::new(path).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = s.f_frsize as u128;
    let total = s.f_blocks as u128 * frsize / 1024;
    let avail = s.f_bavail as u128 * frsize / 1024;
    (total > 0).then(|| (avail * 100 / total) as i64)
}

/// A checkout's git directory, and the common one its refs live in (a linked worktree's is its parent's).
fn git_dirs(repo: &Path) -> Option<(PathBuf, PathBuf)> {
    let dotgit = repo.join(".git");
    let gitdir = if dotgit.is_file() {
        let text = fs::read_to_string(&dotgit).ok()?;
        let target = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
        if target.is_absolute() { target } else { repo.join(target) }
    } else {
        dotgit
    };
    let common = match fs::read_to_string(gitdir.join("commondir")) {
        Ok(rel) => {
            let rel = PathBuf::from(rel.trim());
            if rel.is_absolute() { rel } else { gitdir.join(rel) }
        }
        Err(_) => gitdir.clone(),
    };
    Some((gitdir, common))
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commit a ref names, from its loose file or `packed-refs` (the peeled line when a tag has one).
/// A loose tag or a symbolic ref is `Unknown`: the file may name a tag object, or another ref.
fn read_ref(common: &Path, name: &str) -> Ref {
    let packed = fs::read_to_string(common.join("packed-refs")).unwrap_or_default();
    let spellings = [format!("refs/{name}"), format!("refs/heads/{name}"), format!("refs/remotes/{name}"), format!("refs/tags/{name}")];
    for candidate in spellings.iter().chain(std::iter::once(&name.to_string())) {
        if candidate.starts_with("refs/") {
            if let Ok(text) = fs::read_to_string(common.join(candidate)) {
                let sha = text.trim();
                return if is_sha(sha) && !candidate.starts_with("refs/tags/") { Ref::Sha(sha.to_string()) } else { Ref::Unknown };
            }
        }
        let mut lines = packed.lines().peekable();
        while let Some(l) = lines.next() {
            if let Some((sha, _)) = l.split_once(' ').filter(|(sha, r)| r == candidate && is_sha(sha)) {
                let peeled = lines.peek().and_then(|n| n.strip_prefix('^')).filter(|p| is_sha(p));
                return Ref::Sha(peeled.unwrap_or(sha).to_string());
            }
        }
    }
    if name.starts_with("refs/") || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/')) {
        return Ref::Unknown;
    }
    Ref::Absent
}

/// The `spira/*` branch names of a checkout, read from its ref files without running git; `None`
/// when the checkout's layout is not one this reads.
fn spira_branches(repo: &Path) -> Option<Vec<String>> {
    let (_, common) = git_dirs(repo)?;
    if !common.join("refs").is_dir() {
        return None;
    }
    let mut out = std::collections::BTreeSet::new();
    fn walk(dir: &Path, prefix: &str, out: &mut std::collections::BTreeSet<String>) {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = format!("{prefix}{}", e.file_name().to_string_lossy());
            if e.path().is_dir() {
                walk(&e.path(), &format!("{name}/"), out);
            } else if !name.ends_with(".lock") {
                out.insert(name);
            }
        }
    }
    walk(&common.join("refs/heads/spira"), "spira/", &mut out);
    if let Ok(packed) = fs::read_to_string(common.join("packed-refs")) {
        for l in packed.lines().filter(|l| !l.starts_with(['#', '^'])) {
            if let Some(name) = l.split_once(' ').and_then(|(_, r)| r.strip_prefix("refs/heads/")).filter(|r| r.starts_with("spira/")) {
                out.insert(name.to_string());
            }
        }
    }
    Some(out.into_iter().collect())
}

pub fn send_mail(env: Option<&mail::env::Env>, m: &Mail) -> Result<(), String> {
    let env = env.ok_or("no mail environment configured")?;
    let args = mail::cmds::SendArgs {
        mailbox: &m.mailbox,
        from: Some(reconciler_engine::mail::FROM),
        subject: &m.subject,
        kind: &m.kind,
        default: m.default.as_deref().unwrap_or(""),
        class: m.class.as_deref().unwrap_or(""),
        bead: m.bead.as_deref().unwrap_or(""),
        urgent: false,
        digest: false,
        dry_run: false,
        requested_by: "",
    };
    let bd = mail::bead::BdCli { bdq: String::new(), conn_retries: 1 };
    mail::cmds::send(&bd, env, &args, m.body.clone()).map(|_| ())
}

impl Host for RuntimeHost {
    fn unit_state(&self, verb: &str, unit: &str) -> Result<String, String> {
        let units = self.units()?;
        let word = match verb {
            "is-active" => units.active.get(unit).cloned().unwrap_or_else(|| "inactive".into()),
            "is-enabled" => match units.files.get(unit) {
                Some(state) => state.clone(),
                None => {
                    let mut conn = Conn::connect(&self.bus.as_deref().map_or_else(bus_path, |b| Ok(b.to_string()))?)?;
                    let reply = conn.call(SYSTEMD, MANAGER_PATH, MANAGER, "GetUnitFileState", Some(unit))?;
                    reply.first().and_then(Val::as_str).map(str::to_string).ok_or_else(|| format!("GetUnitFileState {unit}: no state in reply"))?
                }
            },
            other => return Err(format!("no in-process read for systemctl {other}")),
        };
        if reconciler::is_state_word(&word) {
            Ok(word)
        } else {
            Err(format!("systemd {verb} {unit}: unrecognised state {word:?}"))
        }
    }

    fn list_enable(&self) -> String {
        self.enabled.clone()
    }

    fn free_pct(&self, path: &str) -> Option<i64> {
        free_pct(path)
    }

    fn lc(&self, argv: &[&str]) -> Result<String, String> {
        lc_call(&self.world.lc_socket, argv).map_err(|e| e.to_string())
    }

    fn certified_list(&self, repo: &Path) -> String {
        let Some(branches) = spira_branches(repo) else {
            return reconciler::run_cmd(&spira_config::release_env::tool("queue-helpers"), &["certified-list", &repo.to_string_lossy()]);
        };
        let Ok(rows) = self.lc(&["list", "--state", "CERTIFIED", "--snapshot"]) else { return String::new() };
        let Ok(rows) = serde_json::from_str::<Vec<serde_json::Value>>(&rows) else { return String::new() };
        branches
            .iter()
            .filter_map(|b| b.strip_prefix("spira/"))
            .filter_map(|id| rows.iter().find(|r| r["bead_id"] == id))
            .map(|r| format!("{} {} {}\n", r["bead_id"].as_str().unwrap_or(""), r["tip"].as_str().unwrap_or(""), r["since"].as_f64().unwrap_or(0.0) as u64))
            .collect()
    }

    fn config(&self, argv: &[&str]) -> String {
        let roster = Some(self.runes.as_str());
        match argv {
            ["rune", "task"] => runes::spira_task_runes(&self.home, roster),
            ["rune", "lane"] => runes::spira_lane_runes(&self.home, roster),
            ["rune", "partitions"] => runes::rune_partitions(&self.home, roster).into_iter().map(|(labels, exclude)| format!("{labels}\t{exclude}\n")).collect(),
            ["rune", "for-labels", labels] => runes::runes_for_labels(&self.home, labels).into_iter().map(|f| format!("{f}\n")).collect(),
            ["repo", "landref", name] => self.landref(name),
            other => reconciler::run_cmd(&spira_config::release_env::tool("spira-config"), other),
        }
    }

    fn stranded_runners(&self, repo: &Path) -> String {
        let Some(slug) = slug(repo) else { return String::new() };
        self.forge.stranded_runners(&slug, self.now, self.stranded_min_age).into_iter().map(|l| format!("{l}\n")).collect()
    }

    fn send_mail(&self, mail: &Mail) -> Result<(), String> {
        send_mail(self.mail.as_deref(), mail)
    }

    fn ref_sha(&self, repo: &Path, name: &str) -> Ref {
        git_dirs(repo).filter(|(_, common)| common.join("refs").is_dir()).map_or(Ref::Unknown, |(_, common)| read_ref(&common, name))
    }

    fn head_branch(&self, path: &Path) -> Option<String> {
        let head = fs::read_to_string(git_dirs(path)?.0.join("HEAD")).ok()?;
        let head = head.trim();
        match head.strip_prefix("ref: refs/heads/") {
            Some(branch) => Some(branch.to_string()),
            None if is_sha(head) => Some("HEAD".into()),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spira_branches_are_read_from_loose_and_packed_refs_and_through_a_worktrees_gitdir() {
        let d = testkit::TempDir::new("host-branches");
        let common = d.join("main/.git");
        fs::create_dir_all(common.join("refs/heads/spira")).unwrap();
        fs::create_dir_all(common.join("refs/heads/other")).unwrap();
        fs::write(common.join("refs/heads/spira/sp-loose"), "abc\n").unwrap();
        fs::write(common.join("refs/heads/spira/sp-stale.lock"), "").unwrap();
        fs::write(common.join("refs/heads/other/sp-not-ours"), "abc\n").unwrap();
        fs::write(common.join("packed-refs"), "# pack-refs\nabc refs/heads/spira/sp-packed\n^def\nabc refs/heads/main\n").unwrap();
        assert_eq!(spira_branches(&d.join("main")), Some(vec!["spira/sp-loose".to_string(), "spira/sp-packed".to_string()]));

        let wt_git = common.join("worktrees/w");
        fs::create_dir_all(&wt_git).unwrap();
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        fs::create_dir_all(d.join("wt")).unwrap();
        fs::write(d.join("wt/.git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        assert_eq!(spira_branches(&d.join("wt")).map(|b| b.len()), Some(2), "a linked worktree reads the common refs");
        assert_eq!(spira_branches(&d.join("absent")), None, "no checkout is not an empty one");
    }

    #[test]
    fn a_ref_is_its_loose_file_else_its_packed_line_and_a_loose_tag_is_not_trusted() {
        let d = testkit::TempDir::new("host-refs");
        let git = d.join(".git");
        fs::create_dir_all(git.join("refs/heads/local")).unwrap();
        fs::create_dir_all(git.join("refs/tags")).unwrap();
        let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        fs::write(git.join("refs/heads/local/main"), format!("{a}\n")).unwrap();
        fs::write(git.join("refs/tags/loose"), format!("{b}\n")).unwrap();
        fs::write(git.join("packed-refs"), format!("# pack-refs\n{b} refs/heads/packed\n{a} refs/tags/r-1\n^{c}\n")).unwrap();
        let host = RuntimeHost::new(World::default(), Forge::default(), None, d.path().to_path_buf(), String::new(), 0, 0, String::new(), Default::default());
        let sha = |n: &str| host.ref_sha(d.path(), n);
        assert_eq!(sha("local/main"), Ref::Sha(a.clone()));
        assert_eq!(sha("refs/heads/local/main"), Ref::Sha(a));
        assert_eq!(sha("packed"), Ref::Sha(b));
        assert_eq!(sha("r-1"), Ref::Sha(c), "a packed tag answers with the commit it peels to");
        assert_eq!(sha("loose"), Ref::Unknown, "a loose tag may be the tag object, so git is asked");
        assert_eq!(sha("nonesuch"), Ref::Absent, "no spelling git would try names it");
        assert_eq!(sha("HEAD~1"), Ref::Unknown, "a revision expression is git's to read");
        assert_eq!(RuntimeHost::new(World::default(), Forge::default(), None, d.join("elsewhere"), String::new(), 0, 0, String::new(), Default::default()).ref_sha(&d.join("elsewhere"), "x"), Ref::Unknown);
    }

    #[test]
    fn head_names_its_branch_and_a_detached_head_is_head() {
        let d = testkit::TempDir::new("host-head");
        fs::create_dir_all(d.join(".git")).unwrap();
        let host = RuntimeHost::new(World::default(), Forge::default(), None, d.path().to_path_buf(), String::new(), 0, 0, String::new(), Default::default());
        fs::write(d.join(".git/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        assert_eq!(host.head_branch(d.path()).as_deref(), Some("feature/x"));
        fs::write(d.join(".git/HEAD"), format!("{}\n", "d".repeat(40))).unwrap();
        assert_eq!(host.head_branch(d.path()).as_deref(), Some("HEAD"));
        assert_eq!(host.head_branch(&d.join("elsewhere")), None);
    }
}
