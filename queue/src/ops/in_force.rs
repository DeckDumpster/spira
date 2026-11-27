//! Whether a member's content blocker changes round machinery the running release lacks: the
//! hand open's and stage's twin of the cut's rule (batcher-cut/DESIGN.md "Left-out beads").

use std::cell::OnceCell;
use std::path::Path;

use super::{Ctx, World};
use crate::ports::ReleaseManifest;

/// The blocker could not be judged: which one, and why.
pub(super) struct Unread {
    pub dep: String,
    pub error: String,
}

/// The refusal for a member [`InForce::not_in_force`] does not clear, as (skip class, line);
/// `verb` says what did not happen to it.
pub(super) fn refusal(id: &str, found: Result<Option<String>, Unread>, verb: &str) -> Option<(&'static str, String)> {
    match found {
        Ok(None) => None,
        Ok(Some(dep)) => Some(("not-in-force", format!("{id}: blocked by {dep}, which changes the round machinery (`gate.steps` own) the running release lacks — waits for the release carrying {dep}; not {verb}"))),
        Err(Unread { dep, error }) => Some(("in-force-unread", format!("{id}: whether the release carries {dep} could not be read: {error} — not {verb}"))),
    }
}

/// One open's or stage's judge. `base_sha` is the landing ref, whose `gate.steps` declares what
/// is owned.
pub(super) struct InForce<'a> {
    w: &'a World<'a>,
    c: &'a Ctx,
    path: &'a Path,
    base_sha: &'a str,
    def: OnceCell<Result<Option<gate::def::Def>, String>>,
    release: OnceCell<Result<ReleaseManifest, String>>,
}

impl<'a> InForce<'a> {
    pub(super) fn new(w: &'a World<'a>, c: &'a Ctx, path: &'a Path, base_sha: &'a str) -> Self {
        InForce { w, c, path, base_sha, def: OnceCell::new(), release: OnceCell::new() }
    }

    /// The first content blocker of `member_id` that changes an owned path the release lacks.
    /// `blockers_in_round` is (id, tip) of each member that would ride with it or ahead of it.
    pub(super) fn not_in_force(&self, member_id: &str, blockers_in_round: &[(String, String)]) -> Result<Option<String>, Unread> {
        let unread = |dep: &str, error: String| Unread { dep: dep.to_string(), error };
        let def = self.def.get_or_init(|| {
            let Some(text) = self.w.git.read_at(self.path, self.base_sha, gate::def::PATH)? else { return Ok(None) };
            gate::def::parse(&text)
                .map(|d| (!d.own.is_empty()).then_some(d))
                .map_err(|e| format!("{} on the landing ref does not read: {e}", gate::def::PATH))
        });
        let def = match def {
            Ok(None) => return Ok(None),
            Ok(Some(d)) => d,
            Err(e) => return Err(unread("its blockers", e.clone())),
        };
        for dep in self.content_blockers(member_id).map_err(|e| unread("its blockers", e))? {
            if self.lacks(def, &dep, blockers_in_round).map_err(|e| unread(&dep, e))? {
                return Ok(Some(dep));
            }
        }
        Ok(None)
    }

    /// The content store's edges, not only the row's `blocked_by`: that mirror lags, and drops a
    /// blocker once it lands.
    fn content_blockers(&self, id: &str) -> Result<Vec<String>, String> {
        let rows = self.w.bd.show(&[id.to_string()])?;
        let row = rows.into_iter().find(|r| r.id == id).ok_or_else(|| format!("{id} has no content row"))?;
        Ok(row
            .dependencies
            .into_iter()
            .filter(|d| spira_config::edges::blocks(spira_config::edges::Edge::Dep(&d.kind)))
            .map(|d| d.depends_on_id)
            .collect())
    }

    fn lacks(&self, def: &gate::def::Def, dep: &str, in_round: &[(String, String)]) -> Result<bool, String> {
        let riding = in_round.iter().find(|(id, _)| id == dep).map(|(_, tip)| tip.clone());
        let landed = match riding {
            Some(_) => None,
            None => {
                let rows = self.w.lc.bead_rows_of(&[dep.to_string()]).map_err(|e| format!("its blocker's state could not be read: {e}"))?;
                let row = rows.into_iter().find(|r| {
                    r.bead_id == dep && r.state == "LANDED" && r.repo.as_deref().is_none_or(|n| n == self.c.r.name)
                });
                let Some(row) = row else { return Ok(false) };
                let tip = row.tip.unwrap_or_default();
                if !lifecycle::bead::is_full_sha(&tip) {
                    return Err(format!("its LANDED row for {dep} carries no full-sha tip (`{tip}`)"));
                }
                Some(tip)
            }
        };
        let release = self.release.get_or_init(|| self.w.env.release_manifest()).as_ref().map_err(Clone::clone)?;
        if release.repo != self.c.r.name {
            return Ok(false);
        }
        let paths = match (riding, landed) {
            (Some(tip), _) => self.changed_since_fork(self.base_sha, &tip)?,
            (None, Some(sha)) => self.landed_paths(&release.commit, &sha)?,
            (None, None) => Vec::new(),
        };
        Ok(paths.iter().any(|p| def.owns(p)))
    }

    /// `git diff a...b`.
    fn changed_since_fork(&self, a: &str, b: &str) -> Result<Vec<String>, String> {
        let from = self.w.git.fork_point(self.path, a, b)?;
        self.changed(&from, b)
    }

    fn changed(&self, a: &str, b: &str) -> Result<Vec<String>, String> {
        Ok(self.w.git.numstat(self.path, a, b)?.into_iter().map(|(p, ..)| p).collect())
    }

    /// The paths the LANDED change at `sha` touched that the release `commit` lacks: none when
    /// `sha` is off the landing ref or already in `commit`; else its landing merge's diff against
    /// its first parent, or `commit...sha` when no merge took it in.
    fn landed_paths(&self, commit: &str, sha: &str) -> Result<Vec<String>, String> {
        let git = self.w.git;
        if !git.ancestry(self.path, sha, self.base_sha)? || git.ancestry(self.path, sha, commit)? {
            return Ok(Vec::new());
        }
        let log = git.log_range(self.path, &format!("{sha}..{}", self.base_sha))?;
        // Newest first: the last match is the merge that first took `sha` in.
        match log.iter().rev().find(|m| m.parents.iter().skip(1).any(|p| p == sha)) {
            Some(m) => self.changed(&m.parents[0], &m.sha),
            None => self.changed_since_fork(commit, sha),
        }
    }
}
