//! What the scheduler and the round programs read from git, recorded for lc-serve to answer from,
//! so no served verb runs git. `git-fact put <kind> --json <body>` and `git-fact get <kind> <key…>`.
//! `unpublished` is per repo and exact for the tips it names; `range` is pinned to its two SHAs, so
//! a put of an existing key changes nothing.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use state_store::{Backend, Compare, Object, Request, Result, Txn};

use super::keys;
use super::store::Store;
use crate::{flag, CANNOT_TELL, REFUSED};

pub const VERB: &str = "git-fact";

/// The programs (a caller is `<program>` or `<program>/<subcommand>`) that may record a kind of fact:
/// the scheduler measures `unpublished`; the scheduler, the batcher and the queue it runs record `range`.
fn writers(kind: &str) -> &'static [&'static str] {
    match kind {
        "unpublished" => &["spira-runtime"],
        _ => &["spira-runtime", "batcher", "queue"],
    }
}

fn unpublished_key(repo: &str) -> String {
    format!("{}unpublished/{}", keys::GIT_FACTS, keys::component(repo))
}

fn range_key(repo: &str, base: &str, tip: &str) -> String {
    format!("{}range/{}/{}/{}", keys::GIT_FACTS, keys::component(repo), keys::component(base), keys::component(tip))
}

impl<B: Backend> Store<B> {
    pub fn git_fact_unpublished(&self, repo: &str) -> Result<Option<Value>> {
        Ok(self.contract().get(&unpublished_key(repo))?.map(|i| i.value.data))
    }

    pub fn put_git_fact_unpublished(&self, repo: &str, record: Value, req: &Request) -> Result<()> {
        self.contract().txn(&Txn::new().put(&unpublished_key(repo), Object::new(record)), req).map(|_| ())
    }

    /// The commits of `tip ^base` as recorded, or `None` when none was.
    pub fn git_fact_range(&self, repo: &str, base: &str, tip: &str) -> Result<Option<BTreeSet<String>>> {
        Ok(self.contract().get(&range_key(repo, base, tip))?.map(|i| {
            i.value.data["commits"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
        }))
    }

    /// A pinned range never changes: the put is refused by its own compare when the key exists.
    pub fn put_git_fact_range(&self, repo: &str, base: &str, tip: &str, commits: &[String], req: &Request) -> Result<()> {
        let key = range_key(repo, base, tip);
        let t = Txn::new().when(Compare::ModRev(key.clone(), None)).put(&key, Object::new(json!({ "commits": commits })));
        self.contract().txn(&t, req).map(|_| ())
    }
}

fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v[k].as_str().filter(|s| !s.is_empty())
}

pub fn run<B: Backend>(store: &Store<B>, args: &[String], request: &str, caller: &str) -> (i32, String) {
    let cannot = |why: String| (CANNOT_TELL, format!("cannot tell: {why}"));
    match (args.get(1).map(String::as_str), args.get(2).map(String::as_str)) {
        (Some("put"), Some(kind)) => {
            let program = caller.split('/').next().unwrap_or("");
            if !writers(kind).contains(&program) {
                return (REFUSED, format!("git-fact put {kind}: {caller} may not record it; only {} do", writers(kind).join(", ")));
            }
            let Some(body) = flag(args, "--json") else { return cannot("git-fact put: --json <body> is required".into()) };
            let Ok(v) = serde_json::from_str::<Value>(&body) else { return cannot("git-fact put: the body is not JSON".into()) };
            let req = Request::new(request, program, "lc.git-fact");
            let put = match kind {
                "unpublished" => put_unpublished(store, &v, &req),
                "range" => put_range(store, &v, &req),
                other => return cannot(format!("git-fact put: no kind {other:?} (unpublished, range)")),
            };
            match put {
                Ok(Ok(())) => (0, json!({ "recorded": kind }).to_string()),
                Ok(Err(why)) => cannot(format!("git-fact put {kind}: {why}")),
                Err(e) => cannot(e.to_string()),
            }
        }
        (Some("get"), Some("unpublished")) => match args.get(3) {
            Some(repo) => match store.git_fact_unpublished(repo) {
                Ok(Some(v)) => (0, v.to_string()),
                Ok(None) => cannot(format!("git-fact get unpublished {repo}: no record")),
                Err(e) => cannot(e.to_string()),
            },
            None => cannot("git-fact get unpublished <repo>".into()),
        },
        (Some("get"), Some("range")) => match (args.get(3), args.get(4), args.get(5)) {
            (Some(repo), Some(base), Some(tip)) => match store.git_fact_range(repo, base, tip) {
                Ok(Some(c)) => (0, json!(c).to_string()),
                Ok(None) => cannot(format!("git-fact get range {repo} {base} {tip}: no record")),
                Err(e) => cannot(e.to_string()),
            },
            _ => cannot("git-fact get range <repo> <base> <tip>".into()),
        },
        _ => cannot("usage: git-fact put <unpublished|range> --json <body> | get unpublished <repo> | get range <repo> <base> <tip>".into()),
    }
}

fn put_unpublished<B: Backend>(store: &Store<B>, v: &Value, req: &Request) -> Result<std::result::Result<(), String>> {
    let Some(repo) = text(v, "repo") else { return Ok(Err("repo is required".into())) };
    let record = if v["declared_unpublished"] == true {
        json!({ "declared_unpublished": true })
    } else {
        let measured = text(v, "landing_tip").is_some() && text(v, "forge_tip").is_some() && v["count"].is_u64() && v["read_at"].is_u64() && (v["oldest_arrival"].is_u64() || v["oldest_arrival"].is_null());
        if !measured {
            return Ok(Err("landing_tip, forge_tip, count, oldest_arrival and read_at are required".into()));
        }
        json!({ "landing_tip": v["landing_tip"], "forge_tip": v["forge_tip"], "count": v["count"], "oldest_arrival": v["oldest_arrival"], "read_at": v["read_at"] })
    };
    store.put_git_fact_unpublished(repo, record, req).map(Ok)
}

fn put_range<B: Backend>(store: &Store<B>, v: &Value, req: &Request) -> Result<std::result::Result<(), String>> {
    let (Some(repo), Some(base), Some(tip), Some(commits)) = (text(v, "repo"), text(v, "base"), text(v, "tip"), v["commits"].as_array()) else {
        return Ok(Err("repo, base, tip and commits are required".into()));
    };
    let commits: Vec<String> = commits.iter().filter_map(Value::as_str).map(str::to_string).collect();
    store.put_git_fact_range(repo, base, tip, &commits, req).map(Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports};

    fn store() -> Store<MemBackend> {
        let clock = VirtualClock::new(100_000);
        Store::new(Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap(), 1_000)
    }

    fn git_fact(s: &Store<MemBackend>, caller: &str, argv: &[&str]) -> (i32, String) {
        let args: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        run(s, &args, &format!("t/{argv:?}"), caller)
    }

    fn put(s: &Store<MemBackend>, kind: &str, body: Value) -> (i32, String) {
        git_fact(s, "spira-runtime", &["git-fact", "put", kind, "--json", &body.to_string()])
    }

    #[test_id(t23v4mt)]
    #[test]
    fn a_program_that_is_not_a_writer_is_refused_and_nothing_is_recorded() {
        let s = store();
        let body = json!({ "repo": "r", "declared_unpublished": true }).to_string();
        for caller in ["spira-lc", "-", "dwarf/work", "spira-runtimes"] {
            let (code, out) = git_fact(&s, caller, &["git-fact", "put", "unpublished", "--json", &body]);
            assert_eq!(code, REFUSED, "{caller}: {out}");
        }
        assert_eq!(s.git_fact_unpublished("r").unwrap(), None);
        assert_eq!(git_fact(&s, "spira-runtime/publish-backlog", &["git-fact", "put", "unpublished", "--json", &body]).0, 0);
    }

    #[test_id(ty8938n)]
    #[test]
    fn a_permitted_writer_putting_an_unknown_kind_is_told_the_kinds_and_a_stranger_is_refused() {
        let s = store();
        let argv = |caller: &str| git_fact(&s, caller, &["git-fact", "put", "bogus", "--json", "{}"]);
        let (code, out) = argv("batcher");
        assert_eq!(code, CANNOT_TELL);
        assert!(out.contains("no kind"), "{out}");
        assert_eq!(argv("dwarf").0, REFUSED);
    }

    #[test_id(tzvlqj4)]
    #[test]
    fn each_kind_is_recorded_only_by_the_programs_that_measure_it() {
        let s = store();
        let unpublished = json!({ "repo": "r", "declared_unpublished": true }).to_string();
        let range = json!({ "repo": "r", "base": "b", "tip": "t", "commits": [] }).to_string();
        for (caller, kind, body, code) in [
            ("batcher", "unpublished", &unpublished, REFUSED),
            ("queue", "unpublished", &unpublished, REFUSED),
            ("batcher", "range", &range, 0),
            ("queue/round", "range", &range, 0),
            ("spira-runtime", "range", &range, 0),
            ("dwarf", "range", &range, REFUSED),
        ] {
            assert_eq!(git_fact(&s, caller, &["git-fact", "put", kind, "--json", body]).0, code, "{caller} {kind}");
        }
    }

    #[test_id(tpgkllk)]
    #[test]
    fn an_unpublished_record_reads_back_and_a_second_put_replaces_it() {
        let s = store();
        let rec = |count: u64| json!({ "repo": "r", "landing_tip": "b", "forge_tip": "a", "count": count, "oldest_arrival": 5, "read_at": 9 });
        assert_eq!(put(&s, "unpublished", rec(2)).0, 0);
        assert_eq!(s.git_fact_unpublished("r").unwrap().unwrap()["count"], 2);
        assert_eq!(put(&s, "unpublished", rec(3)).0, 0);
        let (code, out) = git_fact(&s, "-", &["git-fact", "get", "unpublished", "r"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["count"], 3);
        assert_eq!(git_fact(&s, "-", &["git-fact", "get", "unpublished", "other"]).0, CANNOT_TELL);
    }

    #[test_id(tj6kxyb)]
    #[test]
    fn an_unpublished_body_missing_a_field_is_refused_not_stored_as_a_zero() {
        let s = store();
        let (code, out) = put(&s, "unpublished", json!({ "repo": "r", "landing_tip": "b", "forge_tip": "a", "read_at": 9 }));
        assert_eq!(code, CANNOT_TELL, "{out}");
        assert_eq!(s.git_fact_unpublished("r").unwrap(), None);
    }

    #[test_id(t77940l)]
    #[test]
    fn a_range_is_pinned_to_its_shas_and_a_second_put_of_the_key_changes_nothing() {
        let s = store();
        let body = |commits: &[&str]| json!({ "repo": "r", "base": "b", "tip": "t", "commits": commits });
        assert_eq!(put(&s, "range", body(&["c1", "c2"])).0, 0);
        assert_eq!(put(&s, "range", body(&["other"])).0, 0);
        assert_eq!(s.git_fact_range("r", "b", "t").unwrap(), Some(["c1", "c2"].map(String::from).into()));
        assert_eq!(s.git_fact_range("r", "b", "t2").unwrap(), None);
        let (code, out) = git_fact(&s, "-", &["git-fact", "get", "range", "r", "b", "t"]);
        assert_eq!((code, out.as_str()), (0, r#"["c1","c2"]"#));
    }

    #[test_id(t9w3b9f)]
    #[test]
    fn a_range_with_no_commits_is_a_fact_not_an_absence() {
        let s = store();
        assert_eq!(put(&s, "range", json!({ "repo": "r", "base": "b", "tip": "t", "commits": [] })).0, 0);
        assert_eq!(s.git_fact_range("r", "b", "t").unwrap(), Some(BTreeSet::new()));
    }

    #[test_id(tu8arkc)]
    #[test]
    fn a_repo_name_with_a_slash_cannot_reach_another_repos_key() {
        let s = store();
        put(&s, "range", json!({ "repo": "a/b", "base": "x", "tip": "y", "commits": ["c"] }));
        assert_eq!(s.git_fact_range("a", "b/x", "y").unwrap(), None);
    }
}
