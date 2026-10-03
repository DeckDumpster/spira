//! CHECK 5 — closed but not landed (audit worker). A closed work bead must be provably on
//! its base: by a landing subject, by its branch's ancestry, or by a LANDED landstate tip.
//! Otherwise one Ops incident, bounded per pass by count and by time; an incident this check
//! filed is resolved the moment the same proof appears (law-closed-is-not-landed).

use std::collections::HashSet;

use sha2::{Digest, Sha256};

use crate::host::{Io, Spec};
use crate::model::LandState;
use crate::pass::Sentinel;
use crate::store::Snapshot;

/// The base's subjects → the ids they prove landed (the awk in sentinel.sh):
/// `spira: land <id> …`, `Merge branch 'spira/<id>' …` (not round-*, no spaces), `<id>: …`.
pub fn landed_ids(subjects: &str) -> HashSet<String> {
    let mut s = HashSet::new();
    for line in subjects.lines() {
        if let Some(r) = line.strip_prefix("spira: land ") {
            if let Some(t) = r.split_whitespace().next() {
                s.insert(t.to_string());
            }
            continue;
        }
        if let Some(r) = line.strip_prefix("Merge branch 'spira/") {
            let r = r.split('\'').next().unwrap_or("");
            if !r.is_empty() && !r.contains(' ') && !r.starts_with("round-") {
                s.insert(r.to_string());
            }
            continue;
        }
        let tok_end = line.find([':', ' ']).unwrap_or(line.len());
        if tok_end > 0 && line[tok_end..].starts_with(':') {
            s.insert(line[..tok_end].to_string());
        }
    }
    s
}

/// The dedup ref incident.sh labels a closed-not-landed incident with.
pub fn ref_hash(id: &str) -> String {
    let d = Sha256::digest(format!("closed-not-landed:{id}").as_bytes());
    d.iter().take(4).fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

struct Budget {
    start: i64,
    secs: i64,
    hit: bool,
}

impl<'a> Sentinel<'a> {
    pub fn check5(&self, snap: &Snapshot) {
        let incidents = snap.incidents(&self.cfg.incident_label);
        let rows = snap.closed_work(
            &self.ctx.partitions,
            &self.cfg.work_types,
            &self.cfg.home_repo,
        );
        let mut budget = Budget {
            start: self.h.now(),
            secs: self.cfg.c5_budget,
            hit: false,
        };
        let (mut filed, mut capped, mut graph, mut resolved, mut resolve_capped) =
            (0u32, 0u32, 0u32, 0u32, 0u32);
        let (mut capped_ids, mut resolve_capped_ids): (Vec<String>, Vec<String>) =
            (Vec::new(), Vec::new());
        let mut absent: HashSet<String> = HashSet::new();
        let mut cur: Option<(String, Option<String>, HashSet<String>)> = None; // (root, base, landed)

        let mut resolve = |this: &Self, id: &str, evidence: &str, budget: &mut Budget| {
            let Some(inc) = incidents.get(&ref_hash(id)) else {
                return;
            };
            if resolved >= this.cfg.c5_max_resolve {
                resolve_capped += 1;
                resolve_capped_ids.push(id.to_string());
                return;
            }
            if this.h.now() - budget.start >= budget.secs {
                budget.hit = true;
                resolve_capped += 1;
                resolve_capped_ids.push(id.to_string());
                return;
            }
            let o = this.bd().call(
                this.h,
                &["close", "--force", inc, "--reason-file", "-"],
                Some(&format!("{evidence}\n")),
            );
            if o.ok() {
                resolved += 1;
                this.log(&format!("CHECK5 resolve {id} -> {inc}: {evidence}"));
            } else {
                let err = o.stderr.trim_end_matches('\n');
                let err = if err.is_empty() {
                    "bd close exited nonzero with no message"
                } else {
                    err
                };
                this.log(&format!("CHECK5 resolve {id} -> {inc} FAILED: {err}"));
            }
        };

        for r in &rows {
            if !self.cfg.run.join(format!("{}.log", r.id)).is_file() || r.exempt() {
                continue;
            }
            let Some(root) = self.ctx.repo(&r.repo).and_then(|x| x.root.clone()) else {
                if absent.insert(r.repo.clone()) {
                    self.log(&format!(
                        "CHECK5: repo:{} is not in repo-map — skipping its closed beads",
                        r.repo
                    ));
                }
                continue;
            };
            if cur.as_ref().map(|c| c.0.as_str()) != Some(root.as_str()) {
                let base = self
                    .ctx
                    .repo(&r.repo)
                    .and_then(|x| x.base().map(str::to_string));
                let landed = match &base {
                    Some(b) => landed_ids(&self.git(&root, &["log", "--format=%s", b]).stdout),
                    None => HashSet::new(),
                };
                cur = Some((root.clone(), base, landed));
            }
            let (_, base, landed) = cur.as_ref().unwrap();
            let Some(base) = base.clone() else {
                self.log(&format!(
                    "CHECK5 {}: cannot resolve the ref {} lands on — not judging whether it landed",
                    r.id, r.repo
                ));
                continue;
            };
            if landed.contains(&r.id) {
                graph += 1;
                resolve(self, &r.id, &format!("{} is landed: {}'s base ({base}) names it in a 'spira: land' or '<id>:' subject, proven by the same commit-graph walk that filed this incident (law-closed-is-not-landed).", r.id, r.repo), &mut budget);
                continue;
            }
            if !r.branch.is_empty()
                && self
                    .git(
                        &root,
                        &[
                            "show-ref",
                            "--verify",
                            "-q",
                            &format!("refs/heads/{}", r.branch),
                        ],
                    )
                    .ok()
                && self
                    .git(
                        &root,
                        &[
                            "merge-base",
                            "--is-ancestor",
                            &format!("refs/heads/{}", r.branch),
                            &base,
                        ],
                    )
                    .ok()
            {
                resolve(self, &r.id, &format!("{} is landed: its recorded branch ({}) tip is an ancestor of {}'s base ({base}), proven directly by the commit graph — no commit subject names it and no landstate record exists (law-closed-is-not-landed).", r.id, r.branch, r.repo), &mut budget);
                continue;
            }
            let cited = r.cited_shas.iter().find(|sha| {
                self.git(&root, &["merge-base", "--is-ancestor", &format!("{sha}^{{commit}}"), &base]).ok()
            });
            if let Some(sha) = cited {
                resolve(self, &r.id, &format!("{} is landed: its close reason cites {sha}, an ancestor of {}'s base ({base}), proven by the commit graph (law-closed-is-not-landed).", r.id, r.repo), &mut budget);
                continue;
            }
            let ls = std::fs::read_to_string(self.cfg.run.join("landstate").join(&r.id))
                .map(|t| LandState::parse(&t))
                .unwrap_or_default();
            if let Some(tip) = ls.landed_tip() {
                if self
                    .git(&root, &["merge-base", "--is-ancestor", tip, &base])
                    .ok()
                {
                    resolve(self, &r.id, &format!("{} is landed: its LANDED landstate tip ({tip}) is an ancestor of {}'s base ({base}).", r.id, r.repo), &mut budget);
                    continue;
                }
            }
            if filed >= self.cfg.c5_max_file {
                capped += 1;
                capped_ids.push(r.id.clone());
                continue;
            }
            if self.h.now() - budget.start >= budget.secs {
                budget.hit = true;
                capped += 1;
                capped_ids.push(r.id.clone());
                continue;
            }
            filed += 1;
            self.log(&format!(
                "CHECK5 {}: closed with no LANDED record on {} ({base}) — filing an Ops incident",
                r.id, r.repo
            ));
            let or_none = |s: &str| {
                if s.is_empty() {
                    "none".to_string()
                } else {
                    s.to_string()
                }
            };
            let body = format!(
                "landstate={} tip={} base={base} repo={}. The landing pass closes work beads when their commit reaches the base (bead_close_on_land, lib.sh); this bead is closed with no such record. Check the landing pass's own log before assuming the work is missing.\n",
                or_none(&ls.state),
                or_none(&ls.tip),
                r.repo
            );
            let scope = if self.cfg.scope.is_empty() {
                "spira".to_string()
            } else {
                self.cfg.scope.clone()
            };
            let _ = self.h.run(
                Spec::args_owned(
                    "bash",
                    vec![
                        self.cfg.incident_sh.to_string_lossy().into_owned(),
                        "file".into(),
                        format!(
                            "CLOSED NOT LANDED: {} has no LANDED record on {}",
                            r.id, r.repo
                        ),
                        "-".into(),
                    ],
                )
                .env("SPIRA_DB", self.cfg.db.clone())
                .env("SPIRA_INCIDENT_TYPE", "bug")
                .env("SPIRA_INCIDENT_PRIORITY", "1")
                .env("SPIRA_INCIDENT_ACTOR", "sentinel")
                .env(
                    "SPIRA_INCIDENT_LABELS",
                    format!("{scope},{}", self.cfg.incident_label),
                )
                .env("SPIRA_INCIDENT_REPO", scope.clone())
                .env("SPIRA_INCIDENT_REF", format!("closed-not-landed:{}", r.id))
                .env("SPIRA_INCIDENT_CAUSE", "closed-not-landed")
                .env("SPIRA_INCIDENT_PATH", format!("closed-not-landed:{}", r.id))
                .stdin(body.into_bytes())
                .out(Io::Null)
                .err(Io::Null),
            );
        }

        if self.ctx.partitions.is_empty() {
            self.log("CHECK5 no persona in the chamber declares a partition — no closed bead is being checked for landing");
        }
        if graph > 0 {
            self.log(&format!("CHECK5: {graph} closed bead(s) with no LANDED record proven landed by the base's commit graph"));
        }
        if capped > 0 {
            self.log(&format!(
                "CHECK5: filed {filed} incident(s), the cap (SPIRA_CHECK5_MAX_FILE={}); {capped} more not filed this pass: {}",
                self.cfg.c5_max_file,
                capped_ids.join(" ")
            ));
        }
        if resolved > 0 {
            self.log(&format!(
                "CHECK5: resolved {resolved} incident(s) for beads this pass proved landed"
            ));
        }
        if resolve_capped > 0 {
            self.log(&format!(
                "CHECK5: resolved {resolved} incident(s), the cap (SPIRA_CHECK5_MAX_RESOLVE={}); {resolve_capped} more proven landed but not resolved this pass: {}",
                self.cfg.c5_max_resolve,
                resolve_capped_ids.join(" ")
            ));
        }
        if budget.hit {
            self.log(&format!(
                "CHECK5: pass budget exhausted (SPIRA_CHECK5_BUDGET_SECS={}) — remaining filings/resolves deferred to the next pass",
                self.cfg.c5_budget
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_that_prove_a_landing() {
        let s = landed_ids(
            "spira: land sp-a — title here\nspira: land sp-b\nMerge branch 'spira/sp-c' into main\nMerge branch 'spira/round-7'\nMerge branch 'spira/a b'\nsp-d: fix thing\nfix: not a bead but the awk takes it\nno colon here\nhas space: before colon\n",
        );
        for id in ["sp-a", "sp-b", "sp-c", "sp-d", "fix"] {
            assert!(s.contains(id), "{id}");
        }
        assert!(!s.contains("round-7") && !s.contains("a b") && !s.contains("has"));
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn ref_hash_is_sha256_prefix() {
        // printf '%s' "closed-not-landed:sp-x" | sha256sum | cut -c1-8
        let want = {
            let o = std::process::Command::new("sh")
                .args([
                    "-c",
                    "printf '%s' closed-not-landed:sp-x | sha256sum | cut -c1-8",
                ])
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        assert_eq!(ref_hash("sp-x"), want);
        assert_eq!(ref_hash("sp-x").len(), 8);
    }
}
