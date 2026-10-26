//! Members that collide with each other are integrated by one aeon holding every involved
//! bead's context, not skipped or ejected one at a time. The integration is filed for the mender
//! (its summons kind `integrate`), never the builder lane; the members it names are carried by it: no round admits them
//! while it is open, and they land by their `spira: land <id>` lines when its tip does.

use std::collections::BTreeSet;
use std::path::Path;

use super::{actor, Ctx, World};
use crate::model::Member;
use crate::records::write_atomic;

const RECORD: &str = "integrations";
const KIND_LABEL: &str = "integrate";
const TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];

fn entries(c: &Ctx) -> Vec<(String, Vec<String>)> {
    std::fs::read_to_string(c.queue_file(RECORD))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(str::to_string);
            Some((it.next()?, it.collect()))
        })
        .collect()
}

fn is_open(w: &World, id: &str) -> bool {
    w.lc.bead_row(id)
        .is_none_or(|r| !TERMINAL.contains(&r.state.as_str()))
}

/// The open integration that carries `member`, if any.
pub(super) fn carried_by(w: &World, c: &Ctx, member: &str) -> Option<String> {
    entries(c)
        .into_iter()
        .find(|(id, ms)| ms.iter().any(|m| m == member) && is_open(w, id))
        .map(|(id, _)| id)
}

fn open_integration(w: &World, c: &Ctx) -> Option<String> {
    entries(c)
        .into_iter()
        .map(|(id, _)| id)
        .find(|id| is_open(w, id))
}

fn changed(w: &World, repo: &Path, base: &str, tip: &str) -> BTreeSet<String> {
    let from = w
        .git
        .merge_base(repo, base, tip)
        .unwrap_or_else(|| base.to_string());
    w.git
        .numstat(repo, &from, tip)
        .unwrap_or_default()
        .into_iter()
        .map(|(p, ..)| p)
        .collect()
}

/// The merged members whose changes touch what `blocked` changes; none when no overlap can be
/// read, and then nothing is filed: guessing the set would pull innocent members out of the round.
pub(super) fn colliding(
    w: &World,
    repo: &Path,
    base: &str,
    blocked: &Member,
    merged: &[Member],
) -> Vec<Member> {
    let theirs = changed(w, repo, base, &blocked.tip);
    merged
        .iter()
        .filter(|m| !changed(w, repo, base, &m.tip).is_disjoint(&theirs))
        .cloned()
        .collect()
}

fn context(w: &World, repo: &Path, base: &str, m: &Member) -> String {
    let row =
        w.bd.show(std::slice::from_ref(&m.id))
            .ok()
            .and_then(|r| r.into_iter().next());
    let (title, body) = row
        .map(|r| {
            (
                r.title.unwrap_or_default(),
                r.description.unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    let from = w
        .git
        .merge_base(repo, base, &m.tip)
        .unwrap_or_else(|| base.to_string());
    let files = w
        .git
        .numstat(repo, &from, &m.tip)
        .unwrap_or_default()
        .into_iter()
        .map(|(p, a, d)| format!("  {p} (+{a} -{d})"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "### {id}: {title}\nTip: {tip}\nDiff against the base: `git diff {from}..{tip}`\nFiles:\n{files}\n\nBead body:\n{body}\n",
        id = m.id,
        tip = m.tip,
    )
}

/// File the integration bead for `members` (each a tip that collides with another), unless an
/// integration is already open for this repo or a member is carried by one. `source` says how
/// the collision showed (the round-open merge, or a red only their combination produces) and
/// `evidence` is the conflict or red text. Returns the new bead id; None leaves today's
/// skip-or-eject path in force, and says why.
#[allow(clippy::too_many_arguments)]
pub(super) fn file(
    w: &World,
    label: &str,
    c: &Ctx,
    repo: &Path,
    base: &str,
    members: &[Member],
    source: &str,
    evidence: &str,
) -> Option<String> {
    if c.r.name.is_empty() || members.len() < 2 {
        return None;
    }
    if let Some(open) = open_integration(w, c) {
        w.out(format!(
            "queue.sh {label}: integration {open} is already open for {} — no second one is filed; the collision stays on today's path",
            c.r.name
        ));
        return None;
    }
    let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
    let list = ids.join(" ");
    let ctx: Vec<String> = members.iter().map(|m| context(w, repo, base, m)).collect();
    let body = format!(
        "These certified beads collide: {list}. {source}\n\n\
         ## Build\n\
         One integrated tip in {repo_name}, on a branch based on `{base}`, that satisfies every goal and acceptance below: merge each member's tip, keep both sides of every conflict, fix what only breaks in combination (renamed identifiers, migration numbers, formatting), and run the repo's checks on the result. Do not drop any member's change.\n\n\
         The commit message must end with these lines; they are how the members land when this tip does:\n{lands}\n\n\
         ## Collision evidence\n{evidence}\n\n\
         ## Members\n{ctx}\n",
        repo_name = c.r.name,
        lands = ids.iter().map(|i| format!("spira: land {i}")).collect::<Vec<_>>().join("\n"),
        ctx = ctx.join("\n"),
    );
    let title = format!("integrate {} in {}", ids.join(" + "), c.r.name);
    let scope = w
        .env
        .config("SPIRA_SCOPE_LABEL")
        .ok()
        .filter(|l| !l.is_empty())
        .map(|l| format!("{l},"))
        .unwrap_or_default();
    let labels = format!(
        "{scope}{},{KIND_LABEL},repo:{}",
        c.s.verdict.mender_label, c.r.name
    );
    let Some(id) = w.lib.create_bug(&actor(w), &title, "1", &labels, &body) else {
        w.err(format!(
            "queue.sh {label}: WARNING — the integration bead for {list} could not be filed; the collision stays on today's path"
        ));
        return None;
    };
    w.lc.create_bead(&id);
    let mut text = std::fs::read_to_string(c.queue_file(RECORD)).unwrap_or_default();
    text.push_str(&format!("{id} {list}\n"));
    if let Err(e) = write_atomic(&c.queue_file(RECORD), &text) {
        w.err(format!(
            "queue.sh {label}: WARNING — integration {id} is filed but not recorded ({e}); its members are not carried"
        ));
        return None;
    }
    w.out(format!(
        "queue.sh {label}: integration {id} filed for {list} — one aeon integrates them; no round admits them meanwhile"
    ));
    Some(id)
}
