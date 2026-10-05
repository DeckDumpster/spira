//! The one operation: rebase `spira/<id>` onto its land ref mechanically, re-certify it, and
//! record what happened. Every branch of the flow ends in exactly one log line (once the repo
//! has resolved) and one `Exit`.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::git::Git;
use crate::holder::{classify, Holder};
use crate::record::{Exit, LogRecord, Outcome};
use crate::resolve::{resolve_stop, ConflictFile, Rules};
use crate::seam::{Gate, Seam};

#[derive(Clone, Debug)]
pub struct Config {
    /// `$SPIRA_RUN` — the sanctioned worktree root is `<run>/worktree`.
    pub run: PathBuf,
    /// `SPIRA_REBASE_STALE_LOG`.
    pub log: PathBuf,
    pub git_name: String,
    pub git_email: String,
    pub lock_wait: Duration,
}

/// What the binary prints and returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub exit: Exit,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

fn not_attempted(msg: String) -> Report {
    Report {
        exit: Exit::NotAttempted,
        stdout: None,
        stderr: Some(format!("rebase-stale: {msg}")),
    }
}

/// An exclusive flock held for the life of the value.
pub struct Lock(#[allow(dead_code)] File);

pub fn lock(path: &Path, wait: Duration) -> Result<Lock, String> {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let start = Instant::now();
    loop {
        // SAFETY: flock on an fd this function owns; no memory is shared.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Lock(f));
        }
        if start.elapsed() >= wait {
            return Err(format!("another rebase-stale holds {}", path.display()));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into())
}

/// The private scratch tree: detached, clean, at `at`. Created with `--force` so a
/// registration left dangling by a crash does not block it.
fn scratch_at(repo: &Git, scratch: &Path, at: &str) -> Result<Git, String> {
    let sg = Git::new(scratch, &repo.name, &repo.email);
    if !scratch.join(".git").exists() {
        if let Some(d) = scratch.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let r = repo.run([
            "worktree".as_ref(),
            "add".as_ref(),
            "-q".as_ref(),
            "-f".as_ref(),
            "--detach".as_ref(),
            scratch.as_os_str(),
            at.as_ref(),
        ]);
        if !r.ok {
            return Err(format!(
                "cannot create scratch worktree: {}",
                r.stderr.trim()
            ));
        }
    }
    if sg.mid_rebase() {
        let _ = sg.run(["rebase", "--abort"]);
    }
    if sg.out(["symbolic-ref", "-q", "HEAD"]).is_some() {
        // Never ours to leave on a branch; detaching is the safe direction.
        let _ = sg.run(["checkout", "-q", "--detach"]);
    }
    if !sg.ok(["checkout", "-q", "-f", "--detach", at]) || !sg.ok(["clean", "-qfd"]) {
        return Err("cannot reset the scratch worktree".into());
    }
    Ok(sg)
}

enum RebaseEnd {
    Done { mechanical: bool },
    Conflict(Vec<ConflictFile>),
    Refused(String),
}

fn continue_or_skip(sg: &Git) -> bool {
    if sg.run(["rebase", "--continue"]).ok {
        return true;
    }
    // A stop whose resolution changes nothing leaves an empty commit git will not make.
    sg.mid_rebase()
        && sg.unmerged_paths().is_empty()
        && sg.ok(["diff", "--cached", "--quiet"])
        && sg.run(["rebase", "--skip"]).ok
}

fn rebase(sg: &Git, onto: &str, rules: &Rules) -> RebaseEnd {
    let first = sg.run(["rebase", "-q", onto]);
    let mut mechanical = false;
    let mut guard = 0;
    while sg.mid_rebase() {
        guard += 1;
        if guard > 1000 {
            return RebaseEnd::Refused("rebase did not converge".into());
        }
        if sg.unmerged_paths().is_empty() {
            if !continue_or_skip(sg) && sg.mid_rebase() && sg.unmerged_paths().is_empty() {
                return RebaseEnd::Refused(format!(
                    "rebase stopped with nothing conflicted: {}",
                    first.stderr.lines().last().unwrap_or("").trim()
                ));
            }
            continue;
        }
        match resolve_stop(sg, rules) {
            Ok(_) => {
                mechanical = true;
                if !continue_or_skip(sg) && sg.mid_rebase() && sg.unmerged_paths().is_empty() {
                    return RebaseEnd::Refused(
                        "rebase --continue refused after a mechanical resolution".into(),
                    );
                }
            }
            Err(files) => return RebaseEnd::Conflict(files),
        }
    }
    if !sg.is_ancestor(onto, "HEAD") {
        return RebaseEnd::Refused(format!(
            "git refused the rebase: {}",
            first.stderr.lines().last().unwrap_or("").trim()
        ));
    }
    RebaseEnd::Done { mechanical }
}

/// Runs the whole operation. `repo_arg` is the optional second CLI argument.
pub fn run(
    id: &str,
    repo_arg: Option<&str>,
    cfg: &Config,
    seam: &dyn Seam,
    rules: &Rules,
) -> Report {
    let name = match repo_arg {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => match seam.home_repo() {
            Ok(n) => n,
            Err(e) => return not_attempted(e),
        },
    };
    let info = match seam.repo(&name) {
        Ok(i) => i,
        Err(e) => return not_attempted(e),
    };
    let br = format!("spira/{id}");
    let bref = format!("refs/heads/{br}");
    let repo = Git::new(&info.path, &cfg.git_name, &cfg.git_email);
    let log = |o: Outcome, reason: &str| LogRecord::now(id, &name, o, reason).append(&cfg.log);

    if !repo.ok(["show-ref", "--verify", "-q", &bref]) {
        return not_attempted(format!("no such branch {br} in {name}"));
    }
    let Some(land_sha) = repo.rev(&info.landref) else {
        return not_attempted(format!(
            "land ref {} does not resolve in {name}",
            info.landref
        ));
    };
    if repo.is_ancestor(&land_sha, &bref) {
        log(Outcome::Current, "");
        return Report {
            exit: Exit::Ok,
            stdout: Some(format!(
                "rebase-stale: {br} already contains {} — nothing to do",
                info.landref
            )),
            stderr: None,
        };
    }

    let tag = basename(&info.path);
    let _lock = match lock(
        &cfg.run.join(format!("rebase-stale.{tag}.lock")),
        cfg.lock_wait,
    ) {
        Ok(l) => l,
        Err(e) => return not_attempted(e),
    };

    let busy = |h: &Holder| -> Option<Report> {
        let why = match h {
            Holder::None | Holder::Leftover { .. } => return None,
            Holder::Live { path, witness } => format!(
                "checked out in a live worktree {} ({witness})",
                path.display()
            ),
            Holder::Dirty { path, why } => format!(
                "its leftover worktree {} cannot be touched: {why}",
                path.display()
            ),
            Holder::Foreign { path } => format!(
                "checked out outside the sanctioned root at {}",
                path.display()
            ),
        };
        log(Outcome::Busy, &why);
        Some(not_attempted(format!("{br} is {why} — leaving it")))
    };
    let holder = classify(&repo, &br, id, &cfg.run, seam);
    if let Some(r) = busy(&holder) {
        return r;
    }

    let Some(old_tip) = repo.rev(&bref) else {
        log(Outcome::Error, "cannot read the branch tip");
        return not_attempted(format!("cannot read {br}"));
    };
    let scratch = cfg
        .run
        .join("worktree")
        .join(format!(".rebase-stale.{tag}"));
    let sg = match scratch_at(&repo, &scratch, &old_tip) {
        Ok(g) => g,
        Err(e) => {
            log(Outcome::Error, &e);
            return not_attempted(e);
        }
    };

    let mechanical = match rebase(&sg, &land_sha, rules) {
        RebaseEnd::Done { mechanical } => mechanical,
        end => {
            let (files, detail) = match end {
                RebaseEnd::Conflict(files) => {
                    let names: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
                    let quoted: String = files
                        .iter()
                        .map(|f| format!("\n--- {} --- ({})\n{}", f.path, f.reason, f.quoted))
                        .collect::<Vec<_>>()
                        .join("");
                    (names.join(" "), quoted)
                }
                RebaseEnd::Refused(why) => (String::new(), format!("\n{why}")),
                RebaseEnd::Done { .. } => unreachable!(),
            };
            let _ = sg.run(["rebase", "--abort"]);
            let _ = sg.run(["checkout", "-q", "-f", "--detach", &land_sha]);
            log(
                Outcome::Conflict,
                &format!(
                    "files: {}",
                    if files.is_empty() { "refused" } else { &files }
                ),
            );
            seam.bump_requeue(id, "merge-conflict");
            seam.reopen(
                id,
                "rebase-conflict",
                &format!(
                    "rebase-stale: {br} does not rebase mechanically onto {}.\n\nConflicting file(s): {}\n{}",
                    info.landref,
                    if files.is_empty() { "unknown" } else { &files },
                    detail
                ),
            );
            return Report {
                exit: Exit::Conflict,
                stdout: None,
                stderr: Some(format!(
                    "rebase-stale: {br} has a real conflict — returned to an aeon"
                )),
            };
        }
    };
    let Some(new_tip) = sg.rev("HEAD") else {
        log(Outcome::Error, "cannot read the rebased tip");
        return not_attempted("cannot read the rebased tip".into());
    };

    // Re-ask immediately before anything outside the scratch tree moves.
    let holder = classify(&repo, &br, id, &cfg.run, seam);
    if let Some(r) = busy(&holder) {
        return r;
    }
    if !repo.ok(["update-ref", &bref, &new_tip, &old_tip]) {
        log(Outcome::Error, "the branch moved during the rebase");
        return not_attempted(format!(
            "{br} moved while it was being rebased — leaving it"
        ));
    }
    let sync = |h: &Holder| {
        if let Holder::Leftover { path } = h {
            // The leftover is on this very branch and was verified clean: its HEAD now names
            // the moved ref, so bring its index and files along.
            let _ = Git::new(path, &cfg.git_name, &cfg.git_email)
                .run(["reset", "-q", "--hard", "HEAD"]);
        }
    };
    sync(&holder);

    let (gate, gate_out) = seam.submit(&br, &name);
    if gate == Gate::NoVerdict {
        let _ = repo.run(["update-ref", &bref, &old_tip, &new_tip]);
        sync(&holder);
        log(Outcome::Error, "gate-no-verdict");
        return not_attempted(format!(
            "{br} rebased but the gate reached no verdict — left at its pre-rebase tip, not reopened"
        ));
    }
    if gate != Gate::Green {
        let _ = repo.run(["update-ref", &bref, &old_tip, &new_tip]);
        sync(&holder);
        log(Outcome::GateRed, &format!("tip={new_tip}"));
        seam.reopen(
            id,
            "rebase-gate-red",
            &format!(
                "rebase-stale: {br} rebased mechanically onto {} (new tip {new_tip}) but failed its gate there. Left at its pre-rebase tip {old_tip} — the rebase itself was mechanical and does not need repeating, only the gate failure below.\n\n{gate_out}",
                info.landref
            ),
        );
        return Report {
            exit: Exit::GateRed,
            stdout: None,
            stderr: Some(format!(
                "rebase-stale: {br} rebased but failed its gate at {new_tip} — returned to an aeon"
            )),
        };
    }

    let (outcome, kind) = if mechanical {
        (Outcome::Mechanical, "mechanical")
    } else {
        (Outcome::Clean, "clean")
    };
    log(outcome, &format!("tip={new_tip}"));
    seam.note(id, &format!("rebase-stale: rebased {br} onto {} ({kind}) and re-certified at {new_tip}. No aeon session used.", info.landref));
    Report {
        exit: Exit::Ok,
        stdout: Some(format!(
            "rebase-stale: {br} rebased ({kind}) and certified at {new_tip}"
        )),
        stderr: None,
    }
}
