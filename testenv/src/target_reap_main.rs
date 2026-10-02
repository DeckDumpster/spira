//! target-reap [--dry-run] [--worktrees DIR] — remove the target/ of every worktree whose
//! branch has truly landed (sp-z61hj, then sp-x9kbg twice over; testenv::reap,
//! testenv::landed, testenv::busy, spira-config/DESIGN-build-cache.md §2.4).
//!
//! Reads SPIRA_RUN (worktrees default to $SPIRA_RUN/worktree). `landing-pass` spawns this
//! by bare name (`real.rs` `ensure()`) WITHOUT setting SPIRA_HOME — only `rebase_stale`
//! does that — so SPIRA_HOME here is an OVERRIDE ONLY, never a hard requirement: this
//! binary resolves its own harness home in-process the same three rungs `queue`'s and
//! `landing-pass`'s own `harness_home` climb (law-a-binary-resolves-the-config-it-reads).
//! Exit 0 on a completed pass (even one that reaped nothing), 1 when a removal failed
//! (nothing else is fatal — a worktree whose landed-ness cannot be told, or that is busy,
//! is simply kept), 2 on usage or when no harness home can be found at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use testenv::{busy, landed, reap};

/// Where the harness this release belongs to lives: `$SPIRA_HOME` as an override only
/// (landing-pass's `ensure()` never sets it), else spira-config's own `spira.prod`, else
/// beside this binary (`<release>/bin/target-reap` → `<release>/spira`,
/// `<workspace>/target/<profile>/target-reap` → `<workspace>/spira`) — the same three
/// rungs `queue::main::harness_home` and `landing_pass::main::harness_home` climb. Resolved
/// in-process; never assumed from an unset env var.
fn harness_home(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let has_lib = |p: &Path| p.join("lib.sh").is_file();
    if let Some(h) = env.get("SPIRA_HOME").map(PathBuf::from).filter(|p| has_lib(p)) {
        return Some(h);
    }
    if let Some(doc) = spira_config::discover(None).and_then(|p| spira_config::load(&p).ok()) {
        if let Some(p) = spira_config::get_path(&doc, "spira.prod").map(PathBuf::from).filter(|p| has_lib(p)) {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    exe.ancestors().skip(1).take(4).map(|a| a.join("spira")).find(|p| has_lib(p))
}

fn main() -> ExitCode {
    let mut dry = false;
    let mut dir: Option<PathBuf> = None;
    let mut if_below_floor = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dry-run" => dry = true,
            "--if-below-floor" => if_below_floor = true,
            "--worktrees" => dir = args.next().map(PathBuf::from),
            _ => {
                eprintln!("usage: target-reap [--dry-run] [--if-below-floor] [--worktrees DIR]");
                return ExitCode::from(2);
            }
        }
    }
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let dir = match dir.or_else(|| var("SPIRA_RUN").map(|r| PathBuf::from(r).join("worktree"))) {
        Some(d) => d,
        None => {
            eprintln!("target-reap: SPIRA_RUN is unset and no --worktrees given — refusing to guess");
            return ExitCode::from(2);
        }
    };
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let Some(home) = harness_home(&env) else {
        eprintln!(
            "target-reap: cannot resolve the harness home — no usable $SPIRA_HOME, no spira.prod, \
             nothing beside this binary's own release"
        );
        return ExitCode::from(2);
    };
    let reg = spira_config::repos::Registry::from_env(env, &home);
    let landed_fn = |id: &str, wt: &Path| landed::landed(&reg, wt, id);
    let busy_fn = |wt: &Path| busy::worktree_busy(wt);

    let doc = spira_config::discover(None).and_then(|p| spira_config::load(&p).ok());
    let num = |k: &str, path: &str, default: u64| -> u64 {
        var(k)
            .or_else(|| doc.as_ref().and_then(|d| spira_config::get_path(d, path)).filter(|v| !v.is_empty()))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default)
    };
    let idle_secs = num("SPIRA_REAP_IDLE_SECS", "spira.reap_idle_secs", 6 * 3600);
    let floor_gib = num("SPIRA_REAP_DISK_FLOOR_GIB", "spira.reap_disk_floor_gib", 50);
    let floor_bytes = floor_gib * 1024 * 1024 * 1024;
    let free = || reap::free_bytes(&dir);
    let report = |label: &str, r: Result<reap::Reaped, String>| -> bool {
        match r {
            Ok(r) => {
                println!("{label}{}", reap::describe(&r, dry));
                true
            }
            Err(e) => {
                println!("target-reap: {e}");
                false
            }
        }
    };

    if if_below_floor {
        match free() {
            Some(f) if f >= floor_bytes => return ExitCode::SUCCESS,
            None => {
                eprintln!("target-reap: cannot read free space under {} — reclaiming nothing", dir.display());
                return ExitCode::from(1);
            }
            Some(f) => eprintln!(
                "target-reap: {} MiB free is below the {floor_gib} GiB floor — reclaiming idle build output",
                f / (1024 * 1024)
            ),
        }
        let r = reap::reap_idle(&dir, dry, idle_secs, &reap::idle_age_secs, &busy_fn, Some((&free, floor_bytes)));
        return if report("", r) { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }

    let landed_ok = report("", reap::reap(&dir, dry, &landed_fn, &busy_fn));
    let idle_ok = report("idle: ", reap::reap_idle(&dir, dry, idle_secs, &reap::idle_age_secs, &busy_fn, None));
    let run = var("SPIRA_RUN").unwrap_or_default();
    let explicit = var("SPIRA_GATE_TARGET_ROOT").unwrap_or_default();
    let max_age = var("SPIRA_GATE_TARGET_MAX_AGE_MIN").and_then(|v| v.parse::<u64>().ok()).unwrap_or(120);
    if let Some(root) = gate::target::root(&explicit, &run, gate::target::on_tmpfs(Path::new("/tmp"))) {
        let gone = gate::target::reap_stale(&root, &dir, std::time::Duration::from_secs(max_age * 60), dry, &busy_fn);
        println!(
            "target-reap: {} {} stale gate target dir(s) under {}{}",
            if dry { "would remove" } else { "removed" },
            gone.len(),
            root.display(),
            if gone.is_empty() { String::new() } else { format!(" ({})", gone.join(" ")) }
        );
    }
    if landed_ok && idle_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
