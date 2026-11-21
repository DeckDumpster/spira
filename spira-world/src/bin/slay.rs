//! slay.sh — stop one dwarf cleanly and make its bead say what is true. Replaces
//! spira/slay.sh (bash).
//!
//! The decisions, the arguments and every refusal are `spira_world::slay`, behind ports; this
//! file is the real adapters: the lifecycle service and its content door, git and `hel`'s destruction
//! chokepoint (the one permitted door onto `git worktree remove` / `git branch -D`), and the
//! dwarf's own pidfiles and unit.

use std::env;
use std::path::{Path, PathBuf};

use hel::ports::World;
use spira_config::admission::RealProcs;
use spira_store::lc_call::LcClient;
use spira_world::slay::gitops;
use spira_world::slay::{self, BeadView, Beads, Ctx, Dwarf, Git, Lifecycle, Parsed, Ports, SalvageFailed, Stopped};
use spira_world::{proc, sysctl};

fn fatal(msg: &str) -> ! {
    eprintln!("{msg}");
    spira_otel::oneshot::exit(1);
}

fn epoch() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn proc_dir(pid: &str) -> bool {
    Path::new(&format!("{}/{pid}", system_paths::PROC)).is_dir()
}

fn read_trim(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default().trim().to_string()
}

/// `$SPIRA_RUN/dwarf-*-<id>.pid`, first match: two pidfiles for one bead is itself a defect,
/// and sorting picks one deterministically rather than silently.
fn find_dwarf_pidfile(run: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("-{id}.pid");
    let mut found: Vec<PathBuf> = std::fs::read_dir(run)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("dwarf-") && n.ends_with(&suffix)))
        .collect();
    found.sort();
    found.into_iter().next()
}

struct RealLc(Result<LcClient, String>);

impl RealLc {
    fn call(&self, argv: &[&str]) {
        if let Ok(c) = &self.0 {
            let _ = c.call(argv);
        }
    }
}

impl Lifecycle for RealLc {
    fn state(&self, id: &str) -> Option<String> {
        hel::reap::lc_state_on(self.0.as_ref().ok()?, id)
    }
    fn holder(&self, id: &str) -> Option<String> {
        let shown = self.0.as_ref().ok()?.call(&["show", id]).ok()?;
        spira_store::lc_state::parse_show(&shown.out).ok()??.holder
    }
    fn disposition(&self, id: &str, why: &str) {
        self.call(&["disposition", id, "slain", why, "slay"]);
    }
    fn holder_dead(&self, id: &str) {
        self.call(&["holder-dead", id, "slay"]);
    }
    fn drop_bead(&self, id: &str, text: &str) {
        self.call(&["drop", id, text, "slay"]);
    }
}

struct RealBeads {
    lc: Result<LcClient, String>,
    registry: spira_config::repos::Registry,
}

impl RealBeads {
    fn content(&self, args: &[&str], body: Option<&str>) -> Option<String> {
        let lc = self.lc.as_ref().ok()?;
        let argv: Vec<&str> = std::iter::once("content").chain(args.iter().copied()).collect();
        let r = match body {
            Some(b) => lc.call_stdin(&argv, b),
            None => lc.call(&argv),
        }
        .ok()?;
        (r.code == 0).then_some(r.out)
    }
}

impl Beads for RealBeads {
    fn view(&self, id: &str) -> Option<BeadView> {
        let shown = self.lc.as_ref().ok()?.call(&["content", "show", id, "--json"]).ok()?;
        let v = hel::real::json_only(&shown.out)?;
        let b = if v.is_array() { v.get(0)? } else { &v };
        let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let labels = b.get("labels").and_then(|l| l.as_array()).map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        Some(BeadView { id, labels })
    }
    fn clear_assignee(&self, id: &str) {
        if self.content(&["update", id, "--assignee", "", "--force"], None).is_none() {
            let _ = self.content(&["update", id, "--assignee", ""], None);
        }
    }
    fn label_add(&self, id: &str, label: &str) {
        let _ = self.content(&["label", "add", id, label], None);
    }
    fn label_remove(&self, id: &str, label: &str) {
        let _ = self.content(&["label", "remove", id, label], None);
    }
    fn note(&self, id: &str, text: &str) {
        let _ = self.content(&["note", id, "--stdin"], Some(text));
    }
    fn repo_root_of(&self, id: &str) -> Option<PathBuf> {
        let stated = self.lc.as_ref().ok().and_then(|c| c.call(&["content", "state", id, "repo"]).ok()).map(|r| r.out.trim().to_string()).unwrap_or_default();
        let named = if stated.is_empty() || stated.starts_with('(') { self.registry.home_repo().to_string() } else { stated };
        if named.is_empty() {
            return None;
        }
        self.registry.root(&named).map(PathBuf::from)
    }
}

struct RealGit {
    world: hel::real::Real,
}

impl Git for RealGit {
    fn is_dir(&self, p: &Path) -> bool {
        p.is_dir()
    }
    fn branch_exists(&self, repo: &Path, br: &str) -> bool {
        hel::git::Git(repo).branch_exists(br)
    }
    fn short_tip(&self, repo: &Path, br: &str) -> Option<String> {
        gitops::short_tip(repo, br)
    }
    fn landref(&self, repo: &Path) -> Option<String> {
        self.world.base(repo).map(|b| b.landref)
    }
    fn names_bead_between(&self, repo: &Path, base: &str, br: &str, id: &str) -> bool {
        gitops::names_bead_between(repo, base, br, id)
    }
    fn ahead(&self, repo: &Path, base: &str, br: &str) -> u64 {
        hel::git::Git(repo).ahead(base, br).unwrap_or(0)
    }
    fn content_landed(&self, repo: &Path, br: &str, base: &str) -> bool {
        hel::git::Git(repo).content_on_base(br, base)
    }
    fn abort_inflight(&self, wt: &Path) {
        gitops::abort_inflight(wt);
    }
    fn commit_all(&self, wt: &Path, msg: &str) -> bool {
        gitops::commit_all(wt, msg)
    }
    fn park(&self, repo: &Path, refname: &str, br: &str) -> bool {
        gitops::park(repo, refname, br)
    }
    fn salvage(&self, id: &str, wt: &Path) -> Result<Option<PathBuf>, SalvageFailed> {
        self.world.salvage(id, wt).map_err(|()| SalvageFailed)
    }
    fn destroy_worktree(&self, id: &str, wt: &Path, repo: &Path, why: &str) -> bool {
        self.world.destroy_worktree(id, wt, repo, why)
    }
    fn destroy_branch(&self, id: &str, br: &str, repo: &Path, why: &str) -> Result<(), String> {
        let base = self.world.base(repo).map(|b| b.landref);
        self.world.destroy_branch(id, br, repo, why, "slain", base.as_deref()).map_err(|e| match e {
            hel::reap::DestroyBranchErr::CertifiedQueued => "certified-queued".to_string(),
            hel::reap::DestroyBranchErr::Refused(m) | hel::reap::DestroyBranchErr::Failed(m) => m,
        })
    }
}

struct RealDwarf {
    run: PathBuf,
    lc: RealLc,
}

impl Dwarf for RealDwarf {
    fn alive(&self, pid: &str) -> bool {
        proc_dir(pid)
    }

    fn pidfile_remains(&self, id: &str) -> bool {
        find_dwarf_pidfile(&self.run, id).is_some()
    }

    fn stop(&self, id: &str, holder: Option<&str>, say: &mut Vec<String>) -> Stopped {
        let run = &self.run;
        let mut s = Stopped::default();
        let hold_pid_file = run.join(format!("hold-{id}.pid"));
        if hold_pid_file.is_file() {
            let hpid = read_trim(&hold_pid_file);
            let hb_file = run.join(format!("hold-{id}.hb"));
            let hbpid = if hb_file.is_file() { read_trim(&hb_file) } else { String::new() };
            if !hbpid.is_empty() {
                let _ = proc::signal(&hbpid, libc::SIGTERM);
            }
            let _ = std::fs::remove_file(&hold_pid_file);
            let _ = std::fs::remove_file(&hb_file);
            self.lc.call(&["unhold", id, "manual", "slay"]);
            say.push(format!(
                "hold: manual hold released for {id} (holder pid {}, heartbeat {})",
                if hpid.is_empty() { "?" } else { &hpid },
                if hbpid.is_empty() { "none" } else { &hbpid }
            ));
            return s;
        }

        let pf = find_dwarf_pidfile(run, id);
        if let Some(pf) = &pf {
            s.pid = read_trim(pf);
            s.name = read_trim(&pf.with_extension("name"));
        }
        if !s.pid.is_empty() {
            let globs = sysctl::dwarf_globs();
            let mut args = vec!["list-units"];
            args.extend(globs.iter().map(String::as_str));
            args.push("--no-legend");
            for u in sysctl::run_lines(&args) {
                if let Some(unit_name) = sysctl::first_field(&u) {
                    if sysctl::run(&["show", "-p", "MainPID", "--value", unit_name]) == s.pid {
                        s.unit = unit_name.to_string();
                        break;
                    }
                }
            }
        }
        let forget = |pf: &Option<PathBuf>| {
            if let Some(pf) = pf {
                let _ = std::fs::remove_file(pf);
                let _ = std::fs::remove_file(pf.with_extension("name"));
            }
        };
        if s.pid.is_empty() || !proc_dir(&s.pid) {
            if let Some(pid) = holder.and_then(|h| slay::live_holder_pid(h, &RealProcs)) {
                s.pid = pid.to_string();
                forget(&pf);
            }
        }
        if s.pid.is_empty() || !proc_dir(&s.pid) {
            say.push(format!("dwarf: none running for {id} (no live pid file) — setting the bead and the work only"));
            forget(&pf);
            return s;
        }
        let who = if s.name.is_empty() { "?" } else { &s.name };
        if s.unit.is_empty() {
            say.push(format!("dwarf: {who} pid {} has no unit — sending TERM", s.pid));
            let _ = proc::signal(&s.pid, libc::SIGTERM);
        } else {
            say.push(format!("dwarf: {who} pid {} is {} — stopping the unit", s.pid, s.unit));
            if !sysctl::run_ok(&["stop", &s.unit]) {
                say.push(format!("dwarf: systemctl stop failed — sending TERM to {}", s.pid));
                let _ = proc::signal(&s.pid, libc::SIGTERM);
            }
        }
        let t0 = epoch();
        loop {
            let pf_exists = pf.as_ref().is_some_and(|p| p.is_file());
            if !pf_exists && !proc_dir(&s.pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1)); // waits-on: the dwarf's exit; KILL after 60s
            if epoch() - t0 >= 60 {
                say.push("dwarf: still alive after 60s — KILL".into());
                let _ = proc::signal(&s.pid, libc::SIGKILL);
                std::thread::sleep(std::time::Duration::from_secs(1)); // waits-on: the KILL to land
                forget(&pf);
                break;
            }
        }
        if proc_dir(&s.pid) {
            say.push(format!("dwarf: pid {} SURVIVED a KILL — investigate by hand", s.pid));
            s.fail = true;
        } else {
            say.push(format!("dwarf: stopped ({}s)", epoch() - t0));
        }
        s
    }
}

fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();
    let args = match slay::parse_args(&argv) {
        Ok(Parsed::Run(a)) => a,
        Ok(Parsed::Help) => {
            print!("{}", slay::usage_text());
            spira_otel::oneshot::exit(0);
        }
        Err(e) => {
            eprint!("{}", e.message());
            spira_otel::oneshot::exit(2);
        }
    };
    spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "spira-slay", "slay");
    let run = spira_world::spira_run().unwrap_or_else(|e| fatal(&format!("spira: FATAL slay: {e}")));
    let submitted_label = spira_config::process::cfg("SPIRA_SUBMITTED_LABEL").unwrap_or_else(|e| fatal(&format!("slay.sh: FATAL: {e}")));
    let home = spira_world::locate_home().unwrap_or_else(|| fatal("slay.sh: cannot locate lib.sh — the bead's claim, note and work were NOT touched"));
    let store = spira_config::process::cfg("SPIRA_DB").unwrap_or_else(|e| format!("(unresolved: {e})"));
    let world = hel::verbs::real_world(None).unwrap_or_else(|e| fatal(&format!("slay.sh: FATAL: {e}")));
    let registry = spira_config::repos::Registry::from_env(env::vars().collect(), &home);

    let git = RealGit { world };
    let beads = RealBeads { lc: LcClient::configured(), registry };
    let lc = RealLc(LcClient::configured());
    let dwarf = RealDwarf { run: run.clone(), lc: RealLc(LcClient::configured()) };
    let ports = Ports { git: &git, beads: &beads, lc: &lc, dwarf: &dwarf, procs: &RealProcs };
    let ctx = Ctx { run: &run, submitted_label: &submitted_label, store: &store };

    let report = slay::slay(&ctx, &ports, &args);
    for l in &report.out {
        println!("{l}");
    }
    for l in &report.err {
        eprintln!("{l}");
    }
    if report.code == 0 {
        spira_otel::oneshot::end(0);
    }
    spira_otel::oneshot::exit(report.code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_store::lc_call::fake::Content;
    use std::sync::Arc;

    #[test_id(tw3a0pm)]
    #[test]
    fn slays_bead_writes_are_content_requests() {
        let door = Arc::new(Content::new(vec![]));
        let beads = RealBeads { lc: Ok(LcClient::over(door.clone(), true)), registry: spira_config::repos::Registry::from_env(Default::default(), Path::new("")) };
        beads.clear_assignee("sp-a");
        beads.note("sp-a", "slain");
        beads.label_remove("sp-a", "x");
        let seen: Vec<String> = door.requests().iter().map(|r| r.join(" ")).collect();
        assert_eq!(seen, ["content update sp-a --assignee  --force", "content note sp-a --stdin", "content label remove sp-a x"]);
    }
}
