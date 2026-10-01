//! spira-claim — bead attempt accounting, the CHECK 4 poison decision, and epic-first claim
//! selection. See DESIGN.md for the contract; this file is argument handling only.

mod audit;
mod counters;
mod deadlock;
mod decide;
mod events;
mod rank;
mod ready;
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
       spira-claim ready-args [--raw] [--scope-label L] [--noloop-label L]   (READY_ARGS/ready_raw_args, one token a line)
       spira-claim shared-exclude                                            (ready_shared_exclude)
       spira-claim ready-count <labels> [<exclude-labels>]                   (ready_count; prints '0' on a failed query too)
       spira-claim claim-retry <bd query argv...>                            (claim_retry; retried SPIRA_CLAIM_RETRIES x)
       spira-claim fayth-exclude <fayth> [own-exclusions]                    (fayth_exclude; needs $SPIRA_HOME, $SPIRA_FAYTHS)
       spira-claim fayth-ready <fayth>                                      (fayth_ready; ditto, plus $SPIRA_READY_CACHE)
       spira-claim bulk-ready-by-fayth                                      (bulk_ready_by_fayth; plus $SPIRA_READY_SNAPSHOT)
       spira-claim unpoison --bead ID [--bead ID...] --cause TEXT [--watch] [--watch-timeout-s N] [--dry-run]
                            [--credit SLUG] [--actor NAME] [--poison-at N]   (the one writer: DESIGN.md §8)
       spira-claim audit --candidates F [--events F] [--lifecycle F]        (every partition's own bd list, exclusions dropped)
       spira-claim deadlocked [--apply] --merge-status F [--actor NAME]     (§9; the git check is groomer's)
       spira-claim write-event <bead> <event-type> [cause]                 (lib.sh _bump_write_event_try's one INSERT; wave 4.18)
       spira-claim count-events <bead> <event-type>                        (lib.sh _counter_events_query: raw COUNT(*), no exemption)
       spira-claim lapse-record <bead> <quiet-s> <last-action> <tip>       (lib.sh write_lapse_record)
       spira-claim bead-metadata <bead> <key>                              (lib.sh bead_metadata)
       spira-claim thrash-streak-bump <bead> [tip] [note]                  (lib.sh thrash_streak_bump)
       spira-claim counter-label <bead> <prefix> <n>                      (lib.sh counter_label)
       spira-claim ask-clear <bead>                                        (lib.sh poison_asked_clear, retired; unpoison's own step 2)
  thresholds: --poison-at N (3) --requeue-at N (5) --reclaim-at N (5)
  common:     --db PATH  --timeout-s N (60)
  exit: 0 answered, 1 usage, 2 cannot tell (stdout empty); unpoison also 3 = a bead failed;
        stack also 3 = not claimable (its own JSON still names the reason);
        deadlocked also 3 = a candidate was refused or a lift did not verify;
        write-event also 1 = could not write (a no-op on an empty bead/event-type is 0);
        fayth-ready also exits 2 (no fayth in the chamber) or 1 (query failed) — stdout is
        '0' in both cases, matching fayth_ready's own historic contract (sp-3ntca)";

/// Parsed flags and positionals. Flags listed in `BOOL_FLAGS` take no value.
struct Args {
    pos: Vec<String>,
    flags: BTreeMap<String, String>,
    /// Every (flag, value) in order, for the repeatable ones (`unpoison --bead`).
    all: Vec<(String, String)>,
}

const BOOL_FLAGS: &[&str] = &["--json", "--top-tier", "--count", "--watch", "--dry-run", "--apply", "--raw"];

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
    // `claim-retry` takes ARBITRARY bd query argv (claim_retry "${READY_ARGS[@]}" ... in
    // bash) — flags meant for bd (`--label`, `--claim`, ...) cannot be told apart from
    // spira-claim's own by the generic `Args` parser, so this verb bypasses it entirely and
    // takes `raw[1..]` as the literal bd argv, never parsed here.
    if verb == "claim-retry" {
        return cmd_claim_retry(&raw[1..], &store::load_config());
    }
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
        "ready-args" => cmd_ready_args(&a, &mut env),
        "shared-exclude" => cmd_shared_exclude(&a, &env),
        "ready-count" => cmd_ready_count(&a, &mut env),
        "fayth-exclude" => cmd_fayth_exclude(&a, &mut env),
        "fayth-ready" => cmd_fayth_ready(&a, &mut env),
        "bulk-ready-by-fayth" => cmd_bulk_ready_by_fayth(&a, &mut env),
        "unpoison" => cmd_unpoison(&a, &env),
        "audit" => cmd_audit(&a, &mut env),
        "deadlocked" => cmd_deadlocked(&a, &mut env),
        "write-event" => cmd_write_event(&a, &env),
        "count-events" => cmd_count_events(&a, &env),
        "lapse-record" => cmd_lapse_record(&a, &env),
        "bead-metadata" => cmd_bead_metadata(&a, &env),
        "thrash-streak-bump" => cmd_thrash_streak_bump(&a, &env),
        "counter-label" => cmd_counter_label(&a, &env),
        "ask-clear" => cmd_ask_clear(&a, &env),
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

// =========================================================================================
// ready / claim (wave 4.25, sp-obhv6, row F): READY_ARGS, ready_count, claim_retry,
// fayth_exclude, fayth_ready, bulk_ready_by_fayth. See ready.rs for the pure predicate
// logic and store.rs for the bd calls; this is env/flag wiring only, same split as the
// rest of this file.
// =========================================================================================

/// `$SPIRA_HOME`, or the refusal every fayth-reading verb prints and fails on — the
/// chamber lives at `<home>/chamber`, and every lib.sh caller already has `SPIRA_HOME` set
/// by the time it calls one of these shims (conf.sh resolves it before lib.sh is sourced);
/// an unset value here means the shim did not thread it through the exec boundary, not
/// "use a guess" (same contract as `spira-config fayth`'s own `fayth_home`).
fn fayth_home() -> Result<std::path::PathBuf, String> {
    match std::env::var("SPIRA_HOME") {
        Ok(h) if !h.is_empty() => Ok(std::path::PathBuf::from(h)),
        _ => Err("SPIRA_HOME is not set".to_string()),
    }
}

/// `flag` (a CLI override, mainly for tests) if given, else `toml` (spira-config's
/// `resolve()`, read in-process — the correct source for a key conf.sh never exports:
/// `SPIRA_QUEUE_WAIT_LABEL`/`SPIRA_OPEN_CHILDREN_LABEL`/`SPIRA_CLAIM_RETRIES`/
/// `SPIRA_CLAIM_RETRY_DELAY_S` are all in this boat), else `env_key` (several of these ARE
/// exported by conf.sh — `SPIRA_SCOPE_LABEL`, `SPIRA_NO_LOOP_LABEL`, `SPIRA_SUBMITTED_LABEL`
/// — so reading them straight from the environment is correct there too, and harmless as a
/// fallback for the others), else `default` (the conf.d-documented default).
fn resolved_label(flag: Option<&str>, toml: Option<&str>, env_key: &str, default: &str) -> String {
    if let Some(v) = flag.filter(|v| !v.is_empty()) {
        return v.to_string();
    }
    if let Some(v) = toml.filter(|v| !v.is_empty()) {
        return v.to_string();
    }
    if let Ok(v) = std::env::var(env_key) {
        if !v.is_empty() {
            return v;
        }
    }
    default.to_string()
}

// EVERY default below is "", NOT the conf.d-documented value (e.g. "no-loop",
// "spira-queue-waiting") — matching each bash original's own bare `${VAR:-}` fallback
// (READY_ARGS, ready_raw_args, ready_shared_exclude never hardcode a default themselves;
// only conf.sh's *derivation* does, and conf.sh running is a precondition this port cannot
// observe). The lib.sh shims thread the caller's CURRENT value of the unexported ones
// (`_spira_claim`'s own env prefix) explicitly across the exec boundary, so this process
// sees exactly what the calling shell held — empty if conf.sh never ran (every suite that
// sources lib.sh alone), derived if it did (production) — never a value of this port's own
// invention. A direct caller that skips the shim (a test, or a future Rust caller) still
// gets spira.toml's value when one is configured, and "" otherwise — the same "no
// restriction" default the bash functions themselves fall back to.
fn no_loop_label(a: &Args, env: &Env) -> String {
    resolved_label(a.get("--noloop-label"), env.config.no_loop_label.as_deref(), "SPIRA_NO_LOOP_LABEL", "")
}

fn scope_label(a: &Args, env: &Env) -> String {
    resolved_label(a.get("--scope-label"), env.config.scope_label.as_deref(), "SPIRA_SCOPE_LABEL", "")
}

fn queue_wait_label(env: &Env) -> String {
    resolved_label(None, env.config.queue_wait_label.as_deref(), "SPIRA_QUEUE_WAIT_LABEL", "")
}

fn open_children_label(env: &Env) -> String {
    resolved_label(None, env.config.open_children_label.as_deref(), "SPIRA_OPEN_CHILDREN_LABEL", "")
}

/// `ready_shared_exclude`'s own reading of `SPIRA_SUBMITTED_LABEL` (bare `${VAR:-}`) —
/// distinct from [`submitted_label`], which backs `epics`/`select` and matches
/// `epic_parent_lookup`'s own `${SPIRA_SUBMITTED_LABEL:-spira-submitted}` fallback. The same
/// key, two different bash functions, two different embedded defaults — both kept exactly.
fn submitted_label_f(env: &Env) -> String {
    resolved_label(None, env.config.submitted_label.as_deref(), "SPIRA_SUBMITTED_LABEL", "")
}

/// `READY_ARGS`, resolved from this call's flags/config/environment.
fn ready_args_for(a: &Args, env: &Env) -> Vec<String> {
    ready::ready_args(&scope_label(a, env), &no_loop_label(a, env))
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

fn cmd_ready_args(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--raw", "--scope-label", "--noloop-label"]) {
        return Outcome::usage(e);
    }
    let args =
        if a.has("--raw") { ready::ready_raw_args(&no_loop_label(a, env)) } else { ready_args_for(a, env) };
    Outcome::ok(args.into_iter().fold(String::new(), |mut s, t| {
        s.push_str(&t);
        s.push('\n');
        s
    }))
}

/// `ready-count <labels> [<exclude-labels>]`: `ready_count` (lib.sh:459). A failed query
/// still prints '0' to stdout (the historic contract — every existing caller reads only
/// stdout), rc 1, the failure on stderr.
fn cmd_ready_count(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--scope-label", "--noloop-label"]) {
        return Outcome::usage(e);
    }
    let (labels, exclude) = match a.pos.as_slice() {
        [l, e] => (l.as_str(), e.as_str()),
        [l] => (l.as_str(), ""),
        _ => return Outcome::usage("ready-count needs <labels> [<exclude-labels>]"),
    };
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    let args = ready_args_for(a, env);
    match st.ready_count(&args, labels, exclude) {
        Ok(n) => Outcome::ok(n.to_string()),
        Err(e) => Outcome {
            code: 1,
            out: "0".into(),
            err: format!("spira-claim: ready_count: query failed: {}", first_line(&e)),
        },
    }
}

/// bd query argv, retried (lib.sh:609 `claim_retry`). Dispatched from `dispatch()` before
/// `Args::parse` ever sees the argv — see the comment there.
/// `cfg`'s toml value, else `$<env_key>` (bash's own `${VAR:-default}` reads this
/// unconditionally — a test, or an operator's own shell, that exports it must still win),
/// else `default`.
fn resolved_u32(cfg: Option<u32>, env_key: &str, default: u32) -> u32 {
    if let Some(v) = cfg {
        return v;
    }
    if let Ok(v) = std::env::var(env_key) {
        if let Ok(n) = v.trim().parse() {
            return n;
        }
    }
    default
}

fn cmd_claim_retry(bd_args: &[String], cfg: &Config) -> Outcome {
    if bd_args.is_empty() {
        return Outcome::usage("claim-retry needs bd query arguments");
    }
    let tries = resolved_u32(cfg.claim_retries, "SPIRA_CLAIM_RETRIES", 3).max(1);
    let delay = std::time::Duration::from_secs(resolved_u32(cfg.claim_retry_delay_s, "SPIRA_CLAIM_RETRY_DELAY_S", 1) as u64);
    let st = Store::new(None, 60, cfg);
    let mut args: Vec<&str> = bd_args.iter().map(String::as_str).collect();
    args.push("--json");
    let mut last = String::new();
    for attempt in 1..=tries {
        match st.bd(&args) {
            Ok(out) => return Outcome::ok(bead::bdq::json_only(&out).to_string()),
            Err(e) => {
                last = e;
                if attempt < tries {
                    std::thread::sleep(delay);
                }
            }
        }
    }
    Outcome {
        code: 1,
        out: String::new(),
        err: format!("spira-claim: claim_retry: query failed after {tries} attempt(s): {}", first_line(&last)),
    }
}

/// `spira_fayths`'s roster, as a `Vec<String>` — `fayth-exclude`/`fayth-ready`/
/// `bulk-ready-by-fayth` all need it, and `SPIRA_FAYTHS` (host policy, unexported by
/// design) must be threaded through the environment, same as `SPIRA_HOME`.
fn roster(home: &std::path::Path) -> Vec<String> {
    spira_config::chamber::spira_fayths(home, std::env::var("SPIRA_FAYTHS").ok().as_deref())
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn fayth_exclude_str(env: &Env, home: &std::path::Path, me: &str, own: &str) -> String {
    let shared = ready::shared_exclude3(&queue_wait_label(env), &submitted_label_f(env), &open_children_label(env));
    ready::fayth_exclude(me, own, &roster(home), &shared)
}

/// `fayth-exclude <fayth> [own-exclusions]`: `fayth_exclude` (lib.sh:666).
fn cmd_fayth_exclude(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let me = match a.pos.first() {
        Some(m) if !m.is_empty() => m.clone(),
        _ => return Outcome::usage("fayth-exclude needs <fayth> [own-exclusions]"),
    };
    let own = a.pos.get(1).map(String::as_str).unwrap_or("");
    let home = match fayth_home() {
        Ok(h) => h,
        Err(e) => return Outcome::cannot_tell(format!("fayth-exclude: {e}")),
    };
    Outcome::ok(fayth_exclude_str(env, &home, &me, own))
}

/// `shared-exclude`: `ready_shared_exclude` (lib.sh:652) — the three labels every "is this
/// claimable" predicate excludes regardless of caller. `fayth_exclude`/`fayth-exclude`
/// folds this in already; this verb exists only because `test-dispatch-open-children.sh`
/// calls `ready_shared_exclude` directly, not through `fayth_exclude`.
fn cmd_shared_exclude(a: &Args, env: &Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    Outcome::ok(ready::shared_exclude3(&queue_wait_label(env), &submitted_label_f(env), &open_children_label(env)))
}

/// `SPIRA_READY_CACHE`'s own lookup (`awk -v f="$f" '$1==f{print $2} END{...}'`): the
/// first matching line's second field, or 0 when the fayth has no line at all — never an
/// error, because sentinel's `export_ready_cache` never writes an empty-but-existing file
/// for a nonempty roster.
fn ready_cache_lookup(text: &str, me: &str) -> u64 {
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() == Some(me) {
            return it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        }
    }
    0
}

/// `fayth-ready <fayth>`: `fayth_ready` (lib.sh:693). Exit code names which of two things
/// failed (sp-3ntca): 2 = no such fayth file; 1 = the file exists but the query failed; 0 =
/// a real count, zero included. Stdout is '0' in every case but a real count — callers
/// read only stdout, never the exit code, for the number itself.
///
/// DROPPED DELIBERATELY: nothing. `SPIRA_READY_CACHE` (sentinel's `export_ready_cache`,
/// still live — it sets this for the bash child it shells into for unported dispatch
/// logic) is kept; it is read straight from the environment because it is a per-pass
/// signal file path, not a config key, and sentinel genuinely exports it.
fn cmd_fayth_ready(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--scope-label", "--noloop-label"]) {
        return Outcome::usage(e);
    }
    let me = match a.pos.first() {
        Some(m) if !m.is_empty() => m.clone(),
        _ => return Outcome::usage("fayth-ready needs <fayth>"),
    };
    let home = match fayth_home() {
        Ok(h) => h,
        Err(e) => return Outcome { code: 2, out: "0".into(), err: format!("spira-claim: fayth_ready: {e}") },
    };
    let file = spira_config::chamber::chamber_dir(&home).join(format!("{me}.fayth"));
    if !file.is_file() {
        return Outcome {
            code: 2,
            out: "0".into(),
            err: format!("spira-claim: fayth_ready: no fayth in the chamber: {}", file.display()),
        };
    }
    if let Ok(cache) = std::env::var("SPIRA_READY_CACHE") {
        if !cache.is_empty() {
            if let Ok(text) = std::fs::read_to_string(&cache) {
                return Outcome::ok(ready_cache_lookup(&text, &me).to_string());
            }
        }
    }
    let labels = spira_config::chamber::fayth_get(&home, &me, "FAYTH_LABELS", "");
    let own = spira_config::chamber::fayth_get(&home, &me, "FAYTH_EXCLUDE_LABELS", "");
    let exclude = fayth_exclude_str(env, &home, &me, &own);
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    let args = ready_args_for(a, env);
    match st.ready_count(&args, &labels, &exclude) {
        Ok(n) => Outcome::ok(n.to_string()),
        Err(e) => Outcome {
            code: 1,
            out: "0".into(),
            err: format!("spira-claim: ready_count: query failed: {}", first_line(&e)),
        },
    }
}

/// `bulk-ready-by-fayth`: `bulk_ready_by_fayth` (lib.sh:734) plus `ready-bucket.py`'s own
/// bucketing ([`ready::bucket`]). `SPIRA_READY_SNAPSHOT`, read straight from the
/// environment for the same reason as `SPIRA_READY_CACHE` (a per-pass signal, not config),
/// replaces the live fetch when it names a readable file.
fn cmd_bulk_ready_by_fayth(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--scope-label", "--noloop-label"]) {
        return Outcome::usage(e);
    }
    let home = match fayth_home() {
        Ok(h) => h,
        Err(e) => return Outcome::cannot_tell(format!("bulk-ready-by-fayth: {e}")),
    };
    let mut parts = Vec::new();
    for f in roster(&home) {
        let inc = spira_config::chamber::fayth_get(&home, &f, "FAYTH_LABELS", "");
        if inc.is_empty() {
            continue; // bulk_ready_by_fayth's own `[ -n "$inc" ] || continue`
        }
        let exc = spira_config::chamber::fayth_get(&home, &f, "FAYTH_EXCLUDE_LABELS", "");
        parts.push(ready::FaythPart { name: f, inc: ready::split_csv(&inc), exc: ready::split_csv(&exc) });
    }
    if parts.is_empty() {
        return Outcome::ok(String::new());
    }
    let raw = match std::env::var("SPIRA_READY_SNAPSHOT").ok().filter(|p| !p.is_empty()) {
        Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
        None => {
            let st = match store(a, &env.config) {
                Ok(s) => s,
                Err(e) => return Outcome::usage(e),
            };
            let args = ready_args_for(a, env);
            // A failed live fetch is a silent, empty superset here, exactly as bash's own
            // `raw="$(bdjson ... 2>/dev/null)"` (no `$?` check) — not this verb's failure.
            st.ready_json(&args).unwrap_or_default()
        }
    };
    if raw.trim().is_empty() {
        return Outcome::ok(String::new());
    }
    // ready-bucket.py: any parse exception is `d = []`, never a hard failure.
    let rows = rank::parse_ready(&raw).unwrap_or_default();
    let counts = ready::bucket(&rows, &parts, &queue_wait_label(env), &submitted_label_f(env));
    Outcome::ok(counts.into_iter().fold(String::new(), |mut s, (name, n)| {
        s.push_str(&format!("{name} {n}\n"));
        s
    }))
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

// =========================================================================================
// wave 4.18 (sp-sn1re) — lib.sh family L's attempt-counter writes and small bd
// accessors, now one-line shims in lib.sh onto the verbs below. See counters.rs.
// =========================================================================================

/// `$SPIRA_RUN`, resolved the same way `unpoison`/`deadlocked` resolve it: the
/// environment wins, then spira-config's `spira.run`.
fn resolved_run_dir(env: &Env) -> Option<std::path::PathBuf> {
    env_nonempty("SPIRA_RUN").or_else(|| env.config.run.clone()).map(std::path::PathBuf::from)
}

fn cmd_write_event(a: &Args, env: &Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let (id, etype, cause) = match a.pos.as_slice() {
        [id, etype] => (id.clone(), etype.clone(), "unrecorded".to_string()),
        [id, etype, cause] => (id.clone(), etype.clone(), cause.clone()),
        _ => return Outcome::usage("write-event: expected <bead> <event-type> [cause]"),
    };
    // `_bump_write_event_try`'s own no-op: an empty bead or event-type writes nothing
    // and is not a failure (aeon.sh calls bump_requeue bare under `set -e`).
    if id.is_empty() || etype.is_empty() {
        return Outcome::ok(String::new());
    }
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    let actor = env_nonempty("BEADS_ACTOR").unwrap_or_else(|| "harness".into());
    match counters::write_event(&st, &actor, &id, &etype, &cause) {
        Ok(()) => Outcome::ok(String::new()),
        Err(e) => Outcome { code: 1, out: String::new(), err: format!("spira-claim: write-event: {e}") },
    }
}

fn cmd_count_events(a: &Args, env: &Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let (id, etype) = match a.pos.as_slice() {
        [id, etype] if !id.is_empty() && !etype.is_empty() => (id.clone(), etype.clone()),
        _ => return Outcome::usage("count-events: expected <bead> <event-type>"),
    };
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    match counters::count_events(&st, &id, &etype) {
        Ok(n) => Outcome::ok(format!("{n}\n")),
        Err(e) => Outcome::cannot_tell(e),
    }
}

fn cmd_lapse_record(a: &Args, env: &Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let (bead, quiet, last, tip) = match a.pos.as_slice() {
        [b, q, l, t] => (b.clone(), q.clone(), l.clone(), t.clone()),
        _ => return Outcome::usage("lapse-record: expected <bead> <quiet-s> <last-action> <tip>"),
    };
    // Best-effort, same as bash: no SPIRA_RUN/spira.run means nowhere to write, which is
    // silent rather than an error — a lapse record is a diagnostic, not load-bearing.
    if let Some(run_dir) = resolved_run_dir(env) {
        counters::write_lapse_record(&run_dir, &bead, &quiet, &last, &tip);
    }
    Outcome::ok(String::new())
}

fn cmd_bead_metadata(a: &Args, env: &Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let (id, key) = match a.pos.as_slice() {
        [id, key] => (id.clone(), key.clone()),
        _ => return Outcome::usage("bead-metadata: expected <bead> <key>"),
    };
    if id.is_empty() || key.is_empty() {
        return Outcome::ok(String::new());
    }
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    Outcome::ok(format!("{}\n", counters::bead_metadata(&st, &id, &key)))
}

fn cmd_thrash_streak_bump(a: &Args, env: &Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let (id, tip, note) = match a.pos.as_slice() {
        [id] => (id.clone(), "?".to_string(), String::new()),
        [id, tip] => (id.clone(), tip.clone(), String::new()),
        [id, tip, note] => (id.clone(), tip.clone(), note.clone()),
        _ => return Outcome::usage("thrash-streak-bump: expected <bead> [tip] [note]"),
    };
    if id.is_empty() {
        return Outcome::ok("0".to_string());
    }
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    Outcome::ok(format!("{}", counters::thrash_streak_bump(&st, &id, &tip, &note)))
}

fn cmd_counter_label(a: &Args, env: &Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let (id, prefix, n) = match a.pos.as_slice() {
        [id, prefix, n] => (id.clone(), prefix.clone(), n.clone()),
        _ => return Outcome::usage("counter-label: expected <bead> <prefix> <n>"),
    };
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    let labels = match counters::bead_labels(&st, &id) {
        Ok(l) => l,
        Err(e) => return Outcome::cannot_tell(e),
    };
    match counters::counter_label(&labels, &prefix, &n) {
        Some(l) => Outcome::ok(l),
        None => Outcome { code: 1, out: String::new(), err: String::new() },
    }
}

fn cmd_ask_clear(a: &Args, env: &Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let id = match a.pos.as_slice() {
        [id] if !id.is_empty() => id.clone(),
        _ => return Outcome::usage("ask-clear: expected <bead>"),
    };
    // Best-effort, same as the retired poison_asked_clear's `rm -f ... || true`: no
    // SPIRA_RUN/spira.run, or a remove that fails, is silent.
    if let Some(run_dir) = resolved_run_dir(env) {
        let asked_dir = env_nonempty("SPIRA_POISON_ASKED").map(std::path::PathBuf::from).unwrap_or_else(|| run_dir.join("poison-asked"));
        let _ = std::fs::remove_file(asked_dir.join(&id));
    }
    Outcome::ok(String::new())
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
