//! batcher-cut as a library: the IO seam (git, the round VM, the queue's round verbs, the forge,
//! the bead store) and the per-repo pacing the `batcher` binary drives, so a scheduler task can
//! call the same seam in its own process instead of forking `batcher rounds-*`. The binary
//! (`src/main.rs`) is the old one-shot driver over this library; the pure decisions stay in the
//! `batcher` crate.

pub mod attribute;
#[allow(dead_code)]
pub mod endpass;
pub mod flip;
pub mod io;
pub mod order;
pub mod pace;
pub mod redtip;
pub mod screen;
pub mod vm;

use std::path::PathBuf;

use io::{Env, Land, Repo};
use spira_config::process::{cfg, cfg_parse};

/// The repo's land mode, root and land ref, from the registry (`spira_config::repos`) — the
/// one source of those facts; a second source for them is what law-schema-over-code forbids.
///
/// `repo_land` already normalizes the `queue.forge` alias to `queue`, so only `queue` and
/// `queue.local` are ever seen here (sp-o1jm6, epic sp-hq9x8).
pub fn find_repo(env_: &Env, name: &str) -> Result<Repo, String> {
    // spira_config::repos (sp-k6lku, "wave 4.13") in-process, instead of three separate
    // repo_land/repo_root/spira_landref bash seam calls.
    let reg = io::registry(env_);
    let mode = reg.land(name);
    let land = match mode.as_str() {
        "queue" => Land::Forge,
        "queue.local" => Land::Local,
        other => {
            return Err(format!(
                "{name}: mode is {other:?}, not queue or queue.local"
            ))
        }
    };
    let path = reg.root(name).unwrap_or_default();
    if path.is_empty() {
        return Err(format!(
            "{name}: repo_root returned nothing — the registry has no row for it"
        ));
    }
    let base = spira_config::repos::landref(&reg, name).unwrap_or_default();
    if base.is_empty() {
        return Err(format!(
            "{name}: spira_landref could not resolve a base ref"
        ));
    }
    // SPIRA_FORGE, resolved once at the top level into `env_.forge` — not re-read here; the
    // bare-`forge` default lives in spira/conf.d/SPIRA_FORGE, the one source (sp-yv4b3).
    let forge = env_.forge.clone();
    Ok(Repo {
        name: name.to_string(),
        path: PathBuf::from(path),
        base,
        forge,
        land,
    })
}

/// `round-vm` from the declared release, unless the caller names another.
fn default_round_vm() -> Result<PathBuf, String> {
    spira_config::release_env::tool("round-vm").map(PathBuf::from)
}

/// Every field sourced from a registered `spira/conf.d` key is read exactly once here, through
/// `spira_config::process::cfg`/`cfg_parse` — THE ONE DOOR (per Ryan 2026-10-05: one source of
/// config) — and handed down as a plain field from here on; nothing downstream re-reads the
/// environment. A key that cannot be resolved is a refusal naming it, never a Rust-level
/// default standing in for it. `SPIRA_BATCHER_ROUND_SLOTS`/`SPIRA_BATCHER_POLL_SECS`/
/// `SPIRA_VERDICTS`/`SPIRA_BATCHER_LAND_LOCK_ATTEMPTS`/`SPIRA_BATCHER_LAND_LOCK_WAIT`/
/// `SPIRA_BATCHER_LAND_UNPAUSE` (a pause-file path; unset = off) are NOT registered keys (`ls
/// spira/conf.d/` does not name them) — they stay bare `env::var` reads with their existing
/// defaults.
pub fn env_from_cfg(home: PathBuf, run: PathBuf, round_vm: Option<PathBuf>) -> Result<Env, String> {
    let var = |k: &str| std::env::var(k).ok();
    Ok(Env {
        lc_rows: Default::default(),
        home: home.clone(),
        run: run.clone(),
        queue_dir: PathBuf::from(cfg("SPIRA_QUEUE_DIR")?),
        forge: PathBuf::from(cfg("SPIRA_FORGE")?),
        // Every harness tool from the declared release.
        tsd_bin: Some(PathBuf::from(spira_config::release_env::tool("tsd-write")?)),
        round_vm: match round_vm {
            Some(r) => r,
            None => default_round_vm()?,
        },
        queue_bin: PathBuf::from(spira_config::release_env::tool("queue")?),
        rebase_stale_bin: PathBuf::from(spira_config::release_env::tool("rebase-stale")?),
        testenv_bin: PathBuf::from(spira_config::release_env::tool("testenv")?),
        round_slots: var("SPIRA_BATCHER_ROUND_SLOTS")
            .and_then(|v| v.trim().parse().ok())
            .filter(|n: &u32| *n > 0),
        hypervisor: io::Hypervisor {
            host_mib: cfg_parse::<u64>("SPIRA_HYPERVISOR_MEM_MIB")?,
            round_vm_mib: cfg_parse::<u64>("SPIRA_ROUND_VM_MEM_MIB")?,
            runner_mib: cfg_parse::<u64>("SPIRA_CI_RUNNER_MEM_MIB")?,
        },
        poll: poll_interval(var("SPIRA_BATCHER_POLL_SECS").as_deref()),
        // Batcher-parity (sp-myi6w): the Concierge's own proven values, not testenv-batch.sh's
        // own hardware-derived or unpinned defaults — see io::run_suites.
        maxpar: cfg_parse::<u32>("SPIRA_BATCH_MAXPAR")?,
        wall_secs: cfg_parse::<u64>("SPIRA_BATCHER_WALL_SECS")?,
        git_name: cfg("SPIRA_GIT_NAME")?,
        git_email: cfg("SPIRA_GIT_EMAIL")?,
        lc_bin: Some(PathBuf::from(spira_config::lifecycle_row::lc_bin()?)),
        lc_timeout: cfg_parse::<u64>("SPIRA_LC_TIMEOUT")?,
        verdicts: gate::cert::verdicts_dir(var("SPIRA_VERDICTS").as_deref(), &run),
        land_lock_attempts: var("SPIRA_BATCHER_LAND_LOCK_ATTEMPTS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(10),
        land_lock_wait: std::time::Duration::from_secs(
            var("SPIRA_BATCHER_LAND_LOCK_WAIT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
        ),
        orphan_redrive_after: cfg_parse::<u64>("SPIRA_BATCHER_ORPHAN_REDRIVE_SECS")?,
        pause_lift_after: cfg_parse::<u64>("SPIRA_BATCHER_PAUSE_LIFT_SECS")?,
        land_unpause: var("SPIRA_BATCHER_LAND_UNPAUSE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from),
    })
}

/// `SPIRA_BATCHER_POLL_SECS` as the gap between polls: seconds, fractions allowed so a world
/// whose jobs finish in milliseconds is not held to a whole second; 2 s when unset or unreadable.
pub fn poll_interval(secs: Option<&str>) -> std::time::Duration {
    let s = secs
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite())
        .unwrap_or(2.0);
    std::time::Duration::from_secs_f64(s.clamp(0.05, 3600.0))
}

/// The most members one cut may write within the store's limits (`cap_round`'s bound).
pub use state_store::limits::MAX_CUT_MEMBERS;

/// Whether a round tree declares its own testenv (`gate::own`), which certifies it with no suites.
pub use gate::own::declares_own_testenv;

/// A forge round's PR, once the base is confirmed and the head carries no foreign commits:
/// `queue batch open` with the members' titles and the shots section, the verdict green, the
/// round metric. The PR number, and whether auto-merge was armed.
#[allow(clippy::too_many_arguments)]
pub fn open_round_pr(
    env_: &Env,
    repo: &Repo,
    merged: &[batcher::core::Member],
    batch_head: &str,
    round_start: u64,
    attribution_seconds: Option<u64>,
    regreen_seconds: Option<u64>,
) -> Result<(String, Result<(), String>), String> {
    let base_sha = io::confirm_base(repo, batch_head)?;
    let base_sha = base_sha.as_str();
    let ids: Vec<String> = merged.iter().map(|m| m.id.clone()).collect();
    let foreign = io::foreign_beads(repo, base_sha, batch_head, &ids)?;
    if !foreign.is_empty() {
        return Err(format!(
            "round head carries commits of non-member bead(s) {} — refusing to open",
            foreign.join(", ")
        ));
    }
    let prr = batcher::core::pr_record(merged);
    let base_branch = repo
        .base
        .rsplit('/')
        .next()
        .unwrap_or(&repo.base)
        .to_string();
    let shots = io::batch_shots_section(
        &env_.testenv_bin,
        &env_.run,
        spira_config::process::cfg("SPIRA_LOOM_ADDR")
            .ok()
            .as_deref(),
        &repo.name,
        base_sha,
        batch_head,
    );
    let body = format!(
        "Merge-queue batch: {} beads for {}, onto {}.\n\n{}\n{shots}",
        merged.len(),
        repo.name,
        base_branch,
        prr.body
    );
    // queue batch open records the batch before the PR exists, refuses while one is open, and
    // arms auto-merge; the PR title is its own, `queue: <n> beads for <repo>`.
    let member_pairs: Vec<(String, String)> = merged
        .iter()
        .map(|m| (m.id.clone(), m.tip.clone()))
        .collect();
    let (pr_n, armed) = io::batch_open(env_, repo, &member_pairs, batch_head, &body)?;
    io::write_local_verdict(env_, &repo.name, "green", "");
    println!("{}", batcher::core::opened_event(&prr).text);
    println!(
        "batcher {}: PR {} opened — {} member(s)",
        repo.name,
        pr_n,
        merged.len()
    );

    let pr_n_out = pr_n.clone();
    let mut fields = vec![
        ("repo", repo.name.clone()),
        ("verdict", "green".to_string()),
        ("members", merged.len().to_string()),
        ("pr", pr_n.clone()),
        (
            "duration_ms",
            ((spira_config::vtime::now_epoch() - round_start) * 1000).to_string(),
        ),
        ("base", base_sha.to_string()),
    ];
    if let Some(a) = attribution_seconds {
        fields.push(("attribution_seconds", a.to_string()));
    }
    if let Some(r) = regreen_seconds {
        fields.push(("regreen_seconds", r.to_string()));
    }
    io::tsd_append_round(env_, &fields);
    Ok((pr_n_out, armed))
}

#[cfg(test)]
mod poll_tests {
    use super::poll_interval;
    use std::time::Duration;

    #[test]
    fn the_poll_gap_takes_fractions_and_bounds_what_it_is_given() {
        assert_eq!(poll_interval(None), Duration::from_secs(2));
        assert_eq!(poll_interval(Some("junk")), Duration::from_secs(2));
        assert_eq!(poll_interval(Some("1")), Duration::from_secs(1));
        assert_eq!(poll_interval(Some(" 0.2 ")), Duration::from_millis(200));
        assert_eq!(
            poll_interval(Some("0")),
            Duration::from_millis(50),
            "never a busy loop"
        );
        assert_eq!(poll_interval(Some("-3")), Duration::from_millis(50));
        assert_eq!(poll_interval(Some("NaN")), Duration::from_secs(2));
    }
}
