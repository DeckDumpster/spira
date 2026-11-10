//! The three refusals `units-install` and the root `install` phase 4 both run before
//! touching a unit: path collisions with a sibling instance's config, landref currency, and
//! live dwarves under this installation. Shared here so the two callers cannot drift.

use crate::bootstrap::nonempty_env;
use crate::guards;
use crate::systemctl::Systemctl;
use crate::values::HostValues;
use std::path::Path;

pub fn check_collisions(instance: &str, host: &HostValues) -> Result<(), Vec<String>> {
    let Some(conf_file) = nonempty_env("SPIRA_CONF_FILE") else { return Ok(()) };
    let dir = match Path::new(&conf_file).parent() {
        Some(d) => d,
        None => return Ok(()),
    };
    let home = nonempty_env("HOME").unwrap_or_default();
    let mut others = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("conf") || p == Path::new(&conf_file) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                others.push(guards::parse_conf_keys(&p.to_string_lossy(), &text, &home));
            }
        }
    }
    let this: std::collections::BTreeMap<&str, &str> = [("SPIRA_RUN", host.run.as_str()), ("SPIRA_DB", host.db.as_str()), ("SPIRA_PROD", host.prod.as_str()), ("SPIRA_DOLT_DATA", host.dolt_data.as_str())]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect();
    guards::check_path_collisions(instance, &this, &others)
}

/// Landref currency (`_check_landref_current`, systemd/install.sh): the checkout must be on
/// its landref branch and not behind it, unless it is a release install (no `.git`) or a
/// detached-HEAD tag checkout.
pub fn check_landref(host: &HostValues) -> Result<(), String> {
    let exe = crate::bootstrap::argv0_path();
    landref_currency(&host.repo, exe.as_deref(), || landref_of(host))
}

fn landref_currency(repo: &str, exe: Option<&Path>, landref: impl Fn() -> Option<String>) -> Result<(), String> {
    if !Path::new(repo).join(".git").exists() {
        return Ok(());
    }
    let git = |args: &[&str]| -> (bool, String) {
        let out = spira_config::bounded::bounded("git").arg("-C").arg(repo).args(args).output();
        match out {
            Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).trim().to_string()),
            Err(_) => (false, String::new()),
        }
    };
    if let Some(commit) = exe.and_then(|e| release_commit(e.parent()?.parent()?)) {
        let Some(base) = landref() else { return Err(format!("cannot resolve landref for {repo}")) };
        return release_currency(&git, &commit, &base);
    }
    let (_, cur) = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    if cur == "HEAD" {
        let (ok, tag) = git(&["describe", "--exact-match", "--tags", "HEAD"]);
        if ok && !tag.is_empty() {
            println!("install: release install from tag {tag} — landref currency not applicable");
            return Ok(());
        }
    }
    let Some(base) = landref() else {
        return Err(format!("cannot resolve landref for {repo}"));
    };
    let base_short = spira_config::repos::ref_branch(&base);
    if cur != base_short.as_str() {
        return Err(format!("refusing — checkout is on branch {cur}, not the landref ({base_short}). switch to {base_short} first, or set SPIRA_INSTALL_FORCE=1 to override."));
    }
    let (_, behind) = git(&["rev-list", "--count", &format!("HEAD..{base}")]);
    let behind: u64 = behind.parse().unwrap_or(0);
    if behind > 0 {
        return Err(format!("refusing — checkout is {behind} commit(s) behind {base}. git -C {repo} pull --rebase, or set SPIRA_INSTALL_FORCE=1 to override."));
    }
    Ok(())
}

fn landref_of(host: &HostValues) -> Option<String> {
    let reg = spira_config::repos::Registry::from_env_checkout(std::env::vars().collect(), Path::new(&host.home));
    spira_config::repos::landref(&reg, &host.repo)
}

/// The commit a release directory records in its `MANIFEST`; `None` for anything that is not one.
fn release_commit(release: &Path) -> Option<String> {
    let text = std::fs::read_to_string(release.join("MANIFEST")).ok()?;
    text.lines().find_map(|l| l.strip_prefix("commit ")).map(|c| c.trim().to_string()).filter(|c| !c.is_empty())
}

/// A release is as current as its own commit, never the lagging checkout's HEAD.
fn release_currency(git: &dyn Fn(&[&str]) -> (bool, String), commit: &str, base: &str) -> Result<(), String> {
    let (ok, behind) = git(&["rev-list", "--count", &format!("{commit}..{base}")]);
    let Some(behind) = behind.parse::<u64>().ok().filter(|_| ok) else {
        return Err(format!("refusing — release commit {commit} is not known to the repository, so its distance from {base} cannot be measured. set SPIRA_INSTALL_FORCE=1 to override."));
    };
    if behind > 0 {
        return Err(format!("refusing — release {commit} is {behind} commit(s) behind {base}. activate a newer release, or set SPIRA_INSTALL_FORCE=1 to override."));
    }
    Ok(())
}

/// `spira_live_dwarves` (lib.sh), reimplemented directly against [`Systemctl`]. Active units
/// only — `list_matching`'s file-known-or-ever-loaded union would also catch a dead or
/// never-started dwarf unit left over from a prior run, which is not one this install would
/// disrupt.
///
/// A dwarf runs by either name (`runtime::units::is_dwarf_unit`): `spira-dwarf-*-<instance>`,
/// or the scheduler's `spira-run-sentinel_2dcall-*` (sp-li4zys). The naming rule carries no
/// instance, so every scheduler-started dwarf under this user counts against this install.
pub fn live_dwarves(systemctl: &dyn Systemctl, instance: &str) -> Vec<String> {
    use runtime::units::{is_dwarf_unit, task_glob, DWARF_TASK};
    let mut live = systemctl.list_active_matching(&format!("spira-dwarf-*-{instance}.service"));
    live.extend(systemctl.list_active_matching(&task_glob(DWARF_TASK)).into_iter().filter(|u| is_dwarf_unit(u)));
    live.sort();
    live.dedup();
    live
}

/// Run all three, in the same order `units-install` does, `Err` naming every refusal line.
pub fn preflight(instance: &str, host: &HostValues, systemctl: &dyn Systemctl) -> Result<(), Vec<String>> {
    let mut lines = Vec::new();
    if let Err(l) = check_collisions(instance, host) {
        lines.extend(l.into_iter().map(|m| format!("refusing — {m}")));
    }
    if let Err(e) = check_landref(host) {
        lines.push(e);
    }
    let live = live_dwarves(systemctl, instance);
    if !live.is_empty() {
        lines.push(format!("refusing — live dwarves for instance {instance} would be disrupted: {}", live.join(", ")));
    }
    if lines.is_empty() {
        Ok(())
    } else {
        Err(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systemctl::{FakeSystemctl, FakeUnit};

    fn repo_with_commits(n: usize) -> (testkit::TempDir, Vec<String>) {
        let d = testkit::TempDir::new("landref");
        testkit::git(d.path(), &["init", "-q", "-b", "main"]);
        let shas = (0..n)
            .map(|i| {
                testkit::git(d.path(), &["commit", "-q", "--allow-empty", "-m", &format!("c{i}")]);
                testkit::git(d.path(), &["rev-parse", "HEAD"])
            })
            .collect();
        (d, shas)
    }

    fn git_in(repo: &Path) -> impl Fn(&[&str]) -> (bool, String) + '_ {
        move |args| {
            let o = std::process::Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
            (o.status.success(), String::from_utf8_lossy(&o.stdout).trim().to_string())
        }
    }

    fn release_dir_at(commit: &str) -> testkit::TempDir {
        let rel = testkit::TempDir::new("rel");
        std::fs::create_dir_all(rel.path().join("bin")).unwrap();
        std::fs::write(rel.path().join("MANIFEST"), format!("commit {commit}\n")).unwrap();
        rel
    }

    #[test]
    fn sp_io4b8m_the_guard_run_from_a_release_at_the_tip_passes_beside_a_lagging_checkout() {
        let (d, shas) = repo_with_commits(4);
        testkit::git(d.path(), &["checkout", "-q", "--detach", &shas[0]]);
        let rel = release_dir_at(&shas[3]);
        let exe = rel.path().join("bin/units-install");
        let repo = d.path().to_str().unwrap();
        assert_eq!(landref_currency(repo, Some(&exe), || Some("main".into())), Ok(()));
        assert!(landref_currency(repo, None, || Some("main".into())).is_err(), "the same checkout, run as a checkout, is behind");
    }

    #[test]
    fn sp_io4b8m_the_guard_run_from_an_old_release_refuses() {
        let (d, shas) = repo_with_commits(4);
        let rel = release_dir_at(&shas[1]);
        let exe = rel.path().join("bin/units-install");
        let err = landref_currency(d.path().to_str().unwrap(), Some(&exe), || Some("main".into())).unwrap_err();
        assert!(err.contains("2 commit(s) behind main"), "{err}");
    }

    #[test]
    fn sp_io4b8m_a_release_with_no_resolvable_landref_is_refused() {
        let (d, shas) = repo_with_commits(1);
        let rel = release_dir_at(&shas[0]);
        let exe = rel.path().join("bin/units-install");
        assert!(landref_currency(d.path().to_str().unwrap(), Some(&exe), || None).is_err());
    }

    #[test]
    fn sp_io4b8m_a_release_at_the_landref_tip_passes_though_the_checkout_head_lags() {
        let (d, shas) = repo_with_commits(4);
        testkit::git(d.path(), &["checkout", "-q", "--detach", &shas[0]]);
        let rel = testkit::TempDir::new("rel");
        std::fs::write(rel.path().join("MANIFEST"), format!("commit {}\nbuilt x\n", shas[3])).unwrap();
        let commit = release_commit(rel.path()).expect("manifest records a commit");
        assert_eq!(release_currency(&git_in(d.path()), &commit, "main"), Ok(()));
    }

    #[test]
    fn sp_io4b8m_a_release_older_than_the_landref_is_refused() {
        let (d, shas) = repo_with_commits(4);
        let err = release_currency(&git_in(d.path()), &shas[1], "main").unwrap_err();
        assert!(err.contains("2 commit(s) behind main"), "{err}");
    }

    #[test]
    fn sp_io4b8m_a_release_commit_the_repository_does_not_know_is_refused() {
        let (d, _) = repo_with_commits(1);
        assert!(release_currency(&git_in(d.path()), &"0".repeat(40), "main").is_err());
    }

    #[test]
    fn sp_io4b8m_a_directory_without_a_manifest_commit_is_not_a_release() {
        let t = testkit::TempDir::new("norel");
        assert_eq!(release_commit(t.path()), None);
    }

    #[test]
    fn sp_li4zys_a_live_dwarf_the_scheduler_started_refuses_the_install() {
        let s = FakeSystemctl::default();
        let own = "spira-run-sentinel_2dcall-builder_2d1794009721297_2d1.service";
        s.set(own, FakeUnit { active: true, ..Default::default() });
        s.set("spira-run-sentinel_2dpass-landing.service", FakeUnit { active: true, ..Default::default() });
        assert_eq!(live_dwarves(&s, "prod"), vec![own.to_string()], "the scheduler-named dwarf is live; the landing pass is not a dwarf");
    }
}
