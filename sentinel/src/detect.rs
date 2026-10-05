//! Family T-a — the two sentinel-side detectors (DESIGN.md §4, CHECK 7c/7d). Ported from
//! lib.sh `detect_unclaimable_ready`/`file_unclaimable_incidents`/
//! `detect_branch_collisions`/`park_branch_collisions` (wave 4.28, sp-fbqsv). The
//! strand-side half of family T (livelocked/landed-but-open/closed-unlanded/false-blockers/
//! incident-needs-builder/invalid-closed/all_partition_members) is a different bead
//! (4.29, "T-b → strand"); nothing here touches it.
//!
//! `detect_unclaimable_ready` still classifies through `spira/unclaimable.py` — split out
//! on purpose (DESIGN comment in that file) so a fixture-JSON table can drive it directly,
//! and `cockpit-collect`'s own test (test-cockpit-unclaimable.sh, UC-dispatch-17)
//! cross-validates against it independently of this binary. Reimplementing the classifier
//! natively here would be a second copy of that logic to keep in sync; this module only
//! gathers PARTS/ALL_PARTS/the ready set and hands them to the one classifier.

use std::collections::HashMap;

use crate::host::{Io, Spec};
use crate::model::parse_beads;
use crate::pass::Sentinel;
use spira_config::lc_state;

// ---------------------------------------------------------------------------------------
// CHECK 7c — unclaimable ready beads.

/// A small bash seam, owned by this module alone (never touches `seams.rs`): every chamber
/// member's `FAYTH_LABELS`/`FAYTH_EXCLUDE_LABELS`, not just the active roster the context
/// probe (S0) already carries in `ctx.fayths`. `fayth_names`/`fayth_get` are themselves
/// one-line shims onto `spira-config fayth names|get` now (sp-r5zd2); this still goes
/// through lib.sh rather than calling that binary directly so a chamber with its own
/// FAYTH_LABELS override hook (none exist today, but lib.sh is the contract) keeps working.
const ALL_FAYTHS: &str = r#"set -uo pipefail
. "$SENTINEL_LIB" >&2 || exit 97
for _f in $(fayth_names); do
    printf '%s\t%s\t%s\0' "$_f" "$(fayth_get "$_f" FAYTH_LABELS)" "$(fayth_get "$_f" FAYTH_EXCLUDE_LABELS)"
done
"#;

/// `PARTS`/`ALL_PARTS`'s shared line shape: `<name>|<labels-csv>|<exclude-csv>\n`. A fayth
/// with no labels at all (a manual-summon persona) is skipped — lib.sh's own `[ -n "$inc" ]`
/// guard.
fn parts_line(name: &str, labels: &str, exclude: &str) -> Option<String> {
    if labels.is_empty() {
        None
    } else {
        Some(format!("{name}|{labels}|{exclude}\n"))
    }
}

impl<'a> Sentinel<'a> {
    /// `PARTS`: the active roster (`ctx.fayths`, already in hand from the probe — no extra
    /// call).
    fn active_parts_str(&self) -> String {
        self.ctx
            .fayths
            .iter()
            .filter_map(|f| parts_line(&f.name, &f.labels.join(","), &f.exclude.join(",")))
            .collect()
    }

    /// `ALL_PARTS`: the full chamber, including fayths the active roster parks.
    fn all_parts_str(&self) -> String {
        let o = self.h.run(
            Spec::args_owned(
                "bash".to_string(),
                vec!["-c".into(), ALL_FAYTHS.into(), "sentinel-all-fayths".into()],
            )
            .env("SENTINEL_LIB", self.lib.to_string_lossy().into_owned())
            .out(Io::Capture)
            .err(Io::Null),
        );
        let mut s = String::new();
        for rec in o.stdout.split('\0') {
            if rec.is_empty() {
                continue;
            }
            let mut f = rec.splitn(3, '\t');
            let name = f.next().unwrap_or("");
            let labels = f.next().unwrap_or("");
            let exclude = f.next().unwrap_or("");
            if let Some(l) = parts_line(name, labels, exclude) {
                s.push_str(&l);
            }
        }
        s
    }

    /// The broad (unscoped) ready set as raw JSON text, passed through unchanged —
    /// `unclaimable.py` reads fields (`external_ref`) this binary's own `Bead` does not
    /// model, so this never round-trips through `parse_beads`. `snap_ready_raw` is the
    /// pass's own `ready_raw_args` snapshot when running inside a full pass; `None` for a
    /// standalone `sentinel --detect-unclaimable`, which falls back to
    /// `$SPIRA_READY_SNAPSHOT` and then a live `bd ready` call.
    fn broad_ready_raw(&self, snap_ready_raw: Option<&str>) -> String {
        if let Some(r) = snap_ready_raw {
            return r.to_string();
        }
        if let Ok(p) = std::env::var("SPIRA_READY_SNAPSHOT") {
            if !p.is_empty() {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    return text;
                }
            }
        }
        let args = crate::store::ready_raw_args(&self.cfg);
        self.bd().json(self.h, &args).unwrap_or_default()
    }

    /// lib.sh `detect_unclaimable_ready`. The bash original re-sourced lib.sh from the
    /// PRODUCTION checkout when called from a worktree whose conf.sh carried different
    /// partition labels (sp-b0j0s) — a workaround for a bash function having no persistent,
    /// correctly-resolved context across calls. This binary resolves its `Context` once at
    /// startup, from `SPIRA_HOME`/the binary's own location, the same way for every check in
    /// the pass (law-a-binary-resolves-the-config-it-reads); there is no second checkout to
    /// fall back to, so that re-exec is retired rather than ported — see
    /// test-unclaimable-worktree.sh for the suite kept to prove this directly.
    pub fn detect_unclaimable_ready(&self, snap_ready_raw: Option<&str>) -> String {
        let parts = self.active_parts_str();
        if parts.is_empty() {
            return String::new();
        }
        let all_parts = self.all_parts_str();
        let ready_raw = self.broad_ready_raw(snap_ready_raw);
        let o = self.h.run(
            Spec::args_owned("unclaimable.py".to_string(), Vec::new())
                .env("PARTS", parts)
                .env("ALL_PARTS", all_parts)
                .stdin(ready_raw.into_bytes())
                .out(Io::Capture)
                .err(Io::Null),
        );
        o.stdout
    }

    /// lib.sh `file_unclaimable_incidents`: one P1 incident per `UNCLAIMABLE <id> — <reason>`
    /// line, filed into the Groomer partition (idempotent — incident.sh dedupes on the
    /// `unclaimable:<id>` ref).
    pub fn file_unclaimable_incidents(&self, lines: &str) {
        let groomer = self
            .ctx
            .get("SPIRA_GROOMER_LABEL")
            .filter(|s| !s.is_empty())
            .unwrap_or("groom")
            .to_string();
        let scope = if self.cfg.scope.is_empty() { "spira" } else { &self.cfg.scope };
        let home_repo = if self.cfg.home_repo.is_empty() { "spira" } else { &self.cfg.home_repo };
        let inc = self.cfg.incident_sh.to_string_lossy().into_owned();
        for line in lines.lines() {
            let Some(rest) = line.strip_prefix("UNCLAIMABLE ") else {
                continue;
            };
            let (bid, reason) = match rest.split_once(" — ") {
                Some((b, r)) => (b, r),
                None => (rest, ""),
            };
            let bid = bid.trim();
            if bid.is_empty() {
                continue;
            }
            self.h.run(
                Spec::args_owned(
                    "bash".to_string(),
                    vec![
                        inc.clone(),
                        "file".into(),
                        format!("UNCLAIMABLE: {bid} — fix the fayth: or partition label"),
                        "-".into(),
                    ],
                )
                .env("SPIRA_DB", self.cfg.db.clone())
                .env("SPIRA_INCIDENT_TYPE", "task")
                .env("SPIRA_INCIDENT_PRIORITY", "1")
                .env("SPIRA_INCIDENT_ACTOR", "sentinel")
                .env("SPIRA_INCIDENT_LABELS", format!("{scope},{groomer}"))
                .env("SPIRA_INCIDENT_REPO", home_repo.to_string())
                .env("SPIRA_INCIDENT_REF", format!("unclaimable:{bid}"))
                .env("SPIRA_INCIDENT_CAUSE", "unclaimable")
                .stdin(reason.as_bytes().to_vec())
                .out(Io::Null)
                .err(Io::Null),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// CHECK 7d — branch collisions.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    pub id: String,
    pub repo: String,
    pub branch: String,
    pub holder_id: String,
    pub holder_path: String,
}

impl Collision {
    pub fn line(&self) -> String {
        format!(
            "COLLISION {} {} {} {} {}",
            self.id, self.repo, self.branch, self.holder_id, self.holder_path
        )
    }

    pub fn parse_line(s: &str) -> Option<Collision> {
        let rest = s.strip_prefix("COLLISION ")?;
        let mut f = rest.splitn(5, ' ');
        Some(Collision {
            id: f.next()?.to_string(),
            repo: f.next()?.to_string(),
            branch: f.next()?.to_string(),
            holder_id: f.next()?.to_string(),
            holder_path: f.next()?.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkOutcome {
    /// An inherited `branch:` label (copied by `bd create --parent`) is cut; the bead falls
    /// back to its own default branch.
    Unlabeled { id: String, repo: String, branch: String, from: String },
    /// The squatting bead is closed, clean and unheld: its worktree is destroyed, freeing
    /// the branch without any human decision.
    Freed { id: String, repo: String, branch: String, holder_id: String, holder_path: String },
}

impl ParkOutcome {
    pub fn line(&self) -> String {
        match self {
            ParkOutcome::Unlabeled { id, repo, branch, from } => {
                format!("UNLABELED {id} {repo} {branch} {from}")
            }
            ParkOutcome::Freed { id, repo, branch, holder_id, holder_path } => {
                format!("FREED {id} {repo} {branch} {holder_id} {holder_path}")
            }
        }
    }
}

/// `git worktree list --porcelain` -> `(branch-ref, worktree-path)` for every entry that has
/// a branch (a detached worktree contributes nothing — never a collision target).
pub fn parse_worktree_porcelain(s: &str) -> Vec<(String, String)> {
    let mut cur: Option<String> = None;
    let mut out = Vec::new();
    for line in s.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            cur = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(w) = &cur {
                out.push((b.to_string(), w.clone()));
            }
        }
    }
    out
}

/// The first worktree (in listing order) checked out on `refs/heads/<branch>`.
pub fn find_holder(list: &[(String, String)], branch: &str) -> Option<String> {
    let want = format!("refs/heads/{branch}");
    list.iter().find(|(b, _)| *b == want).map(|(_, w)| w.clone())
}

impl<'a> Sentinel<'a> {
    fn repo_root_cmd(&self, name: &str) -> Option<String> {
        let o = self.h.run(
            Spec::args_owned("spira-config".to_string(), vec!["repo".into(), "root".into(), name.to_string()])
                .err(Io::Null),
        );
        (o.ok() && !o.stdout.trim().is_empty()).then(|| o.stdout.trim().to_string())
    }

    fn worktree_prefix(&self) -> String {
        format!("{}/", self.cfg.run.join("worktree").display())
    }

    /// lib.sh `detect_branch_collisions`. The candidates are the claimable beads — READY or
    /// REWORK in the lifecycle machine (what bd `open` meant), never bd's status (design
    /// §3.4, sp-mve9i); bd lists every bead's content and the machine says which wait for a
    /// builder. No lifecycle read: no candidates, nothing detected this pass.
    pub fn detect_branch_collisions(&self) -> Vec<Collision> {
        let Some(rows) = self.lc_rows() else {
            return Vec::new();
        };
        let claimable: std::collections::HashSet<&str> = rows
            .iter()
            .filter(|r| lc_state::is_claimable(&r.state))
            .map(|r| r.bead_id.as_str())
            .collect();
        let args = vec![
            "list".into(),
            "--all".into(),
            "--limit".into(),
            "0".into(),
            "--exclude-type".into(),
            "epic,event".into(),
        ];
        let Ok(raw) = self.bd().json(self.h, &args) else {
            return Vec::new();
        };
        let Ok(mut beads) = parse_beads(&raw) else {
            return Vec::new();
        };
        beads.retain(|b| claimable.contains(b.id.as_str()));
        let ask = &self.cfg.ask;
        let prefix = self.worktree_prefix();
        let mut maps: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let mut out = Vec::new();
        for b in &beads {
            if !ask.is_empty() && b.has(ask) {
                continue;
            }
            let repo = b.label_value("repo:").map(str::to_string).unwrap_or_else(|| self.cfg.home_repo.clone());
            if !maps.contains_key(&repo) {
                let list = match self.repo_root_cmd(&repo) {
                    Some(root) if is_git_dir(&root) => {
                        let o = self.git(&root, &["worktree", "list", "--porcelain"]);
                        parse_worktree_porcelain(&o.stdout)
                    }
                    _ => Vec::new(),
                };
                maps.insert(repo.clone(), list);
            }
            let list = &maps[&repo];
            if list.is_empty() {
                continue;
            }
            let branch = b.label_value("branch:").map(str::to_string).unwrap_or_else(|| format!("spira/{}", b.id));
            let Some(holder_path) = find_holder(list, &branch) else {
                continue;
            };
            let Some(holder_id) = holder_path.strip_prefix(&prefix) else {
                continue;
            };
            if holder_id == b.id {
                continue;
            }
            out.push(Collision {
                id: b.id.clone(),
                repo: repo.clone(),
                branch,
                holder_id: holder_id.to_string(),
                holder_path: holder_path.clone(),
            });
        }
        out
    }

    fn label_list(&self, id: &str) -> String {
        self.bd().call(self.h, &["label", "list", id], None).stdout
    }


    fn holder_alive(&self, id: &str) -> bool {
        self.h
            .run(Spec::args_owned(self.cfg.sending_bin.clone(), vec!["holder-alive".into(), id.to_string()]).err(Io::Null))
            .ok()
    }

    fn destroy_worktree(&self, id: &str, path: &str, repo_root: &str, why: &str) -> bool {
        self.h
            .run(
                Spec::args_owned(
                    self.cfg.sending_bin.clone(),
                    vec!["destroy-worktree".into(), id.into(), path.into(), repo_root.into(), why.into()],
                )
                .err(Io::Null),
            )
            .ok()
    }

    /// Commit subjects in `holder_path` naming `id` (lib.sh: `git log --grep="$id:" -F`,
    /// double-checked for a leading `<id>:` so a mid-subject substring match is excluded).
    fn inherited_commits(&self, holder_path: &str, id: &str) -> String {
        let o = self.git(holder_path, &["log", "--format=%h\t%s", "--grep", &format!("{id}:"), "-F"]);
        let want = format!("{id}:");
        o.stdout
            .lines()
            .filter_map(|l| {
                let (sha, subj) = l.split_once('\t')?;
                subj.starts_with(&want).then(|| format!("{sha} {subj}"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// lib.sh `park_branch_collisions`. Idempotent on repeat input: a bead already carrying
    /// `SPIRA_ASK_LABEL` is skipped outright; an inherited `branch:` label already cut is
    /// neither re-cut nor re-noted (the inner match on `branch:$branch` simply finds
    /// nothing, and the function still falls through to its own `continue` for every
    /// apparently-inherited branch name, matching lib.sh exactly).
    pub fn park_branch_collisions(&self, collisions: &[Collision]) -> Vec<ParkOutcome> {
        let mut out = Vec::new();
        for c in collisions {
            let labels = self.label_list(&c.id);
            if labels.contains(self.cfg.ask.as_str()) {
                continue;
            }

            let inherited_from = c.branch.strip_prefix("spira/").map(str::to_string);
            if let Some(inh) = &inherited_from {
                let looks_inherited = !inh.is_empty()
                    && inh != &c.id
                    && self.bd().call(self.h, &["show", inh, "--json"], None).ok();
                if looks_inherited {
                    if labels.contains(&format!("branch:{}", c.branch)) {
                        let commits = self.inherited_commits(&c.holder_path, &c.id);
                        self.bd().quiet(self.h, &["label", "remove", &c.id, &format!("branch:{}", c.branch)], None);
                        let note = if commits.is_empty() {
                            format!(
                                "Corrected by detect_branch_collisions: inherited branch:{} from {}; cuts its own branch.",
                                c.branch, inh
                            )
                        } else {
                            format!(
                                "Corrected by detect_branch_collisions: inherited branch:{} from {}; cuts its own branch. This bead has its own commit(s) sitting unlanded on {}, made before this label was removed: {}",
                                c.branch, inh, c.branch, commits
                            )
                        };
                        self.bd().quiet(self.h, &["note", &c.id, "--stdin"], Some(&note));
                        out.push(ParkOutcome::Unlabeled {
                            id: c.id.clone(),
                            repo: c.repo.clone(),
                            branch: c.branch.clone(),
                            from: inh.clone(),
                        });
                    }
                    continue;
                }
            }

            // The squatter's builder has handed it on: its lifecycle row in the pass's one
            // `spira-lc list` read (never bd status, sp-mve9i). No row, or no read, leaves
            // the worktree alone.
            let holder_done = self
                .lc_rows()
                .is_some_and(|rows| rows.iter().any(|r| r.bead_id == c.holder_id && lc_state::past_builder(&r.state)));
            if holder_done && !self.holder_alive(&c.holder_id) {
                let dirty = self.git(&c.holder_path, &["status", "--porcelain"]);
                if dirty.ok() && dirty.stdout.trim().is_empty() {
                    if let Some(root) = self.repo_root_cmd(&c.repo) {
                        let why = format!(
                            "branch collision: {} is closed and clean, squatting {}, blocking {}",
                            c.holder_id, c.branch, c.id
                        );
                        if self.destroy_worktree(&c.holder_id, &c.holder_path, &root, &why) {
                            out.push(ParkOutcome::Freed {
                                id: c.id.clone(),
                                repo: c.repo.clone(),
                                branch: c.branch.clone(),
                                holder_id: c.holder_id.clone(),
                                holder_path: c.holder_path.clone(),
                            });
                            continue;
                        }
                    }
                }
            }

            self.bd().quiet(self.h, &["label", "add", &c.id, &self.cfg.ask], None);
            self.bd().quiet(self.h, &["label", "add", &c.id, "overseer"], None);
            self.lc_hold(
                &c.id,
                "ask",
                &format!("branch {} squatted by {}'s worktree at {}", c.branch, c.holder_id, c.holder_path),
            );
            let note = format!(
                "Parked by detect_branch_collisions: recorded branch {} is checked out in {}'s worktree at {}, not this bead's own canonical path. Every summon reaches aeon.sh's law-one-aeon-one-worktree refusal (or a no-op self-correct, when this bead's own default branch is the squatted one) before a session can start, and nothing about the input changes on retry. Labeled {} and overseer so dispatch stops spending a claim here — free {} or correct the branch: label, then remove {}.",
                c.branch, c.holder_id, c.holder_path, self.cfg.ask, c.holder_path, self.cfg.ask
            );
            self.bd().quiet(self.h, &["note", &c.id, "--stdin"], Some(&note));
        }
        out
    }
}

fn is_git_dir(root: &str) -> bool {
    let p = std::path::Path::new(root).join(".git");
    p.is_dir() || p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORCELAIN: &str = "worktree /r/wt/sp-hold\nHEAD abc123\nbranch refs/heads/spira/sp-root\n\nworktree /r/wt/sp-fine\nHEAD def456\nbranch refs/heads/spira/sp-fine\n\nworktree /r/repo\nHEAD 000\ndetached\n\n";

    #[test]
    fn porcelain_pairs_branch_with_its_own_worktree_and_skips_detached() {
        let got = parse_worktree_porcelain(PORCELAIN);
        assert_eq!(
            got,
            vec![
                ("refs/heads/spira/sp-root".to_string(), "/r/wt/sp-hold".to_string()),
                ("refs/heads/spira/sp-fine".to_string(), "/r/wt/sp-fine".to_string()),
            ]
        );
    }

    #[test]
    fn find_holder_fires_on_a_squatted_branch_and_stays_quiet_on_a_free_one() {
        let list = parse_worktree_porcelain(PORCELAIN);
        assert_eq!(find_holder(&list, "spira/sp-root"), Some("/r/wt/sp-hold".to_string()));
        assert_eq!(find_holder(&list, "spira/sp-nobody"), None);
    }

    #[test]
    fn collision_line_round_trips_through_parse() {
        let c = Collision {
            id: "sp-root".into(),
            repo: "fixture".into(),
            branch: "spira/sp-root".into(),
            holder_id: "sp-hold".into(),
            holder_path: "/r/worktree/sp-hold".into(),
        };
        assert_eq!(c.line(), "COLLISION sp-root fixture spira/sp-root sp-hold /r/worktree/sp-hold");
        assert_eq!(Collision::parse_line(&c.line()), Some(c));
        assert_eq!(Collision::parse_line("not a collision line"), None);
    }

    #[test]
    fn park_outcome_lines_match_lib_sh_exactly() {
        assert_eq!(
            ParkOutcome::Unlabeled {
                id: "sp-child".into(),
                repo: "fixture".into(),
                branch: "spira/sp-root".into(),
                from: "sp-root".into(),
            }
            .line(),
            "UNLABELED sp-child fixture spira/sp-root sp-root"
        );
        assert_eq!(
            ParkOutcome::Freed {
                id: "sp-root3".into(),
                repo: "fixture".into(),
                branch: "spira/sp-root3".into(),
                holder_id: "sp-hold3".into(),
                holder_path: "/r/worktree/sp-hold3".into(),
            }
            .line(),
            "FREED sp-root3 fixture spira/sp-root3 sp-hold3 /r/worktree/sp-hold3"
        );
    }

    #[test]
    fn parts_line_skips_a_fayth_with_no_labels() {
        assert_eq!(parts_line("builder", "spira,plan", "spira-poison"), Some("builder|spira,plan|spira-poison\n".to_string()));
        assert_eq!(parts_line("human", "", ""), None, "a manual-summon persona contributes no partition");
    }
}
