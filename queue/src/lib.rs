//! queue — the merge queue's operator and landing tool. See DESIGN.md.

pub mod cli;
pub mod ident;
pub mod lock;
pub mod model;
pub mod ops;
pub mod ports;
pub mod publish_range;
pub mod real;
pub mod records;
pub mod seam;
pub mod stats;

#[cfg(test)]
pub mod testutil;
#[cfg(test)]
mod tests;

use cli::Cmd;
use ports::World;

/// Run one parsed command against a world; the process exit code.
pub fn dispatch(w: &World, cmd: &Cmd) -> i32 {
    use ops::*;
    match cmd {
        Cmd::Help => {
            w.out(cli::USAGE);
            0
        }
        Cmd::Submit { branch, repo } => simple::submit(w, branch, repo.as_deref()),
        Cmd::Protect { repo } => simple::protect(w, repo.as_deref()),
        Cmd::Stats => simple::stats(w),
        Cmd::Flush { repo } => simple::flush(w, repo.as_deref()),
        Cmd::Step { repo } => simple::step(w, repo),
        Cmd::StepAll => simple::step_all(w),
        Cmd::Verdict { repo } => verdict::verdict(w, repo),
        Cmd::Eject { id, repo, reason, suites, red, dry_run } => batch::eject(w, id, repo.as_deref(), reason, suites, *red, *dry_run),
        Cmd::Abandon { repo, reason, dry_run } => batch::abandon(w, repo.as_deref(), reason, *dry_run),
        Cmd::OpenBatch { repo, members, skip_pregate, dry_run } => batch::open_batch(w, repo.as_deref(), members, *skip_pregate, *dry_run),
        Cmd::Claim { repo, reason, force } => simple::claim(w, repo.as_deref(), reason, *force),
        Cmd::Release { repo } => simple::release(w, repo.as_deref()),
        Cmd::LandLocal { repo, head, members, worktree } => land::land_local(w, repo.as_deref(), head, members, worktree.as_deref()),
        Cmd::Publish { repo } => publish::publish(w, repo.as_deref()),
        Cmd::PublishSettle { repo } => publish::publish_settle(w, repo.as_deref()),
        Cmd::ToForge { repo } => transition::to_forge(w, repo.as_deref()),
        Cmd::ToLocal { repo } => transition::to_local(w, repo.as_deref()),
        Cmd::RollbackLocal { repo } => land::rollback_local(w, repo.as_deref()),
    }
}
