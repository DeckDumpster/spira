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
use spira_config::git_refs::{branch_tips, read_ref_in};
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

/// One lifecycle request for the reconciler: a read the view holds is answered from it (a stale
/// view refuses, it never reaches the store); anything else goes to the store's socket.
pub fn lc_read(socket: &Path, argv: &[&str]) -> Result<String, String> {
    match store_view::held_read(argv) {
        Some(held) => held.map_err(|e| e.to_string()),
        None => lc_call(socket, argv).map_err(|e| e.to_string()),
    }
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
    /// The scheduler's status board, where the sentinel's last pass is carried; `None` where no scheduler runs.
    pub board: Option<Arc<runtime::engine::StatusBoard>>,
    units: OnceLock<Result<Units, String>>,
}

impl RuntimeHost {
    #[allow(clippy::too_many_arguments)]
    pub fn new(world: World, forge: Forge, mail: Option<Arc<mail::env::Env>>, home: PathBuf, runes: String, stranded_min_age: u64, now: u64, enabled: String, landrefs: Arc<Landrefs>) -> RuntimeHost {
        RuntimeHost { world, forge, mail, home, runes, stranded_min_age, now, enabled, landrefs, bus: None, board: None, units: OnceLock::new() }
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

/// Dwarves of `rune` the unit snapshot shows live, as `strand dwarf-count` counts them from `list-units`.
fn live_dwarves(active: &BTreeMap<String, String>, rune: &str) -> i64 {
    active
        .iter()
        .filter(|(name, state)| {
            runtime::units::is_live_dwarf(name, runtime::units::state_of_word(state)) && runtime::units::is_dwarf_of(name, rune, rune)
        })
        .count() as i64
}

/// The default tmux socket's identity (inode and change time) when the socket exists; `None`
/// otherwise, which leaves the question to tmux itself, as a down server is tmux's to name. A stale
/// socket is no harm: it is only an identity to hold a healthy answer under, and a held answer is
/// only ever one tmux gave.
fn tmux_server_identity() -> Option<String> {
    let dir = std::env::var_os("TMUX_TMPDIR").filter(|d| !d.is_empty()).map_or_else(|| PathBuf::from(system_paths::TMP), PathBuf::from);
    tmux_socket_identity(&dir.join(format!("tmux-{}", unsafe { libc::getuid() })))
}

fn tmux_socket_identity(socket_dir: &Path) -> Option<String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let socket = socket_dir.join("default");
    let meta = fs::metadata(&socket).ok().filter(|m| m.file_type().is_socket())?;
    Some(format!("{}:{}.{}", meta.ino(), meta.ctime(), meta.ctime_nsec()))
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
    let bd = mail::bead::BdCli { conn_retries: 1 };
    mail::cmds::send(&bd, env, &args, m.body.clone()).map(|_| ())
}

impl RuntimeHost {
    /// Starts per rune whose unit is not yet live: a live one is already in `dwarf_count`.
    pub fn starts_not_yet_live(&self, in_flight: &BTreeMap<String, String>) -> BTreeMap<String, u64> {
        not_yet_live(in_flight, &self.units().map(|u| u.active.clone()).unwrap_or_default())
    }
}

fn not_yet_live(in_flight: &BTreeMap<String, String>, active: &BTreeMap<String, String>) -> BTreeMap<String, u64> {
    let mut starting = BTreeMap::new();
    for (unit, rune) in in_flight {
        let live = active.get(unit).is_some_and(|state| runtime::units::is_live_dwarf(unit, runtime::units::state_of_word(state)));
        if !live {
            *starting.entry(rune.clone()).or_insert(0) += 1;
        }
    }
    starting
}

impl Host for RuntimeHost {
    fn uncalled(&self) -> Option<Result<BTreeMap<String, String>, String>> {
        let board = self.board.as_ref()?;
        Some(crate::status::sentinel_carry(board).map_err(|e| e.to_string()).and_then(|c| c.uncalled.ok_or_else(|| crate::status::NO_PASS.to_string())))
    }

    fn unit_listing(&self) -> Result<(), String> {
        self.units().map(|_| ())
    }

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
        lc_read(&self.world.lc_socket, argv)
    }

    fn certified_list(&self, repo: &Path) -> String {
        let Some(branches) = branch_tips(repo, "refs/heads/spira/") else {
            return match spira_config::release_env::tool("queue-helpers") {
                Ok(t) => reconciler::run_cmd(&t, &["certified-list", &repo.to_string_lossy()]),
                Err(e) => {
                    eprintln!("spira-runtime: {e}");
                    String::new()
                }
            };
        };
        let Ok(rows) = self.lc(&["list", "--state", "CERTIFIED"]) else { return String::new() };
        let Ok(rows) = serde_json::from_str::<Vec<serde_json::Value>>(&rows) else { return String::new() };
        branches
            .iter()
            .filter_map(|(b, _)| b.strip_prefix("spira/"))
            .filter_map(|id| rows.iter().find(|r| r["bead_id"] == id))
            .map(|r| format!("{} {} {}\n", r["bead_id"].as_str().unwrap_or(""), r["tip"].as_str().unwrap_or(""), r["since"].as_f64().unwrap_or(0.0) as u64))
            .collect()
    }

    fn config(&self, argv: &[&str]) -> String {
        let roster = Some(self.runes.as_str());
        match argv {
            ["rune", "task"] => runes::spira_task_runes(&self.home, roster),
            ["rune", "reactive"] => runes::spira_reactive_runes(&self.home, roster),
            ["rune", "partitions"] => runes::rune_partitions(&self.home, roster).into_iter().map(|(labels, exclude)| format!("{labels}\t{exclude}\n")).collect(),
            ["rune", "for-labels", labels] => runes::runes_for_labels(&self.home, labels).into_iter().map(|f| format!("{f}\n")).collect(),
            ["repo", "landref", name] => self.landref(name),
            other => match spira_config::release_env::tool("spira-config") {
                Ok(t) => reconciler::run_cmd(&t, other),
                Err(e) => {
                    eprintln!("spira-runtime: {e}");
                    String::new()
                }
            },
        }
    }

    fn stranded_runners(&self, repo: &Path) -> Result<String, String> {
        let Some(slug) = slug(repo) else { return Ok(String::new()) };
        let lines = self.forge.stranded_runners(&slug, self.now, self.stranded_min_age);
        self.forge.report_spend();
        Ok(lines?.into_iter().map(|l| format!("{l}\n")).collect())
    }

    fn forge_ci_in_flight(&self, repo_name: &str) -> Result<bool, String> {
        let views = store_view::shared().ok_or("no store view is loaded")?;
        let shown = views.batch_view(repo_name).map_err(|e| format!("batch view of {repo_name}: {e}"))?.value.shown();
        let batch_pr = shown.lines().filter_map(|l| l.split_once('=')).any(|(k, v)| k == "pr" && !v.trim().is_empty());
        if batch_pr {
            return Ok(true);
        }
        let record = queue::records::read_kv(&self.world.queue_dir.join(repo_name).join("publish"))?;
        Ok(record.is_some_and(|kv| kv.get_first("pr").is_some_and(|v| !v.trim().is_empty())))
    }

    fn send_mail(&self, mail: &Mail) -> Result<(), String> {
        send_mail(self.mail.as_deref(), mail)
    }

    fn ref_sha(&self, repo: &Path, name: &str) -> Ref {
        read_ref_in(repo, name)
    }

    fn dwarf_count(&self, rune: &str) -> Option<i64> {
        Some(live_dwarves(&self.units().ok()?.active, rune))
    }

    fn ready_rows(&self, labels: &str, exclude: &str) -> Option<Vec<(String, String)>> {
        let argv = ["ready-count".to_string(), labels.to_string(), exclude.to_string(), "--json".to_string()];
        let out = spira_claim::cli::dispatch_in(&argv, &mut std::io::empty(), &|k| std::env::var(k).ok());
        Some(reconciler::parse_ready_rows(&out.out))
    }

    fn tmux_server(&self) -> Option<Option<String>> {
        tmux_server_identity().map(Some)
    }

    fn round_legs_live(&self, repo: &str, batch: &str) -> bool {
        runtime::units::SystemdUnits::connect(std::sync::Arc::new(runtime::ports::WallClock))
            .map_err(|e| e.0)
            .and_then(|u| runtime::units::live_round_units(&u, repo, batch, &|_| false))
            .map_or(true, |live| !live.is_empty())
    }

    fn head_branch(&self, path: &Path) -> Option<String> {
        spira_config::git_refs::head_branch(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tn4efjk)]
    #[test]
    fn a_start_already_live_is_not_counted_twice_as_a_start_in_flight() {
        let unit = |rune: &str, n: u32| runtime::units::unit_name("sentinel-call", &format!("{rune}-1000-{n}")).unwrap();
        let in_flight: BTreeMap<String, String> = [(unit("builder", 0), "builder".to_string()), (unit("builder", 1), "builder".to_string()), (unit("ops", 0), "ops".to_string())].into();
        let active: BTreeMap<String, String> = [(unit("builder", 0), "active".to_string()), (unit("ops", 0), "inactive".to_string())].into();
        assert_eq!(not_yet_live(&in_flight, &active), BTreeMap::from([("builder".to_string(), 1), ("ops".to_string(), 1)]));
    }

    #[test_id(t8g45l8)]
    #[test]
    fn a_unit_listing_that_cannot_reach_the_bus_is_an_error_from_the_one_cached_read() {
        let d = testkit::TempDir::new("host-listing");
        let mut host = RuntimeHost::new(World::default(), Forge::default(), None, d.path().to_path_buf(), String::new(), 0, 0, String::new(), Default::default());
        host.bus = Some(d.join("no-such-bus").display().to_string());
        let listing = host.unit_listing().unwrap_err();
        assert_eq!(host.unit_state("is-enabled", "spira-x.timer").unwrap_err(), listing);
    }

    #[test_id(trn44bc)]
    #[test]
    fn dwarves_of_a_rune_are_the_live_units_of_that_name_a_directed_one_included() {
        let units = |l: &[(&str, &str)]| l.iter().map(|(n, s)| (n.to_string(), s.to_string())).collect::<BTreeMap<_, _>>();
        let active = units(&[
            ("spira-dwarf-builder-1.service", "active"),
            ("spira-dwarf-builder-2.service", "activating"),
            ("spira-dwarf-builder-3.service", "inactive"),
            ("spira-dwarf-builder-4.service", "failed"),
            ("spira-dwarf-builder-directed-5.service", "active"),
            ("spira-dwarf-builderx-6.service", "active"),
            ("spira-dwarf-other-7.service", "active"),
            ("unrelated.service", "active"),
        ]);
        assert_eq!(live_dwarves(&active, "builder"), 3, "two pooled and the directed one");
        assert_eq!(live_dwarves(&active, "other"), 1);
        assert_eq!(live_dwarves(&active, "nobody"), 0);
    }

    #[test_id(timw746)]
    #[test]
    fn a_tmux_socket_has_an_identity_and_a_missing_or_plain_file_has_none() {
        let d = testkit::TempDir::new("host-tmux");
        assert_eq!(tmux_socket_identity(&d.join("tmux-1")), None, "no directory, no server");
        fs::create_dir_all(d.join("tmux-1")).unwrap();
        fs::write(d.join("tmux-1/default"), "").unwrap();
        assert_eq!(tmux_socket_identity(&d.join("tmux-1")), None, "a plain file is not a server");
        fs::remove_file(d.join("tmux-1/default")).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(d.join("tmux-1/default")).unwrap();
        assert!(tmux_socket_identity(&d.join("tmux-1")).is_some());
    }

    #[test_id(t1k657y)]
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
