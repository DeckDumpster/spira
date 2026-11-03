//! target-reap [--dry-run] [--worktrees DIR] — remove the target/ of every worktree whose
//! bead the lifecycle record has LANDED (sp-z61hj, sp-x9kbg, sp-2c1n0; testenv::reap,
//! testenv::landed, testenv::busy, spira-config/DESIGN-build-cache.md §2.4).
//!
//! SPIRA_RUN (worktrees default to $SPIRA_RUN/worktree) is read through
//! `spira_config::process::cfg`; spira-lc and the child PATH come from `$SPIRA_RELEASE`.
//! Exit 0 on a completed pass (even one that reaped nothing), 1 when a removal failed
//! (nothing else is fatal — a worktree whose landed-ness cannot be told, or that is busy,
//! is simply kept), 2 on usage or when a declared value cannot be resolved.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use testenv::{busy, landed, reap};

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
    let dir = match dir {
        Some(d) => d,
        None => match spira_config::process::cfg("SPIRA_RUN") {
            Ok(r) => PathBuf::from(r).join("worktree"),
            Err(e) => {
                eprintln!("target-reap: {e}");
                return ExitCode::from(2);
            }
        },
    };
    // Landed-ness is the lifecycle record's: `spira-lc state <id>` = LANDED.
    let (lc_bin, path_env) = match spira_config::lifecycle_row::lc_bin().and_then(|b| Ok((b, spira_config::release_env::child_path_env_for_process()?))) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("target-reap: {e}");
            return ExitCode::from(2);
        }
    };
    let landed_fn = |id: &str, _wt: &Path| landed::lc_landed(&lc_bin, &path_env, id);
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

    let foreign_root = PathBuf::from(spira_config::process::cfg("SPIRA_RUN").unwrap_or_default()).join("foreign-cache");
    let gone = testenv::foreign::reap_stale_runs(&foreign_root, testenv::foreign::STALE_RUN_AGE, dry, &busy_fn);
    println!(
        "target-reap: {} {} stale foreign-cache run dir(s), {} MiB",
        if dry { "would remove" } else { "removed" },
        gone.len(),
        gone.iter().map(|(_, b)| b).sum::<u64>() / (1024 * 1024)
    );

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
    let state_fn = |id: &str| landed::lc_state(&lc_bin, &path_env, id);
    let tmp_ok = report("tmp: ", reap::reap_tmp_targets(Path::new("/tmp"), dry, &state_fn, &busy_fn));
    let run = spira_config::process::cfg("SPIRA_RUN").unwrap_or_default();
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
    if landed_ok && idle_ok && tmp_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
