//! The members of a publish range (DESIGN.md §8 D4, sp-bauwt): derived from local/main's
//! own `spira: land <id>` commits in forge..local — a fact nothing can reap — with the
//! lifecycle row consulted only for a member's tip.

use std::collections::BTreeMap;

use crate::model::{LcBeadRow, Member, RangeCommit};

/// Parse `git log -z --format=%H%x1f%P%x1f%s <range>` output.
pub fn parse_log(raw: &str) -> Vec<RangeCommit> {
    raw.split('\0')
        .map(|r| r.trim_start_matches('\n'))
        .filter(|r| !r.is_empty())
        .filter_map(|r| {
            let mut f = r.splitn(3, '\u{1f}');
            let sha = f.next()?.trim().to_string();
            let parents = f
                .next()
                .unwrap_or("")
                .split_whitespace()
                .map(String::from)
                .collect();
            let subject = f.next().unwrap_or("").trim_end_matches('\n').to_string();
            (!sha.is_empty()).then_some(RangeCommit {
                sha,
                parents,
                subject,
            })
        })
        .collect()
}

/// The bead id a land commit names: `spira: land <id>` optionally followed by ` — <title>`
/// (lib.sh land_subject) — the id is the token after the prefix.
pub fn land_id(subject: &str) -> Option<&str> {
    let rest = subject.strip_prefix("spira: land ")?;
    let id = rest.split_whitespace().next()?;
    (!id.is_empty()).then_some(id)
}

/// The names to ask the store about: every land id, plus the text before the first `:` of each
/// other subject when it is a well-formed bead id. A candidate is only a name; the store says
/// whether the bead landed (law-landed-is-content).
pub fn candidate_ids(commits: &[RangeCommit]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in commits {
        let id = land_id(&c.subject).or_else(|| {
            let (head, _) = c.subject.split_once(':')?;
            crate::ident::check("bead id", head).ok().map(|_| head)
        });
        if let Some(id) = id {
            if !out.iter().any(|o| o == id) {
                out.push(id.to_string());
            }
        }
    }
    out
}

/// Members, oldest first, one per id (a re-landed id keeps its newest commit).
///
/// `commits` is newest-first (git log's order). A land commit makes its id a member; its tip is,
/// in order of preference: its LANDED row's tip when `tip_in_range` says it is in the range; else
/// the land merge's second parent; else the land commit itself. A candidate with no land commit
/// is a member only when its row is LANDED at a tip in the range (a hand-landing), and that tip
/// is its tip.
pub fn members(
    commits: &[RangeCommit],
    rows: &BTreeMap<String, LcBeadRow>,
    tip_in_range: &dyn Fn(&str) -> bool,
) -> Vec<Member> {
    let row_tip = |id: &str| {
        rows.get(id)
            .filter(|r| r.state == "LANDED")
            .and_then(|r| r.tip.clone())
            .filter(|t| !t.is_empty() && t != "none" && tip_in_range(t))
    };
    let landed_by_commit: std::collections::BTreeSet<&str> =
        commits.iter().filter_map(|c| land_id(&c.subject)).collect();
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for c in commits {
        let (id, tip) = if let Some(id) = land_id(&c.subject) {
            let tip = row_tip(id)
                .unwrap_or_else(|| c.parents.get(1).cloned().unwrap_or_else(|| c.sha.clone()));
            (id, tip)
        } else {
            let Some((id, _)) = c.subject.split_once(':') else {
                continue;
            };
            if landed_by_commit.contains(id) {
                continue;
            }
            let Some(tip) = row_tip(id) else {
                continue;
            };
            (id, tip)
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        out.push(Member {
            id: id.to_string(),
            tip,
        });
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(sha: &str, parents: &[&str], subject: &str) -> RangeCommit {
        RangeCommit {
            sha: sha.into(),
            parents: parents.iter().map(|s| s.to_string()).collect(),
            subject: subject.into(),
        }
    }

    #[test_id(t2p4p6y)]
    #[test]
    fn parses_git_log_records() {
        let raw = "aaa\u{1f}p1 p2\u{1f}spira: land sp-x — title\0\nbbb\u{1f}p3\u{1f}round 1: fix\0";
        let v = parse_log(raw);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].parents, vec!["p1", "p2"]);
        assert_eq!(v[1].subject, "round 1: fix");
    }

    #[test_id(tmzbjdz)]
    #[test]
    fn land_id_reads_titled_and_bare_subjects() {
        assert_eq!(land_id("spira: land sp-atbfa"), Some("sp-atbfa"));
        assert_eq!(
            land_id("spira: land sp-s088v.5 — some title"),
            Some("sp-s088v.5")
        );
        assert_eq!(land_id("spira: format batch"), None);
        assert_eq!(land_id("round 122: x"), None);
    }

    #[test_id(tabm1bz)]
    #[test]
    fn members_survive_missing_lifecycle_rows() {
        let commits = vec![
            c("m3", &["m2", "t3"], "spira: land sp-c — c"),
            c("r2", &["m2"], "round 2: concierge fix"),
            c("m2", &["m1", "t2"], "spira: land sp-b"),
            c("m1", &["f0", "t1"], "spira: land sp-a"),
        ];
        let ms = members(&commits, &BTreeMap::new(), &|_| true);
        let got: Vec<_> = ms.iter().map(|m| m.render()).collect();
        assert_eq!(got, vec!["sp-a:t1", "sp-b:t2", "sp-c:t3"]);
    }

    #[test_id(tpikukp)]
    #[test]
    fn row_tip_wins_only_when_in_range() {
        let commits = vec![c("m1", &["f0", "t1"], "spira: land sp-a")];
        let mut ls = BTreeMap::new();
        ls.insert(
            "sp-a".to_string(),
            LcBeadRow {
                bead_id: "sp-a".into(),
                state: "LANDED".into(),
                tip: Some("real".into()),
                since: Some(1),
                blocked_by: Vec::new(),
                reason: None,
                holds: Vec::new(),
                stack: Default::default(),
                ejected_red_tip: None,
                holder: None,
                repo: None,
            },
        );
        assert_eq!(members(&commits, &ls, &|t| t == "real")[0].tip, "real");
        assert_eq!(members(&commits, &ls, &|_| false)[0].tip, "t1");
        ls.get_mut("sp-a").unwrap().state = "CERTIFIED".into();
        assert_eq!(members(&commits, &ls, &|_| true)[0].tip, "t1");
    }

    #[test_id(t4t3xm7)]
    #[test]
    fn a_single_parent_land_commit_is_its_own_tip_and_duplicates_collapse() {
        let commits = vec![
            c("n2", &["n1"], "spira: land sp-a"),
            c("n1", &["n0"], "spira: land sp-a"),
        ];
        let ms = members(&commits, &BTreeMap::new(), &|_| true);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].tip, "n2");
    }

    fn landed(id: &str, tip: &str, state: &str) -> (String, LcBeadRow) {
        (
            id.to_string(),
            LcBeadRow {
                bead_id: id.into(),
                state: state.into(),
                tip: Some(tip.into()),
                since: Some(1),
                blocked_by: Vec::new(),
                reason: None,
                holds: Vec::new(),
                stack: Default::default(),
                ejected_red_tip: None,
                holder: None,
                repo: None,
            },
        )
    }

    fn hand_landed_range() -> Vec<RangeCommit> {
        vec![
            c("c3", &["c2"], "sp-b: x"),
            c("c2", &["c1", "z"], "stacked base: merge sp-a (y)"),
            c("c1", &["c0"], "sp-a: y"),
        ]
    }

    #[test_id(t03v9yi)]
    #[test]
    fn hand_landed_beads_are_members_at_their_row_tips() {
        let rows: BTreeMap<_, _> =
            [landed("sp-a", "c1", "LANDED"), landed("sp-b", "c3", "LANDED")].into();
        let ms = members(&hand_landed_range(), &rows, &|_| true);
        let got: Vec<_> = ms.iter().map(|m| m.render()).collect();
        assert_eq!(got, vec!["sp-a:c1", "sp-b:c3"]);
        assert_eq!(
            candidate_ids(&hand_landed_range()),
            vec!["sp-b", "sp-a"]
        );
    }

    #[test_id(tkpgolv)]
    #[test]
    fn a_candidate_not_landed_in_range_is_no_member() {
        let commits = hand_landed_range();
        let certified: BTreeMap<_, _> = [landed("sp-a", "c1", "CERTIFIED")].into();
        assert!(members(&commits, &certified, &|_| true).is_empty());
        let landed_out: BTreeMap<_, _> = [landed("sp-a", "c1", "LANDED")].into();
        assert!(members(&commits, &landed_out, &|_| false).is_empty());
        assert!(members(&commits, &BTreeMap::new(), &|_| true).is_empty());
    }

    #[test_id(ts083v9)]
    #[test]
    fn no_land_commit_is_no_member() {
        assert!(members(&[c("x", &["y"], "round 3: x")], &BTreeMap::new(), &|_| true).is_empty());
    }
}
