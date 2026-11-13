//! target-reap [--dry-run] [--if-below-floor] [--worktrees DIR] [--fraction F [--scratch]] —
//! remove the target/ of every worktree whose bead the lifecycle record has LANDED (sp-z61hj,
//! sp-x9kbg, sp-2c1n0; testenv::reap, testenv::landed, testenv::busy,
//! spira-config/DESIGN-build-cache.md §2.4).
//!
//! `--fraction F` is the bounded reap the host section's levers call: only the oldest share F of
//! the idle worktree targets (or, with `--scratch`, of the stale gate scratch) by count, and
//! nothing else.
//!
//! SPIRA_RUN (worktrees default to $SPIRA_RUN/worktree) is read through
//! `spira_config::process::cfg`; spira-lc and the child PATH come from `$SPIRA_RELEASE`.
//! Exit 0 on a completed pass (even one that reaped nothing), 1 when a removal failed
//! (nothing else is fatal — a worktree whose landed-ness cannot be told, or that is busy,
//! is simply kept), 2 on usage or when a declared value cannot be resolved.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{busy, landed, reap};

const USAGE: &str = "usage: target-reap [--dry-run] [--if-below-floor] [--worktrees DIR] [--fraction F [--scratch]]";

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub dry: bool,
    pub if_below_floor: bool,
    pub dir: Option<PathBuf>,
    pub fraction: Option<f64>,
    pub scratch: bool,
}

pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => out.dry = true,
            "--if-below-floor" => out.if_below_floor = true,
            "--scratch" => out.scratch = true,
            "--worktrees" => out.dir = Some(it.next().map(PathBuf::from).ok_or(USAGE)?),
            "--fraction" => {
                let f: f64 = it.next().and_then(|v| v.parse().ok()).ok_or(USAGE)?;
                if !(f > 0.0 && f <= 1.0) {
                    return Err(format!("--fraction must be in (0, 1], not {f}"));
                }
                out.fraction = Some(f);
            }
            _ => return Err(USAGE.into()),
        }
    }
    if out.scratch && out.fraction.is_none() {
        return Err("--scratch bounds a reap, so it needs --fraction".into());
    }
    Ok(out)
}

fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn report(out: &mut dyn Write, label: &str, r: Result<reap::Reaped, String>, dry: bool) -> bool {
    match r {
        Ok(r) => {
            let _ = writeln!(out, "{label}{}", reap::describe(&r, dry));
            true
        }
        Err(e) => {
            let _ = writeln!(out, "target-reap: {e}");
            false
        }
    }
}

/// The entry both the binary and its tests call; returns the process exit code.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            return 2;
        }
    };
    let dry = a.dry;
    let dir = match a.dir.clone() {
        Some(d) => d,
        None => match spira_config::process::cfg("SPIRA_RUN") {
            Ok(r) => PathBuf::from(r).join("worktree"),
            Err(e) => {
                let _ = writeln!(err, "target-reap: {e}");
                return 2;
            }
        },
    };
    let busy_fn = |wt: &Path| busy::worktree_busy(wt);

    let doc = spira_config::discover(None).and_then(|p| spira_config::load(&p).ok());
    let num = |k: &str, path: &str, default: u64| -> u64 {
        var(k)
            .or_else(|| doc.as_ref().and_then(|d| spira_config::get_path(d, path)).filter(|v| !v.is_empty()))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default)
    };
    let idle_secs = num("SPIRA_REAP_IDLE_SECS", "spira.reap_idle_secs", 6 * 3600);

    if let Some(fraction) = a.fraction {
        let ok = if a.scratch {
            reap_scratch(&dir, fraction, dry, &busy_fn, out, err)
        } else {
            report(out, "idle: ", reap::reap_idle(&dir, dry, idle_secs, &reap::idle_age_secs, &busy_fn, Some(fraction), None), dry)
        };
        return if ok { 0 } else { 1 };
    }

    let floor_gib = num("SPIRA_REAP_DISK_FLOOR_GIB", "spira.reap_disk_floor_gib", 50);
    let floor_bytes = floor_gib * 1024 * 1024 * 1024;
    let free = || reap::free_bytes(&dir);
    // Landed-ness is the lifecycle record's: `spira-lc state <id>` = LANDED.
    let (lc_bin, path_env) = match spira_store::lifecycle_row::lc_bin().and_then(|b| Ok((b, spira_config::release_env::child_path_env_for_process()?))) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(err, "target-reap: {e}");
            return 2;
        }
    };
    let landed_fn = |id: &str, _wt: &Path| landed::lc_landed(&lc_bin, &path_env, id);

    let foreign_root = PathBuf::from(spira_config::process::cfg("SPIRA_RUN").unwrap_or_default()).join("foreign-cache");
    let gone = crate::foreign::reap_stale_runs(&foreign_root, crate::foreign::STALE_RUN_AGE, dry, &busy_fn);
    let _ = writeln!(
        out,
        "target-reap: {} {} stale foreign-cache run dir(s), {} MiB",
        if dry { "would remove" } else { "removed" },
        gone.len(),
        gone.iter().map(|(_, b)| b).sum::<u64>() / (1024 * 1024)
    );

    if a.if_below_floor {
        match free() {
            Some(f) if f >= floor_bytes => return 0,
            None => {
                let _ = writeln!(err, "target-reap: cannot read free space under {} — reclaiming nothing", dir.display());
                return 1;
            }
            Some(f) => {
                let _ = writeln!(err, "target-reap: {} MiB free is below the {floor_gib} GiB floor — reclaiming idle build output", f / (1024 * 1024));
            }
        }
        let r = reap::reap_idle(&dir, dry, idle_secs, &reap::idle_age_secs, &busy_fn, None, Some((&free, floor_bytes)));
        return if report(out, "", r, dry) { 0 } else { 1 };
    }

    let landed_ok = report(out, "", reap::reap(&dir, dry, &landed_fn, &busy_fn), dry);
    let idle_ok = report(out, "idle: ", reap::reap_idle(&dir, dry, idle_secs, &reap::idle_age_secs, &busy_fn, None, None), dry);
    let state_fn = |id: &str| landed::lc_state(&lc_bin, &path_env, id);
    let tmp_ok = report(out, "tmp: ", reap::reap_tmp_targets(Path::new("/tmp"), dry, &state_fn, &busy_fn), dry);
    let _ = reap_scratch_report(&dir, None, dry, gate::target::on_tmpfs(Path::new("/tmp")), &busy_fn, out);
    if landed_ok && idle_ok && tmp_ok {
        0
    } else {
        1
    }
}

fn reap_scratch(dir: &Path, fraction: f64, dry: bool, busy_fn: &dyn Fn(&Path) -> bool, out: &mut dyn Write, err: &mut dyn Write) -> bool {
    if spira_config::process::cfg("SPIRA_RUN").is_err() && var("SPIRA_GATE_TARGET_ROOT").is_none() {
        let _ = writeln!(err, "target-reap: no SPIRA_RUN and no SPIRA_GATE_TARGET_ROOT — no gate scratch to find");
        return false;
    }
    if !reap_scratch_report(dir, Some(fraction), dry, gate::target::on_tmpfs(Path::new("/tmp")), busy_fn, out) {
        let _ = writeln!(out, "target-reap: {} 0 stale gate target dir(s): this box keeps no gate scratch", if dry { "would remove" } else { "removed" });
    }
    true
}

/// Reaps (or counts) the stale gate scratch; `false` when this box keeps none (`tmpfs`: whether
/// the temp dir is a tmpfs, which is where it lives unless a root is named).
pub fn reap_scratch_report(dir: &Path, fraction: Option<f64>, dry: bool, tmpfs: bool, busy_fn: &dyn Fn(&Path) -> bool, out: &mut dyn Write) -> bool {
    let run = spira_config::process::cfg("SPIRA_RUN").unwrap_or_default();
    let explicit = var("SPIRA_GATE_TARGET_ROOT").unwrap_or_default();
    let max_age = var("SPIRA_GATE_TARGET_MAX_AGE_MIN").and_then(|v| v.parse::<u64>().ok()).unwrap_or(120);
    let held_fn = |name: &str| gate::target::held_by_gate(Path::new(&run), name);
    let Some(root) = gate::target::root(&explicit, &run, tmpfs) else { return false };
    let gone = gate::target::reap_stale(&root, dir, std::time::Duration::from_secs(max_age * 60), fraction, dry, busy_fn, &held_fn);
    let _ = writeln!(
        out,
        "target-reap: {} {} stale gate target dir(s) under {}{}",
        if dry { "would remove" } else { "removed" },
        gone.len(),
        root.display(),
        if gone.is_empty() { String::new() } else { format!(" ({})", gone.join(" ")) }
    );
    true
}
