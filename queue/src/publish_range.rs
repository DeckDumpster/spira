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
            let parents = f.next().unwrap_or("").split_whitespace().map(String::from).collect();
            let subject = f.next().unwrap_or("").trim_end_matches('\n').to_string();
            (!sha.is_empty()).then_some(RangeCommit { sha, parents, subject })
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

/// Members, oldest land first, one per id (a re-landed id keeps its newest commit).
///
/// `commits` is newest-first (git log's order). A member's tip is, in order of preference:
/// its LANDED lifecycle-row tip when `tip_in_range` says that tip is in the range (the tip
/// land-local delivered); else the land merge's second parent (the member branch it merged);
/// else the land commit itself (a fast-forwarded single-commit round).
pub fn members(
    commits: &[RangeCommit],
    rows: &BTreeMap<String, LcBeadRow>,
    tip_in_range: &dyn Fn(&str) -> bool,
) -> Vec<Member> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for c in commits {
        let Some(id) = land_id(&c.subject) else { continue };
        if !seen.insert(id.to_string()) {
            continue;
        }
        let from_state = rows
            .get(id)
            .filter(|r| r.state == "LANDED")
            .and_then(|r| r.tip.clone())
            .filter(|t| !t.is_empty() && t != "none" && tip_in_range(t));
        let tip = from_state.unwrap_or_else(|| c.parents.get(1).cloned().unwrap_or_else(|| c.sha.clone()));
        out.push(Member { id: id.to_string(), tip });
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(sha: &str, parents: &[&str], subject: &str) -> RangeCommit {
        RangeCommit { sha: sha.into(), parents: parents.iter().map(|s| s.to_string()).collect(), subject: subject.into() }
    }

    #[test]
    fn parses_git_log_records() {
        let raw = "aaa\u{1f}p1 p2\u{1f}spira: land sp-x — title\0\nbbb\u{1f}p3\u{1f}round 1: fix\0";
        let v = parse_log(raw);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].parents, vec!["p1", "p2"]);
        assert_eq!(v[1].subject, "round 1: fix");
    }

    #[test]
    fn land_id_reads_titled_and_bare_subjects() {
        assert_eq!(land_id("spira: land sp-atbfa"), Some("sp-atbfa"));
        assert_eq!(land_id("spira: land sp-s088v.5 — some title"), Some("sp-s088v.5"));
        assert_eq!(land_id("spira: format batch"), None);
        assert_eq!(land_id("round 122: x"), None);
    }

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

    #[test]
    fn row_tip_wins_only_when_in_range() {
        let commits = vec![c("m1", &["f0", "t1"], "spira: land sp-a")];
        let mut ls = BTreeMap::new();
        ls.insert("sp-a".to_string(), LcBeadRow { bead_id: "sp-a".into(), state: "LANDED".into(), tip: Some("real".into()), since: Some(1), blocked_by: Vec::new() });
        assert_eq!(members(&commits, &ls, &|t| t == "real")[0].tip, "real");
        assert_eq!(members(&commits, &ls, &|_| false)[0].tip, "t1");
        ls.get_mut("sp-a").unwrap().state = "CERTIFIED".into();
        assert_eq!(members(&commits, &ls, &|_| true)[0].tip, "t1");
    }

    #[test]
    fn a_single_parent_land_commit_is_its_own_tip_and_duplicates_collapse() {
        let commits = vec![c("n2", &["n1"], "spira: land sp-a"), c("n1", &["n0"], "spira: land sp-a")];
        let ms = members(&commits, &BTreeMap::new(), &|_| true);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].tip, "n2");
    }

    #[test]
    fn no_land_commit_is_no_member() {
        assert!(members(&[c("x", &["y"], "round 3: x")], &BTreeMap::new(), &|_| true).is_empty());
    }
}
