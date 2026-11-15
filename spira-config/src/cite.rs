//! The bead-citation rule: which tokens in a commit message count as citing a bead, and which
//! of those the store does not hold. `work submit` and the landing gate both apply it.

/// A commit that cites an id the store does not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phantom {
    pub sha: String,
    pub id: String,
}

/// Asks the store which of the ids it holds; `Err` is the store not answering.
pub type Lookup<'a> = dyn FnMut(&[String]) -> Result<Vec<String>, String> + 'a;

fn id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The distinct bead ids `message` cites, sorted. A citation is `<prefix>-` and three or more
/// lowercase base36 characters (the allocator's shape), then optional `.<digits>` children, as a
/// whole token: not inside `a-sp-abc` or `sp-abc-def`.
pub fn citations(message: &str, prefix: &str) -> Vec<String> {
    let head = format!("{prefix}-");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = message[from..].find(&head) {
        let start = from + at;
        from = start + head.len();
        if message[..start].chars().next_back().is_some_and(|c| id_char(c) || c == '-') {
            continue;
        }
        let rest = &message[from..];
        let word = rest.bytes().take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit()).count();
        let mut end = word;
        loop {
            let tail = &rest[end..];
            let digits = tail.strip_prefix('.').map_or(0, |t| t.bytes().take_while(u8::is_ascii_digit).count());
            if digits == 0 {
                break;
            }
            end += 1 + digits;
        }
        let after = &rest[end..];
        let mut chars = after.chars();
        let ends_cleanly = match chars.next() {
            None => true,
            Some(c) if id_char(c) => false,
            Some('-') => !chars.next().is_some_and(|c| c.is_ascii_alphanumeric()),
            Some(_) => true,
        };
        if word >= 3 && ends_cleanly {
            out.push(message[start..from + end].to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Every `(sha, id)` where the commit cites an id `known` does not return. `known` is asked once,
/// for every id cited; its `Err` is the store not answering, never "all exist".
pub fn phantoms(
    commits: &[(String, String)],
    prefix: &str,
    known: &mut Lookup<'_>,
) -> Result<Vec<Phantom>, String> {
    let cited: Vec<(&str, Vec<String>)> = commits.iter().map(|(sha, msg)| (sha.as_str(), citations(msg, prefix))).collect();
    let mut all: Vec<String> = cited.iter().flat_map(|(_, ids)| ids.iter().cloned()).collect();
    all.sort();
    all.dedup();
    if all.is_empty() {
        return Ok(Vec::new());
    }
    let present = known(&all)?;
    Ok(cited
        .into_iter()
        .flat_map(|(sha, ids)| ids.into_iter().map(move |id| (sha, id)))
        .filter(|(_, id)| !present.contains(id))
        .map(|(sha, id)| Phantom { sha: sha.to_string(), id })
        .collect())
}

/// One line per phantom, then how to fix it.
pub fn refusal(phantoms: &[Phantom]) -> String {
    let mut out: String = phantoms
        .iter()
        .map(|p| format!("{} cites {}, which does not exist in the bead store\n", &p.sha[..p.sha.len().min(9)], p.id))
        .collect();
    out.push_str(
        "Reword the commit: `git commit --amend` for the tip, or `git rebase -i <landref>` and mark it `reword`. \
A test fixture id written in prose is not a bead: quote it in backticks or describe it, and cite only beads that exist.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The retired commit-cite.sh rule: any-length head. Kept so the red against it is a test.
    fn old_regex_hits(message: &str) -> Vec<String> {
        let mut out = Vec::new();
        for tok in message.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')) {
            let t = tok.trim_end_matches('.');
            if let Some(h) = t.strip_prefix("sp-") {
                if !h.is_empty() && h.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
                    out.push(t.to_string());
                }
            }
        }
        out
    }

    #[test]
    fn the_old_rule_counted_fixture_ids() {
        assert_eq!(old_regex_hits("answers sp-a claimable and sp-b blocked"), ["sp-a", "sp-b"]);
    }

    #[test]
    fn fixture_ids_in_prose_are_not_citations() {
        assert!(citations("a claim view that answers sp-a claimable and sp-b blocked", "sp").is_empty());
    }

    #[test]
    fn allocator_shaped_tokens_are_citations() {
        let got = citations("sp-zzzzzz and sp-abc, also sp-boo22.2 and sp-vx3oi. done", "sp");
        assert_eq!(got, ["sp-abc", "sp-boo22.2", "sp-vx3oi", "sp-zzzzzz"]);
    }

    #[test]
    fn tokens_that_are_not_whole_ids_are_not_citations() {
        for m in ["sp-ow-mail", "xsp-abcdef", "SP-ABCDEF", "sp-ab", "sp-abcDEF", "sp-abc_x"] {
            assert!(citations(m, "sp").is_empty(), "{m}: {:?}", citations(m, "sp"));
        }
    }

    #[test]
    fn a_configured_prefix_is_used() {
        assert_eq!(citations("zz-abcd sp-abcd", "zz"), ["zz-abcd"]);
    }

    fn commits() -> Vec<(String, String)> {
        vec![("0123456789abcdef".into(), "sp-real1: x\n\nsee sp-zzzzzz".into())]
    }

    #[test]
    fn a_cited_id_the_store_lacks_is_a_phantom_with_its_sha() {
        let got = phantoms(&commits(), "sp", &mut |_| Ok(vec!["sp-real1".to_string()])).unwrap();
        assert_eq!(got, [Phantom { sha: "0123456789abcdef".into(), id: "sp-zzzzzz".into() }]);
        let text = refusal(&got);
        assert!(text.contains("012345678 cites sp-zzzzzz, which does not exist in the bead store") && text.contains("Reword"), "{text}");
    }

    #[test]
    fn a_store_that_cannot_answer_is_an_error_not_a_pass() {
        assert!(phantoms(&commits(), "sp", &mut |_| Err("down".into())).is_err());
    }

    #[test]
    fn a_message_citing_nothing_asks_the_store_nothing() {
        let c = vec![("a".to_string(), "no ids sp-a".to_string())];
        assert_eq!(phantoms(&c, "sp", &mut |_| Err("asked".into())).unwrap(), []);
    }
}
