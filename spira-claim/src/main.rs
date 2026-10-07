//! spira-claim — bead attempt accounting, the CHECK 4 poison decision, and epic-first claim
//! selection. See DESIGN.md for the contract; this file is argument handling only.

mod audit;
mod counters;
mod deadlock;
mod decide;
mod events;
mod rank;
mod ready;
mod reopen;
mod store;
mod unpoison;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;

use decide::{AskedStamp, Inputs, Thresholds};
use events::EventRow;
use rank::{EpicLookup, LifecycleRow, ReadyRow, Verdict};
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
    /// A real error — never `CANNOT_TELL` (callers read that 2 as "no fayth / nothing
    /// there", not "something is broken"): code 1, the message, no `USAGE_TEXT`. Per the
    /// Concierge's sp-hh599 decision (2026-10-05): `store::load_config`'s own failure is
    /// this, for every verb, not a per-verb "cannot tell".
    fn error(msg: impl Into<String>) -> Self {
        Outcome { code: 1, out: String::new(), err: format!("spira-claim: {}", msg.into()) }
    }
}

const USAGE_TEXT: &str = "usage: spira-claim attempts <bead> [--events F] [--json]
       spira-claim requeues <bead> [--events F] [--json]
       spira-claim counts [--events F]                      (ids on stdin)
       spira-claim decide [thresholds] [--] <n> <requeues> <reclaims> <labels> <stamp> [poisoned]
       spira-claim poison-decide <bead> [--labels CSV] [--asked RQ:RC:PO[:PL]] [--poisoned 0|1] [thresholds] [--events F] [--json]
       spira-claim epics [--ready F]                       (ready JSON on stdin by default)
       spira-claim select --fayth NAME [--ready F] [--epics F] [--resumable F] [--top-tier|--count|--json]
                          [--blockers bd|machine] [--lifecycle F] [--blocker-records F] [--stack-max-depth N]
       spira-claim stack <bead> [--lifecycle F] [--blocker-records F] [--stack-max-depth N]
       spira-claim ready-args [--raw] [--scope-label L] [--noloop-label L]   (READY_ARGS/ready_raw_args, one token a line)
       spira-claim shared-exclude                                            (ready_shared_exclude)
       spira-claim ready-count <labels> [<exclude-labels>] [--json]          (ready_count; prints '0' on a failed query too; --json: the rows)
       spira-claim claim-retry <bd query argv...>                            (claim_retry; retried SPIRA_CLAIM_RETRIES x)
       spira-claim fayth-exclude <fayth> [own-exclusions]                    (fayth_exclude; resolves $SPIRA_HOME in-process, $SPIRA_FAYTHS)
       spira-claim fayth-ready <fayth> [--json]                             (fayth_ready; ditto, plus $SPIRA_READY_CACHE; --json: the rows themselves, never cached)
       spira-claim bulk-ready-by-fayth                                      (bulk_ready_by_fayth; the machine's claimable set)
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
       spira-claim reopen <bead> [<cause>] [<note>] [<suites>]             (lib.sh bead_reopen; wave 4.19; cause default unrecorded)
       spira-claim release <bead>                                         (lib.sh release_claim; wave 4.19)
       spira-claim deliberate-causes                                      (lib.sh _census_deliberate_reopen_causes; wave 4.19)
       spira-claim deliberate-exempt <cause>                              (lib.sh _census_reopen_admission_exempt; wave 4.19)
  thresholds: --poison-at N (3) --requeue-at N (5) --reclaim-at N (5)
  common:     --db PATH  --timeout-s N (60)
  exit: 0 answered, 1 usage, 2 cannot tell (stdout empty); unpoison also 3 = a bead failed;
        stack also 3 = not claimable (its own JSON still names the reason);
        deadlocked also 3 = a candidate was refused or a lift did not verify;
        write-event also 1 = could not write (a no-op on an empty bead/event-type is 0);
        reopen also 1 = the lifecycle reopen/note each separately failed (refused);
        deliberate-exempt is a bare exit code (0 exempt, 1 not), no stdout;
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
        return match store::load_config() {
            Ok(cfg) => cmd_claim_retry(&raw[1..], &cfg),
            Err(e) => Outcome::error(format!("config: {e}")),
        };
    }
    let a = match Args::parse(&raw[1..]) {
        Ok(a) => a,
        Err(e) => return Outcome::usage(e),
    };
    // Config only for verbs that read a store or a config key: `decide` is called once per
    // bead per CHECK 4 pass and must not re-read (or refuse over) spira.toml each time.
    let config = if verb == "decide" {
        Config::default()
    } else {
        match store::load_config() {
            Ok(c) => c,
            Err(e) => return Outcome::error(format!("config: {e}")),
        }
    };
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
        "reopen" => cmd_reopen(&a, &env),
        "release" => cmd_release(&a, &env),
        "deliberate-causes" => cmd_deliberate_causes(&a),
        "deliberate-exempt" => cmd_deliberate_exempt(&a),
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

/// The lifecycle rows the epic lookup reads a child's progress from (sp-mve9i: a work bead's
/// state is its row, never bd's status). `known` is a snapshot the caller already holds
/// (machine-mode `select`). Otherwise `spira-lc list` is read, and a machine that cannot
/// answer is cannot-tell — never a guess from bd, which would be the very read this replaces
/// (DESIGN.md §6a).
fn epic_lc(st: &Store, known: Option<&HashMap<String, LifecycleRow>>) -> Result<HashMap<String, LifecycleRow>, Outcome> {
    if let Some(m) = known {
        return Ok(m.clone());
    }
    st.lifecycle_snapshot()
        .and_then(|t| rank::parse_lifecycle(&t))
        .map_err(|e| Outcome::cannot_tell(format!("epic lookup: lifecycle snapshot: {e} (the machine must answer)")))
}

fn fetch_lookup(a: &Args, env: &Env, rows: &[ReadyRow], known: Option<&HashMap<String, LifecycleRow>>) -> Result<EpicLookup, Outcome> {
    let parents = rank::parents(rows);
    if parents.is_empty() {
        return Ok(EpicLookup::default());
    }
    let s = store(a, &env.config).map_err(Outcome::usage)?;
    let epics = s.list_by_ids(&parents).map_err(Outcome::cannot_tell)?;
    let prio = epics.iter().map(|e| (e.id.clone(), e.priority.unwrap_or(99))).collect();
    let lc = epic_lc(&s, known)?;
    let mut started = Vec::new();
    for p in &parents {
        let kids = s.children(p).map_err(Outcome::cannot_tell)?;
        if rank::epic_started(&kids, &lc) {
            started.push(p.clone());
        }
    }
    Ok(EpicLookup { prio, started })
}

fn cmd_epics(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--ready"]) {
        return Outcome::usage(e);
    }
    let rows = match read_ready(a, env) {
        Ok(r) => r,
        Err(o) => return o,
    };
    match fetch_lookup(a, env, &rows, None) {
        Ok(l) => Outcome::ok(format!("{}\n", serde_json::to_string(&l).unwrap())),
        Err(o) => o,
    }
}

fn cmd_select(a: &Args, env: &mut Env) -> Outcome {
    let known = [
        "--fayth", "--ready", "--epics", "--resumable", "--top-tier", "--count", "--json", "--blockers",
        "--lifecycle", "--blocker-records", "--stack-max-depth",
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
    let stack_max = match a.num("--stack-max-depth", env.config.stack_max_depth) {
        Ok(n) => n.min(rank::STACK_CEILING),
        Err(e) => return Outcome::usage(e),
    };
    let mut rows = match read_ready(a, env) {
        Ok(r) => r,
        Err(o) => return o,
    };
    let mut machine_lc: Option<HashMap<String, LifecycleRow>> = None;
    if machine {
        let st = match store(a, &env.config) {
            Ok(s) => s,
            Err(e) => return Outcome::usage(e),
        };
        let lc_text = match a.get("--lifecycle") {
            Some(p) => read_source(p, env.stdin),
            None => st.lifecycle_snapshot(),
        };
        let lc = match lc_text.and_then(|t| rank::parse_lifecycle(&t)) {
            Ok(m) => m,
            Err(e) => {
                return Outcome::cannot_tell(format!(
                    "{fayth}: lifecycle snapshot: {e} (the machine must answer)"
                ))
            }
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
        rows.retain(|r| matches!(rank::claimable(r, &lc, &bd, rank::stack_cap(r, &env.config.incident_label, stack_max)), Verdict::Claimable { .. }));
        machine_lc = Some(lc);
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
        None => match fetch_lookup(a, env, &rows, machine_lc.as_ref()) {
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
    let stack_max = match a.num("--stack-max-depth", env.config.stack_max_depth) {
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
    match rank::stack_plan(&cand, &lc, &bd, rank::stack_cap(&cand, &env.config.incident_label, stack_max)) {
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

/// `$SPIRA_HOME`, resolved IN-PROCESS — law-a-binary-resolves-the-config-it-reads
/// (sp-hh599): `spira-sentinel.service`/`spira-summon.service` carry `SPIRA_RELEASE`/
/// `PATH` and nothing else (wave 4.25, sp-obhv6, stopped `conf.sh` from exporting
/// `SPIRA_HOME` at all). Every `fayth-ready`/`fayth-exclude`/`bulk-ready-by-fayth` call the
/// sentinel makes is ITS OWN in-process child (never through `lib.sh`), so under that
/// unit's real environment this used to refuse outright — and `summon.rs`'s `fayth_ready`
/// read that refusal's rc 2 as "no fayth", skipping every fayth every pass, forever,
/// silently (this bead's own repro). The environment is an OVERRIDE a caller may still set
/// on purpose (a fixture pinning a different tree; a lib.sh shim that already resolved it
/// and threads it through for free) — never the ONLY source. Absent, this falls back to
/// this process's own release root (`spira_config::release_env::own_release_root_for_process`,
/// sp-kgzql's identical ascending search `sentinel::locate_home`/`spira_world::locate_home`
/// already run for the home THEIR OWN binary needs — reused rather than invented a fourth
/// way) plus `/spira`, the directory `conf.sh` itself lives beside and where the chamber
/// lives too (exactly `SPIRA_HOME=<release>/spira`, matching this bead's own manual repro).
/// `Err` only when NEITHER source answers — no ancestor of this executable holds a release
/// at all (a bare `cargo test` binary, or a truly detached shell) — a genuine "cannot
/// evaluate", and every caller below is careful never to let that read as "no fayth" (see
/// `cmd_fayth_ready`'s own exit-code doc, just below).
fn fayth_home() -> Result<std::path::PathBuf, String> {
    if let Ok(h) = std::env::var("SPIRA_HOME") {
        if !h.is_empty() {
            return Ok(std::path::PathBuf::from(h));
        }
    }
    match spira_config::release_env::own_release_root_for_process() {
        Some(release) => Ok(release.join("spira")),
        None => Err("cannot resolve SPIRA_HOME: no release found above this executable's own location (set SPIRA_HOME to override)".to_string()),
    }
}

/// `flag` (a CLI override, kept only for tests that want to pin a label without a toml
/// fixture) if given and non-empty, else `env.config`'s own already-resolved value — the
/// ONE source of config (per Ryan 2026-10-05): `Config` (see store.rs) is the only reader
/// of `SPIRA_QUEUE_WAIT_LABEL`/`SPIRA_OPEN_CHILDREN_LABEL`/`SPIRA_SCOPE_LABEL`/
/// `SPIRA_NO_LOOP_LABEL`/`SPIRA_SUBMITTED_LABEL`/`SPIRA_CLAIM_RETRIES`/
/// `SPIRA_CLAIM_RETRY_DELAY_S`, and it already refused at construction if any could not
/// resolve — no caller-side default survives here.
fn resolved_label(flag: Option<&str>, configured: &str) -> String {
    match flag {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => configured.to_string(),
    }
}

fn no_loop_label(a: &Args, env: &Env) -> String {
    resolved_label(a.get("--noloop-label"), &env.config.no_loop_label)
}

fn scope_label(a: &Args, env: &Env) -> String {
    resolved_label(a.get("--scope-label"), &env.config.scope_label)
}

fn queue_wait_label(env: &Env) -> String {
    env.config.queue_wait_label.clone()
}

fn open_children_label(env: &Env) -> String {
    env.config.open_children_label.clone()
}

/// `SPIRA_SUBMITTED_LABEL`, via `env.config` — the one source now (per Ryan 2026-10-05).
/// Two bash functions (`ready_shared_exclude`'s bare `${VAR:-}` and `bead_reopen`'s
/// `${VAR:-spira-submitted}`) used to carry two different embedded defaults for this same
/// key; both are gone — `reopen`'s own caller now reads this same function, not a second
/// one (see the migration report). (`epics`/`select` read no label: a child's progress is
/// its lifecycle row, sp-mve9i.)
fn submitted_label_f(env: &Env) -> String {
    env.config.submitted_label.clone()
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
///
/// `--json` prints the counted rows instead (empty stdout and rc 1 on a failed query): the
/// one ready set for a reader that shows beads rather than counts them, so nothing outside
/// spira-claim asks bd for "ready" itself.
fn cmd_ready_count(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--scope-label", "--noloop-label", "--json"]) {
        return Outcome::usage(e);
    }
    let (labels, exclude) = match a.pos.as_slice() {
        [l, e] => (l.as_str(), e.as_str()),
        [l] => (l.as_str(), ""),
        _ => return Outcome::usage("ready-count needs <labels> [<exclude-labels>]"),
    };
    if a.has("--json") {
        return ready_rows_json(a, env, labels, exclude);
    }
    match machine_claimable(a, env) {
        Ok(rows) => Outcome::ok(ready::count_matching(&rows, &ready::split_csv(labels), &ready::split_csv(exclude)).to_string()),
        Err(e) => Outcome { code: 1, out: "0".into(), err: format!("spira-claim: ready_count: {}", first_line(&e)) },
    }
}

/// `ready-count --json`: [`machine_claimable`]'s rows matching the labels. A machine that
/// cannot answer is a refusal, never an empty queue.
fn ready_rows_json(a: &Args, env: &Env, labels: &str, exclude: &str) -> Outcome {
    let rows = match machine_claimable(a, env) {
        Ok(rows) => rows,
        Err(e) => return Outcome { code: 1, out: String::new(), err: format!("spira-claim: ready_count: {}", first_line(&e)) },
    };
    let mine = ready::matching(&rows, &ready::split_csv(labels), &ready::split_csv(exclude));
    Outcome::ok(format!("{}\n", serde_json::to_string(&mine).unwrap()))
}

/// bd query argv, retried (lib.sh:609 `claim_retry`). Dispatched from `dispatch()` before
/// `Args::parse` ever sees the argv — see the comment there.
fn cmd_claim_retry(bd_args: &[String], cfg: &Config) -> Outcome {
    if bd_args.is_empty() {
        return Outcome::usage("claim-retry needs bd query arguments");
    }
    let tries = cfg.claim_retries.max(1);
    let delay = std::time::Duration::from_secs(cfg.claim_retry_delay_s as u64);
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
/// `bulk-ready-by-fayth` all need it. `SPIRA_FAYTHS` (`spira.fayths`, a list) is the one
/// source of config now (per Ryan 2026-10-05): `env.config.fayths`, read once at
/// construction — never the environment directly.
fn roster(home: &std::path::Path, fayths: &str) -> Vec<String> {
    let ov = (!fayths.is_empty()).then_some(fayths);
    spira_config::chamber::spira_fayths(home, ov).split_whitespace().map(str::to_string).collect()
}

fn fayth_exclude_str(env: &Env, home: &std::path::Path, me: &str, own: &str) -> String {
    let shared = ready::shared_exclude3(&queue_wait_label(env), &submitted_label_f(env), &open_children_label(env));
    ready::fayth_exclude(me, own, &roster(home, &env.config.fayths), &shared)
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

/// The beads a claim could actually take. The candidates are the
/// machine's READY/REWORK rows; bd is read only for their content (labels, type, priority,
/// blockers), never for status or assignee — the same rule `select --blockers machine`
/// applies. `Err` when the machine cannot answer: a count must refuse, not read 0.
fn machine_claimable(a: &Args, env: &Env) -> Result<Vec<rank::ReadyRow>, String> {
    let st = store(a, &env.config)?;
    let lc = rank::parse_lifecycle(&st.lifecycle_snapshot()?)?;
    let ids = ready::lifecycle_ready_ids(&lc);
    let want: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    let mut rows = if ids.is_empty() { Vec::new() } else { st.list_by_ids(&ids)? };
    let (scope, no_loop) = (scope_label(a, env), no_loop_label(a, env));
    rows.retain(|r| want.contains(r.id.as_str()) && ready::in_scope(r, &scope, &no_loop));
    let wanted = rank::all_blockers(&rows);
    let recs = if wanted.is_empty() { Vec::new() } else { st.list_by_ids(&wanted)? };
    let bd = rank::index_rows(recs);
    let stack_max = env.config.stack_max_depth.min(rank::STACK_CEILING);
    Ok(rows.into_iter().filter(|r| matches!(rank::claimable(r, &lc, &bd, rank::stack_cap(r, &env.config.incident_label, stack_max)), Verdict::Claimable { .. })).collect())
}

/// `fayth-ready <fayth>`: `fayth_ready` (lib.sh:693). Exit code names which of FOUR things
/// happened (sp-3ntca; widened by sp-hh599, then sp-xsnid): 2 = no such fayth file — the
/// ONE outcome a resolved chamber was actually consulted and came back empty, the only rc
/// `sentinel::summon::fayth_ready` may map to `ReadyAnswer::NoFayth`; 1 = "could not
/// evaluate" — the chamber was never reached at all (SPIRA_HOME itself would not resolve)
/// OR the file exists but the query failed; 3 = the fayth's OWN predicate would WIDEN
/// rather than narrow (a bare config reference `FAYTH_LABELS`/`FAYTH_EXCLUDE_LABELS` uses
/// resolved empty — sp-xsnid's own bug: `ops`/`spike`/`builder` all read `234`, the WHOLE
/// ready queue, because `$SPIRA_INCIDENT_LABEL`/`$SPIRA_SPIKE_LABEL`/`$SPIRA_PLAN_LABEL`
/// resolved empty under the sentinel unit's own bare environment); 0 = a real count, zero
/// included. law-a-control-that-cannot-check-must-refuse: an rc that means "could not
/// evaluate" (1) or "would widen" (3) must never collapse into "no fayth" (2) — both
/// bugs this bead's own two parts fixed were exactly that collapse. Stdout is '0' in
/// every case but a real count — callers read only stdout, never the exit code, for the
/// number itself.
///
/// DROPPED DELIBERATELY: nothing. `SPIRA_READY_CACHE` (sentinel's `export_ready_cache`,
/// still live — it sets this for the bash child it shells into for unported dispatch
/// logic) is kept; it is read straight from the environment because it is a per-pass
/// signal file path, not a config key, and sentinel genuinely exports it. ITS OWN COUNTS
/// come from `ctx.fayths` (sentinel's probe script, a REAL `bash -c '. conf.sh; ...'`
/// subshell where every label IS exported — sp-xsnid's "the sentinel's own lane count
/// already said ops has 25" is this fast path, already correct, untouched here), so a
/// cache hit skips [`spira_config::chamber::fayth_predicate`] entirely, on purpose.
///
/// `--json` prints the counted rows instead of their number: an aeon's ready set, so what it
/// claims from is what the sentinel summoned it for.
fn cmd_fayth_ready(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--scope-label", "--noloop-label", "--json"]) {
        return Outcome::usage(e);
    }
    let me = match a.pos.first() {
        Some(m) if !m.is_empty() => m.clone(),
        _ => return Outcome::usage("fayth-ready needs <fayth>"),
    };
    let home = match fayth_home() {
        // rc 1, NEVER 2: the chamber was never reached, so this is "could not evaluate",
        // not "no fayth in the chamber" — see this function's own doc above.
        Ok(h) => h,
        Err(e) => return Outcome { code: 1, out: "0".into(), err: format!("spira-claim: fayth_ready: {e}") },
    };
    let file = spira_config::chamber::chamber_dir(&home).join(format!("{me}.fayth"));
    if !file.is_file() {
        return Outcome {
            code: 2,
            out: "0".into(),
            err: format!("spira-claim: fayth_ready: no fayth in the chamber: {}", file.display()),
        };
    }
    let json = a.has("--json");
    if let Ok(cache) = std::env::var("SPIRA_READY_CACHE") {
        if !cache.is_empty() && !json {
            if let Ok(text) = std::fs::read_to_string(&cache) {
                return Outcome::ok(ready_cache_lookup(&text, &me).to_string());
            }
        }
    }
    // rc 3, NEVER 2 or an empty-string widen: see this function's own doc above.
    let predicate = match spira_config::chamber::fayth_predicate(&home, &me) {
        Ok(p) => p,
        Err(e) => return Outcome { code: 3, out: "0".into(), err: format!("spira-claim: fayth_ready: {e}") },
    };
    let exclude = fayth_exclude_str(env, &home, &me, &predicate.exclude_labels);
    let rows = match machine_claimable(a, env) {
        Ok(r) => r,
        Err(e) => return Outcome { code: 1, out: "0".into(), err: format!("spira-claim: fayth_ready: {}", first_line(&e)) },
    };
    let part = ready::FaythPart { name: me, inc: ready::split_csv(&predicate.labels), exc: ready::split_csv(&exclude) };
    let mine = ready::partition(&rows, &part, &queue_wait_label(env), &submitted_label_f(env));
    if json {
        return Outcome::ok(format!("{}\n", serde_json::to_string(&mine).unwrap()));
    }
    Outcome::ok(mine.len().to_string())
}

/// `bulk-ready-by-fayth`: `bulk_ready_by_fayth` (lib.sh:734) plus `ready-bucket.py`'s own
/// bucketing ([`ready::bucket`]) over the machine's claimable set ([`machine_claimable`]).
fn cmd_bulk_ready_by_fayth(a: &Args, env: &mut Env) -> Outcome {
    if let Err(e) = a.check_known(&["--scope-label", "--noloop-label"]) {
        return Outcome::usage(e);
    }
    let home = match fayth_home() {
        Ok(h) => h,
        Err(e) => return Outcome::cannot_tell(format!("bulk-ready-by-fayth: {e}")),
    };
    let mut parts = Vec::new();
    let mut warnings = Vec::new();
    for f in roster(&home, &env.config.fayths) {
        // sp-xsnid: a fayth whose predicate would WIDEN (a bare config reference resolved
        // empty) is skipped exactly like a declared-empty one — `bulk_ready_by_fayth`'s
        // own `[ -n "$inc" ] || continue` already never widened on an empty label, only
        // undercounted it, which is the safe direction — but it is LOUD here, never
        // silent, because undercounting a real incident/spike partition is a real defect
        // worth seeing even though it is not the dangerous one.
        let predicate = match spira_config::chamber::fayth_predicate(&home, &f) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(e.to_string());
                continue;
            }
        };
        if predicate.labels.is_empty() {
            continue; // bulk_ready_by_fayth's own `[ -n "$inc" ] || continue`
        }
        // sp-85p8t: the SAME exclusion `fayth-ready` and an aeon's `select` apply (own,
        // shared incl. open-children, and every other fayth's `fayth:<name>`) — counting with
        // the bare own list let the sentinel summon builders for epics with open children
        // that no builder could claim (56 summons in 10 minutes, each "nothing ready").
        let exc = fayth_exclude_str(env, &home, &f, &predicate.exclude_labels);
        parts.push(ready::FaythPart { name: f, inc: ready::split_csv(&predicate.labels), exc: ready::split_csv(&exc) });
    }
    if parts.is_empty() {
        return Outcome { code: 0, out: String::new(), err: warnings.join("\n") };
    }
    let rows = match machine_claimable(a, env) {
        Ok(r) => r,
        Err(e) => return Outcome::cannot_tell(format!("bulk-ready-by-fayth: {}", first_line(&e))),
    };
    let counts = ready::bucket(&rows, &parts, &queue_wait_label(env), &submitted_label_f(env));
    Outcome {
        code: 0,
        out: counts.into_iter().fold(String::new(), |mut s, (name, n)| {
            s.push_str(&format!("{name} {n}\n"));
            s
        }),
        err: warnings.join("\n"),
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
    })
}

fn env_nonempty(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.trim().is_empty())
}

fn cmd_unpoison(a: &Args, env: &Env) -> Outcome {
    let o = match unpoison_opts(a) {
        Ok(o) => o,
        Err(e) => return Outcome::usage(e),
    };
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    if st.db.is_none() {
        return Outcome::cannot_tell("no bead store: --db, $SPIRA_DB and spira.db are all unset — nothing was written");
    }
    // SPIRA_RUN and SPIRA_ASK_LABEL are both registered keys — `env.config` already
    // refused at construction if either could not resolve (per Ryan 2026-10-05, one
    // source of config), so neither is re-read from the environment or re-derived here.
    let run_dir = std::path::PathBuf::from(&env.config.run);
    let asked_dir = env_nonempty("SPIRA_POISON_ASKED").map(Into::into).unwrap_or_else(|| run_dir.join("poison-asked"));
    let ask_label = env.config.ask_label.clone();
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
    let lc = {
        let lc_text = match a.get("--lifecycle") {
            Some(p) => read_source(p, env.stdin),
            None => match store(a, &env.config) {
                Ok(s) => s.lifecycle_snapshot(),
                Err(e) => return Outcome::usage(e),
            },
        };
        match lc_text.and_then(|t| rank::parse_lifecycle(&t)) {
            Ok(m) => m,
            Err(e) => return Outcome::cannot_tell(format!("lifecycle snapshot: {e} (the machine must answer)")),
        }
    };
    Outcome::ok(audit::run(&candidates, &events_by, &lc))
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
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    if st.db.is_none() {
        return Outcome::cannot_tell("no bead store: --db, $SPIRA_DB and spira.db are all unset — nothing was written");
    }
    // Only `mark_poison_lifted` (sp-wiyr2) touches run_dir; every other World method this
    // verb calls is store-only, but that one write is load-bearing (DESIGN.md §9). SPIRA_RUN
    // is a registered key — `env.config` already refused at construction if it could not
    // resolve, exactly as `unpoison` relies on the same guarantee.
    let mut live = unpoison::Live {
        store: st,
        run_dir: std::path::PathBuf::from(&env.config.run),
        asked_dir: std::path::PathBuf::new(),
        ask_label: String::new(),
        beads_actor: actor.clone(),
    };
    let o = deadlock::Opts { apply: a.has("--apply"), actor };
    let (code, out) = deadlock::run(&o, &candidates, &mut live);
    Outcome { code, out, err: String::new() }
}

// =========================================================================================
// wave 4.18 (sp-sn1re) — lib.sh family L's attempt-counter writes and small bd
// accessors, now one-line shims in lib.sh onto the verbs below. See counters.rs.
// =========================================================================================

/// `$SPIRA_RUN` — `env.config.run`, the one source of config; always present (`Config`
/// refused at construction otherwise).
fn resolved_run_dir(env: &Env) -> std::path::PathBuf {
    std::path::PathBuf::from(&env.config.run)
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
    counters::write_lapse_record(&resolved_run_dir(env), &bead, &quiet, &last, &tip);
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
    // Best-effort, same as the retired poison_asked_clear's `rm -f ... || true`: a remove
    // that fails is silent.
    let run_dir = resolved_run_dir(env);
    let asked_dir = env_nonempty("SPIRA_POISON_ASKED").map(std::path::PathBuf::from).unwrap_or_else(|| run_dir.join("poison-asked"));
    let _ = std::fs::remove_file(asked_dir.join(&id));
    Outcome::ok(String::new())
}

// =========================================================================================
// wave 4.19 (sp-3wfcb) — lib.sh family I's `bead_reopen`/`release_claim`, plus the census
// admission-exemption list both it and row M's SQL producers read. See reopen.rs.
// =========================================================================================

/// One line per reopen in `$SPIRA_RUN/reopen.log`, written here so no caller can skip it. The
/// caller is the parent's argv[0] read from /proc, never matched from a command-line pattern.
fn trace_reopen(run_dir: &std::path::Path, id: &str, cause: &str, actor: &str) {
    use std::io::Write;
    if run_dir.as_os_str().is_empty() {
        return;
    }
    let caller = std::fs::read(format!("/proc/{}/cmdline", std::os::unix::process::parent_id()))
        .ok()
        .and_then(|b| b.split(|&c| c == 0).next().map(|a| String::from_utf8_lossy(a).into_owned()))
        .and_then(|a| a.rsplit('/').next().map(str::to_string))
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let ts = spira_config::bounded::bounded("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(run_dir.join("reopen.log")) {
        let _ = writeln!(f, "{ts} reopen {id} cause={cause} actor={actor} caller={caller}");
    }
}

fn cmd_reopen(a: &Args, env: &Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let (id, cause, note, suites) = match a.pos.as_slice() {
        [id] => (id.clone(), String::new(), String::new(), String::new()),
        [id, cause] => (id.clone(), cause.clone(), String::new(), String::new()),
        [id, cause, note] => (id.clone(), cause.clone(), note.clone(), String::new()),
        [id, cause, note, suites] => (id.clone(), cause.clone(), note.clone(), suites.clone()),
        _ => return Outcome::usage("reopen: expected <bead> [<cause>] [<note>] [<suites>]"),
    };
    if id.is_empty() {
        return Outcome::usage("reopen: <bead> must not be empty");
    }
    // `bead_reopen`'s own default: `local ... cause="${2:-unrecorded}"` — an omitted OR
    // empty-string cause both land here (bash's `${2:-unrecorded}` only skips the default
    // for an UNSET positional, but every caller either omits it or passes a real cause; no
    // caller in the tree passes a deliberately empty one, so treating "" as "omitted" here
    // matches every observed call and is the safer reading either way).
    let cause = if cause.is_empty() { "unrecorded".to_string() } else { cause };
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    let actor = env_nonempty("BEADS_ACTOR").unwrap_or_else(|| "harness".into());
    let run_dir = resolved_run_dir(env);
    trace_reopen(&run_dir, &id, &cause, &actor);
    let mut live = unpoison::Live {
        store: st,
        run_dir,
        asked_dir: std::path::PathBuf::new(),
        ask_label: String::new(),
        beads_actor: actor,
    };
    let o = reopen::Opts { id: id.clone(), cause, note, suites };
    let rc = reopen::run(&o, &mut live);
    if rc == 0 {
        Outcome::ok(String::new())
    } else {
        Outcome { code: rc, out: String::new(), err: format!("bead_reopen: {id} — {}\n", reopen::FAILURE_REASON) }
    }
}

fn cmd_release(a: &Args, env: &Env) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let id = match a.pos.as_slice() {
        [id] if !id.is_empty() => id.clone(),
        _ => return Outcome::usage("release: expected <bead>"),
    };
    let st = match store(a, &env.config) {
        Ok(s) => s,
        Err(e) => return Outcome::usage(e),
    };
    match st.release_claim(&id, &env_nonempty("BEADS_ACTOR").unwrap_or_else(|| "harness".into())) {
        Ok(()) => Outcome::ok(String::new()),
        Err(e) => Outcome { code: 1, out: String::new(), err: format!("spira-claim: release: {e}") },
    }
}

/// lib.sh `_census_deliberate_reopen_causes` alone — row M's census SQL producers (still
/// bash; a later bead) read this through the lib.sh shim, same list `reopen` decides
/// admission from.
fn cmd_deliberate_causes(a: &Args) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    if !a.pos.is_empty() {
        return Outcome::usage("deliberate-causes: takes no arguments");
    }
    Outcome::ok(reopen::deliberate_causes_text())
}

/// lib.sh `_census_reopen_admission_exempt <cause>` alone — exit 0 exempt, 1 not (no
/// stdout: the bash function's own contract is a bare return code).
fn cmd_deliberate_exempt(a: &Args) -> Outcome {
    if let Err(e) = a.check_known(&[]) {
        return Outcome::usage(e);
    }
    let cause = match a.pos.as_slice() {
        [c] => c.clone(),
        _ => return Outcome::usage("deliberate-exempt: expected <cause>"),
    };
    if reopen::admission_exempt(&cause) {
        Outcome::ok(String::new())
    } else {
        Outcome { code: 1, out: String::new(), err: String::new() }
    }
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
