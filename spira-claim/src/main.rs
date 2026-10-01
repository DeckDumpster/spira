//! spira-claim — bead attempt accounting, the CHECK 4 poison decision, and epic-first claim
//! selection. See DESIGN.md for the contract; this file is argument handling only.

mod audit;
mod deadlock;
mod decide;
mod events;
mod rank;
mod store;
mod unpoison;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;

use decide::{AskedStamp, Inputs, Thresholds};
use events::EventRow;
use rank::{EpicLookup, ReadyRow, Verdict};
use store::{Config, Store};

pub const USAGE: i32 = 1;
pub const CANNOT_TELL: i32 = 2;

#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Outcome {
    fn ok(out: String) -> Self {
        Outcome { code: 0, out, err: String::new() }
    }
    fn usage(msg: impl Into<String>) -> Self {
        Outcome { code: USAGE, out: String::new(), err: format!("spira-claim: {}\n{}", msg.into(), USAGE_TEXT) }
    }
    fn cannot_tell(msg: impl Into<String>) -> Self {
        Outcome { code: CANNOT_TELL, out: String::new(), err: format!("spira-claim: cannot tell: {}", msg.into()) }
    }
}

const USAGE_TEXT: &str = "usage: spira-claim attempts <bead> [--events F] [--json]
       spira-claim requeues <bead> [--events F] [--json]
       spira-claim counts [--events F]                      (ids on stdin)
       spira-claim decide [thresholds] [--] <n> <requeues> <reclaims> <labels> <stamp> [poisoned]
       spira-claim poison-decide <bead> [--labels CSV] [--asked RQ:RC:PO[:PL]] [--poisoned 0|1] [thresholds] [--events F] [--json]
       spira-claim epics [--ready F] [--submitted-label L]  (ready JSON on stdin by default)
       spira-claim select --fayth NAME [--ready F] [--epics F] [--resumable F] [--top-tier|--count|--json]
                          [--blockers bd|machine] [--lifecycle F] [--blocker-records F] [--stack-max-depth N]
       spira-claim stack <bead> [--lifecycle F] [--blocker-records F] [--stack-max-depth N]
       spira-claim unpoison --bead ID [--bead ID...] --cause TEXT [--watch] [--watch-timeout-s N] [--dry-run]
                            [--credit SLUG] [--actor NAME] [--poison-at N]   (the one writer: DESIGN.md §8)
       spira-claim audit --candidates F [--events F] [--lifecycle F]        (every partition's own bd list, exclusions dropped)
       spira-claim deadlocked [--apply] --merge-status F [--actor NAME]     (§9; the git check is groomer's)
  thresholds: --poison-at N (3) --requeue-at N (5) --reclaim-at N (5)
  common:     --db PATH  --timeout-s N (60)
  exit: 0 answered, 1 usage, 2 cannot tell (stdout empty); unpoison also 3 = a bead failed;
        stack also 3 = not claimable (its own JSON still names the reason);
        deadlocked also 3 = a candidate was refused or a lift did not verify";

/// Parsed flags and positionals. Flags listed in `BOOL_FLAGS` take no value.
struct Args {
    pos: Vec<String>,
    flags: BTreeMap<String, String>,
    /// Every (flag, value) in order, for the repeatable ones (`unpoison --bead`).
    all: Vec<(String, String)>,
}

const BOOL_FLAGS: &[&str] = &["--json", "--top-tier", "--count", "--watch", "--dry-run", "--apply"];

impl Args {
    fn parse(raw: &[String]) -> Result<Args, String> {
        let mut pos = Vec::new();
        let mut flags = BTreeMap::new();
        let mut all = Vec::new();
        let mut i = 0;
        let mut only_pos = false;
        while i < raw.len() {
            let a = &raw[i];
            if only_pos || !a.starts_with("--") {
                pos.push(a.clone());
            } else if a == "--" {
                only_pos = true;
            } else if BOOL_FLAGS.contains(&a.as_str()) {
                flags.insert(a.clone(), "1".into());
            } else {
                let v = raw.get(i + 1).ok_or_else(|| format!("{a} needs a value"))?;
                flags.insert(a.clone(), v.clone());
                all.push((a.clone(), v.clone()));
                i += 1;
            }
            i += 1;
        }
        Ok(Args { pos, flags, all })
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.get(k).map(String::as_str)
    }
    fn has(&self, k: &str) -> bool {
        self.flags.contains_key(k)
    }
    fn num(&self, k: &str, default: u32) -> Result<u32, String> {
        match self.get(k) {
            None => Ok(default),
            Some(v) => v.trim().parse().map_err(|_| format!("{k}: not a non-negative integer: {v:?}")),
        }
    }
    fn every(&self, k: &str) -> Vec<String> {
        self.all.iter().filter(|(f, _)| f == k).map(|(_, v)| v.clone()).collect()
    }
    fn check_known(&self, known: &[&str]) -> Result<(), String> {
        for k in self.flags.keys() {
            if !known.contains(&k.as_str()) && !["--db", "--timeout-s"].contains(&k.as_str()) {
                return Err(format!("unknown flag {k}"));
            }
        }
        Ok(())
    }
}

/// Where rows come from: the store, or a file/stdin the caller supplied.
struct Env<'a> {
    stdin: &'a mut dyn Read,
    config: Config,
}

fn read_source(path: &str, stdin: &mut dyn Read) -> Result<String, String> {
    if path == "-" {
        let mut s = String::new();
        stdin.read_to_string(&mut s).map_err(|e| format!("stdin: {e}"))?;
        Ok(s)
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
    }
}

fn store(a: &Args, cfg: &Config) -> Result<Store, String> {
    let t = a.num("--timeout-s", 60)?;
    Ok(Store::new(a.get("--db").map(str::to_string), t as u64, cfg))
}

fn load_events(a: &Args, env: &mut Env, ids: &[String]) -> Result<Vec<EventRow>, Outcome> {
    match a.get("--events") {
        Some(p) => {
            let text = read_source(p, env.stdin).map_err(Outcome::cannot_tell)?;
            events::parse_rows(&text).map_err(Outcome::cannot_tell)
        }
        None => {
            let s = store(a, &env.config).map_err(Outcome::usage)?;
            s.events(ids).map_err(Outcome::cannot_tell)
        }
    }
}

fn thresholds(a: &Args) -> Result<Thresholds, String> {
    let d = Thresholds::default();
    Ok(Thresholds {
        poison_at: a.num("--poison-at", d.poison_at)?,
        requeue_at: a.num("--requeue-at", d.requeue_at)?,
        reclaim_at: a.num("--reclaim-at", d.reclaim_at)?,
    })
}

const THRESH: &[&str] = &["--poison-at", "--requeue-at", "--reclaim-at"];

pub fn dispatch(raw: &[String], stdin: &mut dyn Read) -> Outcome {
    let Some(verb) = raw.first() else { return Outcome::usage("missing verb") };
    let a = match Args::parse(&raw[1..]) {
        Ok(a) => a,
        Err(e) => return Outcome::usage(e),
    };
    // Config only for verbs that read a store or a config key: `decide` is called once per
    // bead per CHECK 4 pass and must not re-read (or warn about) spira.toml each time.
    let config = if verb == "decide" { Config::default() } else { store::load_config() };
    let mut env = Env { stdin, config };
    match verb.as_str() {
        "attempts" | "requeues" => cmd_count(verb, &a, &mut env),
        "counts" => cmd_counts(&a, &mut env),
        "decide" => cmd_decide(&a),
        "poison-decide" => cmd_poison_decide(&a, &mut env),
        "epics" => cmd_epics(&a, &mut env),
        "select" => cmd_select(&a, &mut env),
        "stack" => cmd_stack(&a, &mut env),
        "unpoison" => cmd_unpoison(&a, &env),
        "audit" => cmd_audit(&a, &mut env),
        "deadlocked" => cmd_deadlocked(&a, &mut env),
        "-h" | "--help" | "help" => Outcome::ok(format!("{USAGE_TEXT}\n")),
        other => Outcome::usage(format!("unknown verb {other}")),
    }
}

fn one_bead(a: &Args) -> Result<String, Outcome> {
    match a.pos.as_slice() {
        [b] if !b.is_empty() => Ok(b.clone()),
        _ => Err(Outcome::usage("expected exactly one <bead>")),
    }
}

fn cmd_count(verb: &str, a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--events", "--json"]) {
        return Outcome::usage(e);
    }
    let bead = match one_bead(a) {
        Ok(b) => b,
        Err(o) => return o,
    };
    let rows = match load_events(a, env, std::slice::from_ref(&bead)) {
        Ok(r) => r,
        Err(o) => return o,
    };
    let l = events::fold(&bead, &rows);
    if a.has("--json") {
        return Outcome::ok(format!("{}\n", serde_json::to_string_pretty(&l).unwrap()));
    }
    Outcome::ok(format!("{}\n", if verb == "attempts" { l.attempts } else { l.requeues }))
}

fn cmd_counts(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--events"]) {
        return Outcome::usage(e);
    }
    if a.get("--events") == Some("-") {
        return Outcome::usage("counts reads ids on stdin; give --events a file");
    }
    let mut input = String::new();
    if let Err(e) = env.stdin.read_to_string(&mut input) {
        return Outcome::cannot_tell(format!("stdin: {e}"));
    }
    let mut seen = BTreeSet::new();
    let ids: Vec<String> = input
        .lines()
        .filter_map(|l| l.split('\t').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.to_string()))
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return Outcome::ok(String::new());
    }
    let rows = match load_events(a, env, &ids) {
        Ok(r) => r,
        Err(o) => return o,
    };
    let mut by: HashMap<&str, Vec<EventRow>> = HashMap::new();
    for r in &rows {
        by.entry(r.issue_id.as_str()).or_default().push(r.clone());
    }
    let mut out = String::new();
    for id in &ids {
        let l = events::fold(id, by.get(id.as_str()).map(Vec::as_slice).unwrap_or(&[]));
        out.push_str(&format!("{id}\t{}\t{}\t{}\n", l.attempts, l.requeues, l.reclaims));
    }
    Outcome::ok(out)
}

fn cmd_decide(a: &Args) -> Outcome {
    if let Err(e) = a.check_known(THRESH) {
        return Outcome::usage(e);
    }
    let t = match thresholds(a) {
        Ok(t) => t,
        Err(e) => return Outcome::usage(e),
    };
    if a.pos.len() < 3 || a.pos.len() > 6 {
        return Outcome::usage("decide takes <n> <requeues> <reclaims> [<labels> [<stamp> [poisoned]]]");
    }
    let num = |i: usize| -> Result<u32, String> {
        let s = a.pos.get(i).map(String::as_str).unwrap_or("");
        let s = if s.is_empty() { "0" } else { s };
        s.trim().parse().map_err(|_| format!("argument {} is not a non-negative integer: {s:?}", i + 1))
    };
    let (n, rq, rc) = match (num(0), num(1), num(2)) {
        (Ok(n), Ok(rq), Ok(rc)) => (n, rq, rc),
        (Err(e), ..) | (_, Err(e), _) | (.., Err(e)) => return Outcome::usage(e),
    };
    let labels = a.pos.get(3).map(String::as_str).unwrap_or("");
    let stamp = AskedStamp::parse(a.pos.get(4).map(String::as_str).unwrap_or("0:0:0:0"));
    let poisoned = a.pos.get(5).map(String::as_str) == Some("1");
    let tokens = decide::decide(&Inputs { attempts: n, requeues: rq, reclaims: rc, labels, stamp, poisoned }, t);
    Outcome::ok(format!("{}\n", decide::render(&tokens)))
}

fn cmd_poison_decide(a: &Args, env: &mut Env) -> Outcome {
    let mut known = vec!["--events", "--json", "--labels", "--asked", "--poisoned"];
    known.extend_from_slice(THRESH);
    if let Err(e) = a.check_known(&known) {
        return Outcome::usage(e);
    }
    let bead = match one_bead(a) {
        Ok(b) => b,
        Err(o) => return o,
    };
    let t = match thresholds(a) {
        Ok(t) => t,
        Err(e) => return Outcome::usage(e),
    };
    let poisoned = match a.get("--poisoned").unwrap_or("0") {
        "0" => false,
        "1" => true,
        v => return Outcome::usage(format!("--poisoned takes 0 or 1, not {v:?}")),
    };
    let rows = match load_events(a, env, std::slice::from_ref(&bead)) {
        Ok(r) => r,
        Err(o) => return o,
    };
    let l = events::fold(&bead, &rows);
    let labels = a.get("--labels").unwrap_or("");
    let stamp = AskedStamp::parse(a.get("--asked").unwrap_or("0:0:0:0"));
    let tokens = decide::decide(
        &Inputs { attempts: l.attempts, requeues: l.requeues, reclaims: l.reclaims, labels, stamp, poisoned },
        t,
    );
    if a.has("--json") {
        let v = serde_json::json!({
            "bead": bead, "attempts": l.attempts, "requeues": l.requeues, "reclaims": l.reclaims,
            "poisoned": poisoned, "stamp": stamp, "thresholds": t, "decision": tokens,
        });
        return Outcome::ok(format!("{}\n", serde_json::to_string_pretty(&v).unwrap()));
    }
    Outcome::ok(format!("{}\n", decide::render(&tokens)))
}

fn read_ready(a: &Args, env: &mut Env) -> Result<Vec<ReadyRow>, Outcome> {
    let text = read_source(a.get("--ready").unwrap_or("-"), env.stdin).map_err(Outcome::cannot_tell)?;
    rank::parse_ready(&text).map_err(Outcome::cannot_tell)
}

fn submitted_label(a: &Args, cfg: &Config) -> String {
    a.get("--submitted-label")
        .map(str::to_string)
        .or_else(|| cfg.submitted_label.clone())
        .unwrap_or_else(|| "spira-submitted".into())
}

fn fetch_lookup(a: &Args, env: &Env, rows: &[ReadyRow]) -> Result<EpicLookup, Outcome> {
    let parents = rank::parents(rows);
    if parents.is_empty() {
        return Ok(EpicLookup::default());
    }
    let s = store(a, &env.config).map_err(Outcome::usage)?;
    let epics = s.list_by_ids(&parents).map_err(Outcome::cannot_tell)?;
    let prio = epics.iter().map(|e| (e.id.clone(), e.priority.unwrap_or(99))).collect();
    let label = submitted_label(a, &env.config);
    let mut started = Vec::new();
    for p in &parents {
        let kids = s.children(p).map_err(Outcome::cannot_tell)?;
        if rank::epic_started(&kids, &label) {
            started.push(p.clone());
        }
    }
    Ok(EpicLookup { prio, started })
}

fn cmd_epics(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--ready", "--submitted-label"]) {
        return Outcome::usage(e);
    }
    let rows = match read_ready(a, env) {
        Ok(r) => r,
        Err(o) => return o,
    };
    match fetch_lookup(a, env, &rows) {
        Ok(l) => Outcome::ok(format!("{}\n", serde_json::to_string(&l).unwrap())),
        Err(o) => o,
    }
}

/// `lifecycle_enforce` for `select` (DESIGN.md §6a): `spira_config::lifecycle_enforce` —
/// `SPIRA_LIFECYCLE_ENFORCE` wins, else `spira.lifecycle_enforce`, else off; the same rule
/// as the aeon crate and as `unpoison` (§8.7). Tests pin it per thread instead of reading
/// the host's environment or spira.toml, and default to off.
fn lifecycle_on() -> bool {
    #[cfg(test)]
    {
        tests::ENFORCE.with(|c| c.get())
    }
    #[cfg(not(test))]
    {
        spira_config::lifecycle_enforce(None)
    }
}

fn cmd_select(a: &Args, env: &mut Env) -> Outcome {
    let known = [
        "--fayth", "--ready", "--epics", "--resumable", "--top-tier", "--count", "--json", "--blockers",
        "--lifecycle", "--blocker-records", "--stack-max-depth", "--submitted-label",
    ];
    if let Err(e) = a.check_known(&known) {
        return Outcome::usage(e);
    }
    let Some(fayth) = a.get("--fayth").filter(|f| !f.is_empty()).map(str::to_string) else {
        return Outcome::usage("select needs --fayth <name>");
    };
    if !a.pos.is_empty() {
        return Outcome::usage("select takes no positional arguments (the ready set comes on stdin or --ready)");
    }
    if [a.has("--top-tier"), a.has("--count"), a.has("--json")].iter().filter(|x| **x).count() > 1 {
        return Outcome::usage("--top-tier, --count and --json are exclusive");
    }
    let machine = match a.get("--blockers").unwrap_or("bd") {
        "bd" => false,
        "machine" => true,
        v => return Outcome::usage(format!("--blockers takes bd or machine, not {v:?}")),
    };
    let stack_max = match a.num("--stack-max-depth", env.config.stack_max_depth.unwrap_or(rank::STACK_CEILING)) {
        Ok(n) => n.min(rank::STACK_CEILING),
        Err(e) => return Outcome::usage(e),
    };
    let mut rows = match read_ready(a, env) {
        Ok(r) => r,
        Err(o) => return o,
    };
    // THE lifecycle switch (DESIGN.md §6a). Off: spira-lc is never run and the poison is
    // the spira-poison label — in either blockers mode a labelled bead is not claimable.
    let enforce = lifecycle_on();
    if !enforce {
        rows.retain(|r| !rank::poisoned_by_label(r));
    }

    if machine {
        let st = match store(a, &env.config) {
            Ok(s) => s,
            Err(e) => return Outcome::usage(e),
        };
        let lc = if enforce {
            let lc_text = match a.get("--lifecycle") {
                Some(p) => read_source(p, env.stdin),
                None => st.lifecycle_snapshot(),
            };
            match lc_text.and_then(|t| rank::parse_lifecycle(&t)) {
                Ok(m) => Some(m),
                Err(e) => {
                    return Outcome::cannot_tell(format!(
                        "{fayth}: lifecycle snapshot: {e} (lifecycle_enforce is on, so the machine must answer)"
                    ))
                }
            }
        } else {
            if a.has("--lifecycle") {
                eprintln!("spira-claim: {fayth}: --lifecycle ignored — lifecycle_enforce is off, claimability comes from bd records");
            }
            None
        };
        let wanted = rank::all_blockers(&rows);
        let recs = match a.get("--blocker-records") {
            Some(p) => read_source(p, env.stdin).and_then(|t| rank::parse_ready(&t)),
            None if wanted.is_empty() => Ok(Vec::new()),
            None => st.list_by_ids(&wanted),
        };
        let bd = match recs {
            Ok(r) => rank::index_rows(r),
            Err(e) => return Outcome::cannot_tell(format!("{fayth}: blocker records: {e}")),
        };
        rows.retain(|r| {
            let v = match &lc {
                Some(lc) => rank::claimable(r, lc, &bd, stack_max),
                None => rank::claimable_legacy(r, &bd),
            };
            matches!(v, Verdict::Claimable { .. })
        });
    }

    if a.has("--count") {
        let n = rows.iter().filter(|r| !matches!(r.issue_type.as_deref(), Some("epic") | Some("event"))).count();
        return Outcome::ok(format!("{n}\n"));
    }
    let lookup = match a.get("--epics") {
        Some(p) => match read_source(p, env.stdin).and_then(|t| rank::parse_lookup(&t)) {
            Ok(l) => l,
            Err(e) => return Outcome::cannot_tell(format!("{fayth}: epic lookup: {e}")),
        },
        None => match fetch_lookup(a, env, &rows) {
            Ok(l) => l,
            Err(o) => return o,
        },
    };
    if a.has("--top-tier") {
        return Outcome::ok(rank::top_tier(&rows, &lookup).iter().fold(String::new(), |mut s, l| {
            s.push_str(l);
            s.push('\n');
            s
        }));
    }
    let resumable = match a.get("--resumable") {
        Some(p) => match read_source(p, env.stdin) {
            Ok(t) => rank::parse_id_set(&t),
            Err(e) => return Outcome::cannot_tell(e),
        },
        None => BTreeSet::new(),
    };
    let ranked = rank::rank(&rows, &lookup, &resumable);
    if a.has("--json") {
        return Outcome::ok(format!("{}\n", serde_json::to_string_pretty(&ranked).unwrap()));
    }
    Outcome::ok(ranked.iter().fold(String::new(), |mut s, r| {
        s.push_str(&r.tsv());
        s.push('\n');
        s
    }))
}

/// `stack <bead>`: the same claimability rule `select --blockers machine` applies, but for
/// one already-known candidate and reporting the stack proposal — `{prereq: certified_tip}`
/// — a claim of it would carry, rather than a yes/no filtered out of a ready set. The one
/// caller is the aeon crate, right after it wins the bd claim: the Claim event and the
/// worktree's own merged base both need the same map, so this is where it is computed once.
fn cmd_stack(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--lifecycle", "--blocker-records", "--stack-max-depth"]) {
        return Outcome::usage(e);
    }
    let bead = match one_bead(a) {
        Ok(b) => b,
        Err(o) => return o,
    };
    let stack_max = match a.num("--stack-max-depth", env.config.stack_max_depth.unwrap_or(rank::STACK_CEILING)) {
        Ok(n) => n.min(rank::STACK_CEILING),
        Err(e) => return Outcome::usage(e),
    };
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    let cand = match st.list_by_ids(std::slice::from_ref(&bead)) {
        Ok(rows) => match rows.into_iter().find(|r| r.id == bead) {
            Some(r) => r,
            None => return Outcome::cannot_tell(format!("{bead}: bd has no record")),
        },
        Err(e) => return Outcome::cannot_tell(format!("{bead}: {e}")),
    };
    let lc = {
        let lc_text = match a.get("--lifecycle") {
            Some(p) => read_source(p, env.stdin),
            None => st.lifecycle_snapshot(),
        };
        match lc_text.and_then(|t| rank::parse_lifecycle(&t)) {
            Ok(m) => m,
            Err(e) => return Outcome::cannot_tell(format!("{bead}: lifecycle snapshot: {e}")),
        }
    };
    let wanted = rank::blockers(&cand);
    let recs = match a.get("--blocker-records") {
        Some(p) => read_source(p, env.stdin).and_then(|t| rank::parse_ready(&t)),
        None if wanted.is_empty() => Ok(Vec::new()),
        None => st.list_by_ids(&wanted),
    };
    let bd = match recs {
        Ok(r) => rank::index_rows(r),
        Err(e) => return Outcome::cannot_tell(format!("{bead}: blocker records: {e}")),
    };
    match rank::stack_plan(&cand, &lc, &bd, stack_max) {
        Ok((stack, depth)) => {
            let v = serde_json::json!({"claimable": true, "stack": stack, "stack_depth": depth, "stack_max_depth": stack_max});
            Outcome::ok(format!("{}\n", v))
        }
        Err(rank::Verdict::TooDeep { depth, max }) => {
            let v = serde_json::json!({"claimable": false, "reason": "TooDeep", "stack": {}, "stack_depth": depth, "stack_max_depth": max});
            Outcome { code: 3, out: format!("{v}\n"), err: String::new() }
        }
        Err(v) => Outcome {
            code: 3,
            out: format!("{}\n", serde_json::json!({"claimable": false, "reason": format!("{v:?}")})),
            err: String::new(),
        },
    }
}

/// `unpoison`'s arguments, validated, before anything is read (DESIGN.md §8.2).
fn unpoison_opts(a: &Args) -> Result<unpoison::Opts, String> {
    let known = ["--bead", "--cause", "--watch", "--watch-timeout-s", "--dry-run", "--credit", "--actor", "--poison-at"];
    a.check_known(&known)?;
    if let Some(p) = a.pos.first() {
        return Err(format!("unexpected argument {p:?} — named arguments only (--bead, --cause)"));
    }
    let beads = a.every("--bead");
    if beads.is_empty() {
        return Err("--bead is required".into());
    }
    if let Some(b) = beads.iter().find(|b| !unpoison::valid_id(b)) {
        return Err(format!("--bead {b:?} is not a bead id"));
    }
    let cause = a.get("--cause").unwrap_or("").trim().to_string();
    if cause.is_empty() {
        return Err("--cause is required — say why the poison was wrong".into());
    }
    let credit = a.get("--credit").map(str::to_string);
    if let Some(c) = credit.as_deref().filter(|c| !unpoison::valid_slug(c)) {
        return Err(format!("--credit {c:?} must match [a-z0-9-]{{1,64}}"));
    }
    let actor = a.get("--actor").unwrap_or("unpoison").to_string();
    if !unpoison::valid_id(&actor) {
        return Err(format!("--actor {actor:?} must match [A-Za-z0-9._-]+"));
    }
    let env_p = std::env::var("SPIRA_POISON_AT").ok().and_then(|v| v.trim().parse().ok());
    Ok(unpoison::Opts {
        beads,
        cause,
        watch: a.has("--watch"),
        watch_timeout_s: a.num("--watch-timeout-s", unpoison::WATCH_TIMEOUT_S as u32)? as u64,
        dry_run: a.has("--dry-run"),
        credit,
        actor,
        poison_at: a.num("--poison-at", env_p.unwrap_or(Thresholds::default().poison_at))?,
        enforce: false, // resolved by cmd_unpoison from env and config
    })
}

/// `lifecycle_enforce`, resolved as the aeon crate resolves it (aeon/src/conf.rs): the
/// environment's `SPIRA_LIFECYCLE_ENFORCE` wins ("1"/"true" on, anything else off), else
/// `spira.lifecycle_enforce` through spira-config, else off.
fn lifecycle_enforce(env_value: Option<&str>, cfg: &Config) -> bool {
    match env_value {
        Some(v) => v == "1" || v == "true",
        None => cfg.lifecycle_enforce.unwrap_or(false),
    }
}

fn env_nonempty(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.trim().is_empty())
}

fn cmd_unpoison(a: &Args, env: &Env) -> Outcome {
    let mut o = match unpoison_opts(a) {
        Ok(o) => o,
        Err(e) => return Outcome::usage(e),
    };
    o.enforce = lifecycle_enforce(std::env::var("SPIRA_LIFECYCLE_ENFORCE").ok().as_deref(), &env.config);
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    if st.db.is_none() {
        return Outcome::cannot_tell("no bead store: --db, $SPIRA_DB and spira.db are all unset — nothing was written");
    }
    let Some(run_dir) = env_nonempty("SPIRA_RUN").or_else(|| env.config.run.clone()) else {
        return Outcome::cannot_tell("SPIRA_RUN is unset and spira.run is not configured — nothing was written");
    };
    let run_dir = std::path::PathBuf::from(run_dir);
    let asked_dir = env_nonempty("SPIRA_POISON_ASKED").map(Into::into).unwrap_or_else(|| run_dir.join("poison-asked"));
    let ask_label = env_nonempty("SPIRA_ASK_LABEL")
        .or_else(|| env.config.ask_label.clone())
        .unwrap_or_else(|| "needs-operator".into()); // literal-ok: Rust fallback mirroring conf.sh's default when SPIRA_ASK_LABEL is unset
    let mut live = unpoison::Live {
        store: st,
        run_dir,
        asked_dir,
        ask_label,
        beads_actor: env_nonempty("BEADS_ACTOR").unwrap_or_else(|| "harness".into()),
    };
    let (code, out) = unpoison::run(&o, &mut live);
    Outcome { code, out, err: String::new() }
}

/// `audit`'s candidate set: every bead a partition's own labels would match, exclusions
/// dropped on purpose (attempts.sh's own rule — a poisoned or asked-about bead is exactly
/// the one whose count most needs reading). The caller's own `bd list --label` per
/// partition is where the exclusion-free query lives (lib.sh's chamber reading does not
/// belong in this binary, same boundary as `select`'s ready set); this only folds what it
/// is handed.
fn cmd_audit(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--candidates", "--events", "--lifecycle"]) {
        return Outcome::usage(e);
    }
    if !a.pos.is_empty() {
        return Outcome::usage("audit takes no positional arguments (candidates come on --candidates)");
    }
    if a.get("--events") == Some("-") {
        return Outcome::usage("audit reads candidates on stdin by default; give --events a file");
    }
    let cand_text = match read_source(a.get("--candidates").unwrap_or("-"), env.stdin) {
        Ok(t) => t,
        Err(e) => return Outcome::cannot_tell(e),
    };
    let candidates = match rank::parse_ready(&cand_text) {
        Ok(c) => c,
        Err(e) => return Outcome::cannot_tell(format!("--candidates: {e}")),
    };
    let mut seen = BTreeSet::new();
    let ids: Vec<String> = candidates.iter().map(|r| r.id.clone()).filter(|id| seen.insert(id.clone())).collect();
    let rows = match load_events(a, env, &ids) {
        Ok(r) => r,
        Err(o) => return o,
    };
    let mut events_by: HashMap<String, Vec<EventRow>> = HashMap::new();
    for r in rows {
        events_by.entry(r.issue_id.clone()).or_default().push(r);
    }
    let enforce = lifecycle_on();
    let lc = if enforce {
        let lc_text = match a.get("--lifecycle") {
            Some(p) => read_source(p, env.stdin),
            None => match store(a, &env.config) {
                Ok(s) => s.lifecycle_snapshot(),
                Err(e) => return Outcome::usage(e),
            },
        };
        match lc_text.and_then(|t| rank::parse_lifecycle(&t)) {
            Ok(m) => Some(m),
            Err(e) => return Outcome::cannot_tell(format!("lifecycle snapshot: {e} (lifecycle_enforce is on, so the machine must answer)")),
        }
    } else {
        None
    };
    Outcome::ok(audit::run(&audit::Opts { enforce }, &candidates, &events_by, lc.as_ref()))
}

/// `deadlocked`'s merge-status input comes from `groomer deadlocked`, which does the git
/// legwork this binary deliberately does not (DESIGN.md §7, §9). The write is unpoison's
/// `Live`, reused rather than duplicated, so a lift is provably the same call.
fn cmd_deadlocked(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--apply", "--merge-status", "--actor"]) {
        return Outcome::usage(e);
    }
    if !a.pos.is_empty() {
        return Outcome::usage("deadlocked takes no positional arguments");
    }
    let text = match read_source(a.get("--merge-status").unwrap_or("-"), env.stdin) {
        Ok(t) => t,
        Err(e) => return Outcome::cannot_tell(e),
    };
    let candidates = match deadlock::parse_candidates(&text) {
        Ok(c) => c,
        Err(e) => return Outcome::cannot_tell(e),
    };
    let actor = a.get("--actor").unwrap_or("groomer").to_string();
    let enforce = lifecycle_enforce(std::env::var("SPIRA_LIFECYCLE_ENFORCE").ok().as_deref(), &env.config);
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    if st.db.is_none() {
        return Outcome::cannot_tell("no bead store: --db, $SPIRA_DB and spira.db are all unset — nothing was written");
    }
    // Only `mark_poison_lifted` (sp-wiyr2) touches run_dir; every other World method this
    // verb calls is store-only, but that one write is load-bearing (DESIGN.md §9), so it is
    // resolved exactly as `unpoison` resolves it and refused the same way when it is not.
    let Some(run_dir) = env_nonempty("SPIRA_RUN").or_else(|| env.config.run.clone()) else {
        return Outcome::cannot_tell("SPIRA_RUN is unset and spira.run is not configured — nothing was written");
    };
    let mut live = unpoison::Live {
        store: st,
        run_dir: std::path::PathBuf::from(run_dir),
        asked_dir: std::path::PathBuf::new(),
        ask_label: String::new(),
        beads_actor: actor.clone(),
    };
    let o = deadlock::Opts { apply: a.has("--apply"), actor, enforce };
    let (code, out) = deadlock::run(&o, &candidates, &mut live);
    Outcome { code, out, err: String::new() }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let o = dispatch(&args, &mut std::io::stdin());
    print!("{}", o.out);
    if !o.err.is_empty() {
        eprintln!("{}", o.err.trim_end());
    }
    std::process::exit(o.code);
}

#[cfg(test)]
mod tests;
