//! One GitHub issue as gh-intake cares about it, and the parse from the REST API's JSON.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhIssue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub login: String,
    pub labels: Vec<String>,
    pub association: String,
}

/// Parses one page's body as the `GET /repos/:r/issues` array. Mirrors gh-intake.sh's
/// python exactly for the two cases it distinguishes: a body that parses as a JSON
/// *object* is GitHub's own error shape (`{"message": "..."}`, e.g. a 404 for a typo'd
/// repo), returned as `Err` carrying that message; a body that does not parse as JSON at
/// all is `Err` with a fixed refusal, same as the bash `ERR` case.
///
/// A JSON *array* is the only success case; pull requests are dropped inline (GitHub's
/// Issues API returns PRs too, flagged by a `pull_request` key) and issues missing a
/// `number` are skipped — one intended difference from the bash, named in DESIGN.md
/// ("Parity — named differences"): the python there would raise an uncaught KeyError and
/// the whole page would silently contribute nothing, which is worse than skipping just the
/// one malformed entry.
pub fn parse_issues_page(body: &[u8]) -> Result<Vec<GhIssue>, String> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| "the API returned something that is not JSON — refusing to guess at it".to_string())?;
    match v {
        serde_json::Value::Object(ref o) => {
            let msg = o.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
            Err(format!("the API refused: {msg}"))
        }
        serde_json::Value::Array(items) => {
            let mut out = Vec::new();
            for i in items {
                if i.get("pull_request").is_some() {
                    continue;
                }
                let Some(number) = i.get("number").and_then(|n| n.as_u64()) else { continue };
                let title = i.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                let body = i.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string();
                let login = i
                    .get("user")
                    .and_then(|u| u.get("login"))
                    .and_then(|l| l.as_str())
                    .unwrap_or("")
                    .to_string();
                let labels = i
                    .get("labels")
                    .and_then(|l| l.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|l| l.get("name").and_then(|n| n.as_str()).map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let association = i
                    .get("author_association")
                    .and_then(|a| a.as_str())
                    .unwrap_or("NONE")
                    .to_string();
                out.push(GhIssue { number, title, body, login, labels, association });
            }
            Ok(out)
        }
        _ => Err("the API returned something that is not JSON — refusing to guess at it".to_string()),
    }
}

/// `len` as gh-intake.sh's pager reads it: the array length that decides whether to keep
/// paging (`n >= 100`). Kept separate from `parse_issues_page` because the pager must
/// decide this from ONE page at a time, before pull requests are dropped and before pages
/// are combined — dropping PRs first would undercount a full page and stop paging early.
pub fn page_len(body: &[u8]) -> Result<usize, String> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| "the API returned something that is not JSON — refusing to guess at it".to_string())?;
    match v {
        serde_json::Value::Object(ref o) => {
            let msg = o.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
            Err(format!("the API refused: {msg}"))
        }
        serde_json::Value::Array(items) => Ok(items.len()),
        _ => Err("the API returned something that is not JSON — refusing to guess at it".to_string()),
    }
}

/// Trust by GitHub's own access control (law-work-enters-only-from-the-operator):
/// OWNER, MEMBER or COLLABORATOR is trusted outright.
pub fn association_trusted(assoc: &str) -> bool {
    matches!(assoc, "OWNER" | "MEMBER" | "COLLABORATOR")
}

/// `spira:accept` as a whole label, not a substring — the bash matched
/// `case " $gh_labels " in *" spira:accept "*)`, which is exact-token matching over a
/// space-joined list; this is the same test over the parsed label vector.
pub fn has_accept_label(labels: &[String]) -> bool {
    labels.iter().any(|l| l == "spira:accept")
}

/// `permission` field of the collaborators/permission endpoint: admin, maintain or write
/// grants access; anything else, or a body that doesn't parse, does not
/// (law-a-pattern-match-is-not-an-identity-check — the label alone is never enough).
pub fn permission_grants_access(body: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else { return false };
    matches!(
        v.get("permission").and_then(|p| p.as_str()),
        Some("admin") | Some("maintain") | Some("write")
    )
}

/// The `actor.login` of the LAST `labeled` event applying `spira:accept`, from the issue's
/// events timeline. Mirrors the bash's fold-and-overwrite loop: if the label was applied,
/// removed and reapplied by different people, the most recent applier is the one checked.
pub fn last_accept_actor(events_body: &[u8]) -> String {
    let Ok(serde_json::Value::Array(events)) = serde_json::from_slice::<serde_json::Value>(events_body) else {
        return String::new();
    };
    let mut actor = String::new();
    for ev in events {
        if ev.get("event").and_then(|e| e.as_str()) != Some("labeled") {
            continue;
        }
        let label = ev.get("label").and_then(|l| l.get("name")).and_then(|n| n.as_str()).unwrap_or("");
        if label != "spira:accept" {
            continue;
        }
        actor = ev.get("actor").and_then(|a| a.get("login")).and_then(|l| l.as_str()).unwrap_or("").to_string();
    }
    actor
}
