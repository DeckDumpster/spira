//! `cr` command path: argv in, `Outcome` out. `main` only builds the real ports.

use crate::count;
use crate::findings;
use crate::index::Index;
use crate::pick::{self, Scored, Stratum};
use crate::ports::{Ports, SeededRng};
use crate::record;
use crate::review;
use crate::thread;
use crate::units;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub const HELP: &str = "\
cr — draw one related unit of code to review, weighted, rotated through kinds, reproducible.

  cr pick  [--aim <X>] [--seed <n>] [--json]
  cr count --regex <re> [--path <glob>]... [--limit <n>] [--json]
  cr open [--count <N>] [--aim <X>] [--seed <n>] [--scheduled]
  cr seal --review <NNN> < seal.json
  cr finding --review <NNN> < finding.json
  cr findings --review <NNN> [--post]
  cr unseal --review <NNN>
  cr close --review <NNN>
  cr thread --review <NNN> --thread <id>
  cr reply --review <NNN> --thread <id> < reply.txt
  cr status [--json]

  finding records one verdict of a review (JSON: {id?, words, comment_id?, exemplar: {path, line},
  kind, rule, scope, became, generalised_by?, endorses?}) and prints its id; an id already recorded is
  replaced. kind is question|fix|refactor|condemn|replicate|style|other; fix is for this unit
  only (scope unit, no statute or catalogue), other names one only with generalised_by. findings
  regenerates the review's page and the pattern catalogue in SPIRA_CR_FINDINGS_DIR, commits those
  two files, and prints the closeout comment; --post posts it in the reader, on line 1 of the unit's first document.

  open stages N units (default 1) as reviews and makes no reader call. With --scheduled it refuses
  while another scheduled review is not closed, and names the cr close that ends it. seal reads the
  Concierge's own review as one JSON object
  {does, look_first, lens: [{path, line, body}], fresh: [{path, line, body}]}, stores both halves hashed, posts
  a collection to the Margin reader (the cover note, the unit's files whole at the source commit, and both
  halves as held comments) and prints its URL. unseal records the review as published once the reader
  shows the operator's Publish, and posts nothing; before that it exits 2.

  close closes the review's collection, appends its close row to the ledger once, as published if the
  operator published and as unpublished if not, and marks the review closed; a rerun prints `already closed`.

  status prints the open reviews, then coverage at the landing-ref commit: the share of files a
  published review has reached, by area (first path component) and by unit kind, the oldest
  never-reviewed files, and the files changed since their review with lines changed and the review
  that covered them. Both lists are cut to SPIRA_CR_STATUS_LIMIT and end `… <k> more`. It also
  scores each published or closed review, oldest first: his lens by how many of his comments it
  predicted, the fresh read by how many of its points he endorsed. --json prints
  {reviews, scores, coverage}. Coverage is reported, never a target.

  The coverage ledger is ledger.jsonl in SPIRA_CR_FINDINGS_DIR, committed by cr; seal appends a
  review's open row and close its close row. Only a published review counts a file as reviewed.

  --aim <X>   aim the draw: a path, a workspace crate, a struct/enum/trait/type name, or words
              (every word must appear in a unit's paths or module docs, case-folded)
  --seed <n>  the draw's seed; default SPIRA_SEED, else the clock's milliseconds; always printed
  --count <N> how many units open stages
  --scheduled mark the reviews as scheduled rather than on demand
  --review <NNN>  the review to seal, unseal or close
  --thread <id>   (thread, reply) a thread of the review's collection in the reader
  --json      print one JSON object on every path, errors included
  --regex <re>   (count) the pattern; matched per line against code in .rs .sh .toml .py files
  --path <glob>  (count) search only paths matching the glob; repeatable
  --limit <n>    (count) most sites listed; default SPIRA_CR_COUNT_LIMIT. The total is never cut
  -h, --help  this text

count reads the home repository at its landing ref and prints the total of matching code lines,
the files matched and the files searched. Comment lines (// in .rs, # in .sh .toml .py) and
SPIRA_CR_EXCLUDE paths are not counted. Sites are path:line: text, sorted, then `… <k> more`.

thread prints the thread's file and line range, the source around it, and every comment, read from the
reader. reply posts stdin as a reply in that thread, not held, and prints the new comment's URL; an
empty reply is refused.

Arguments are named; a bare positional is refused.

Exit codes:
  0  a unit was drawn, or a count completed (including a count of zero)
  1  the draw or count could not run: tree, ledger or config unreadable, or no unit is drawable
  2  usage: unknown or malformed argument, an invalid --regex, or --aim matched nothing
";

struct Args {
    aim: Option<String>,
    seed: Option<u64>,
}

struct CountArgs {
    regex: Option<String>,
    paths: Vec<String>,
    limit: Option<usize>,
}

enum Cmd {
    Help,
    Pick(Args),
    Count(CountArgs),
    Open(review::OpenArgs),
    Seal(u32),
    Unseal(u32),
    Close(u32),
    Finding(u32),
    Findings { number: u32, post: bool },
    Thread { number: u32, thread: String },
    Reply { number: u32, thread: String },
    Status { json: bool },
}

const VERBS: [(&str, &[&str], &[&str]); 11] = [
    ("pick", &["--aim", "--seed"], &["--json"]),
    ("count", &["--regex", "--path", "--limit"], &["--json"]),
    ("open", &["--count", "--aim", "--seed"], &["--scheduled"]),
    ("seal", &["--review"], &[]),
    ("unseal", &["--review"], &[]),
    ("close", &["--review"], &[]),
    ("finding", &["--review"], &[]),
    ("findings", &["--review"], &["--post"]),
    ("thread", &["--review", "--thread"], &[]),
    ("reply", &["--review", "--thread"], &[]),
    ("status", &[], &["--json"]),
];

fn takes_value(flag: &str) -> bool {
    VERBS.iter().any(|(_, valued, _)| valued.contains(&flag))
}

/// The one reader of argv: the command, and whether `--json` stood in a flag position.
fn parse(argv: &[String]) -> (Result<Cmd, String>, bool) {
    let mut json = false;
    let mut flags = argv.iter();
    while let Some(a) = flags.next() {
        if takes_value(a) {
            flags.next();
        } else if a == "--json" {
            json = true;
        }
    }
    let first = argv.first().map(String::as_str);
    if matches!(first, Some("-h" | "--help")) {
        return (Ok(Cmd::Help), false);
    }
    (parse_verb(argv, first), json)
}

fn parse_verb(argv: &[String], first: Option<&str>) -> Result<Cmd, String> {
    let names: Vec<&str> = VERBS.iter().map(|v| v.0).collect();
    let verb = match first {
        Some(v) if names.contains(&v) => v,
        Some(other) => return Err(format!("unknown command `{other}`; the commands are {} (see -h)", names.join(", "))),
        None => return Err(format!("no command; use one of {} (see -h)", names.join(", "))),
    };
    let (_, valued, bare) = VERBS.iter().find(|v| v.0 == verb).expect("verb is in the table");
    let mut values: std::collections::BTreeMap<&str, String> = Default::default();
    let mut paths: Vec<String> = Vec::new();
    let mut set: Vec<&str> = Vec::new();
    let mut it = argv.iter().skip(1);
    while let Some(arg) = it.next() {
        let arg = arg.as_str();
        if matches!(arg, "-h" | "--help") {
            return Ok(Cmd::Help);
        }
        if let Some(f) = valued.iter().find(|f| **f == arg) {
            let v = it.next().ok_or(format!("{f} needs a value"))?.clone();
            if *f == "--path" {
                paths.push(v);
            } else {
                values.insert(f, v);
            }
        } else if let Some(f) = bare.iter().find(|f| **f == arg) {
            set.push(f);
        } else if arg.starts_with('-') {
            return Err(format!("unknown flag `{arg}` for `cr {verb}` (see -h)"));
        } else {
            let hint = match verb {
                "open" => "--count",
                "count" => "--regex",
                _ => "--aim",
            };
            return Err(format!("positional argument `{arg}` is not accepted; name it: `{hint} {arg}`"));
        }
    }
    let seed = match values.get("--seed") {
        Some(v) => Some(v.parse().map_err(|_| format!("--seed {v:?} is not a non-negative integer"))?),
        None => None,
    };
    let review = || -> Result<u32, String> { record::parse_number(values.get("--review").ok_or("--review <NNN> is required")?) };
    let limit = match values.get("--limit") {
        Some(v) => Some(v.parse().map_err(|_| format!("--limit {v:?} is not a non-negative integer"))?),
        None => None,
    };
    let thread = || -> Result<String, String> {
        let v = values.get("--thread").ok_or("--thread <id> is required")?;
        if v.trim().is_empty() {
            return Err("--thread is empty; give a thread id".to_string());
        }
        Ok(v.clone())
    };
    Ok(match verb {
        "thread" => Cmd::Thread { number: review()?, thread: thread()? },
        "reply" => Cmd::Reply { number: review()?, thread: thread()? },
        "count" => Cmd::Count(CountArgs { regex: values.get("--regex").cloned(), paths, limit }),
        "pick" => Cmd::Pick(Args { aim: values.get("--aim").cloned(), seed }),
        "open" => {
            let count = match values.get("--count") {
                Some(v) => v.parse::<usize>().map_err(|_| format!("--count {v:?} is not a positive integer"))?,
                None => 1,
            };
            Cmd::Open(review::OpenArgs { count, aim: values.get("--aim").cloned(), seed, scheduled: set.contains(&"--scheduled") })
        }
        "seal" => Cmd::Seal(review()?),
        "unseal" => Cmd::Unseal(review()?),
        "close" => Cmd::Close(review()?),
        "finding" => Cmd::Finding(review()?),
        "findings" => Cmd::Findings { number: review()?, post: set.contains(&"--post") },
        _ => Cmd::Status { json: set.contains(&"--json") },
    })
}

pub fn wants_help(argv: &[String]) -> bool {
    matches!(parse(argv).0, Ok(Cmd::Help))
}

pub fn wants_json(argv: &[String]) -> bool {
    parse(argv).1
}

fn fail(code: i32, json: bool, msg: &str, tried: &[String], seed: Option<u64>, commit: Option<&str>) -> Outcome {
    if json {
        let v = json!({"ok": false, "code": code, "error": msg, "tried": tried, "seed": seed, "commit": commit});
        Outcome { code, stdout: format!("{v}\n"), stderr: String::new() }
    } else {
        let mut e = format!("cr: {msg}\n");
        for t in tried {
            e.push_str(&format!("  tried {t}\n"));
        }
        Outcome { code, stdout: String::new(), stderr: e }
    }
}

pub fn error_outcome(code: i32, json: bool, msg: &str) -> Outcome {
    fail(code, json, msg, &[], None, None)
}

pub fn run(argv: &[String], ports: &Ports) -> Outcome {
    let (parsed, json) = parse(argv);
    let args = match parsed {
        Ok(Cmd::Help) => return Outcome { code: 0, stdout: HELP.to_string(), stderr: String::new() },
        Ok(Cmd::Count(c)) => return run_count(c, json, ports),
        Ok(Cmd::Pick(a)) => a,
        Ok(Cmd::Open(a)) => return review::open(&a, ports),
        Ok(Cmd::Seal(n)) => return review::seal(n, ports),
        Ok(Cmd::Unseal(n)) => return review::unseal(n, ports),
        Ok(Cmd::Close(n)) => return review::close(n, ports),
        Ok(Cmd::Finding(n)) => return findings::record_finding(n, ports),
        Ok(Cmd::Findings { number, post }) => return findings::findings(number, post, ports),
        Ok(Cmd::Thread { number, thread }) => return thread::thread(number, &thread, ports),
        Ok(Cmd::Reply { number, thread }) => return thread::reply(number, &thread, ports),
        Ok(Cmd::Status { json }) => return review::status(json, ports),
        Err(e) => return fail(2, json, &e, &[], None, None),
    };
    let now = ports.clock.now_ms();
    let seed = args.seed.or_else(|| ports.env_seed.as_deref().and_then(|s| s.trim().parse().ok())).unwrap_or(now);
    let ix = match Index::build(ports.tree.as_ref()) {
        Ok(i) => i,
        Err(e) => return fail(1, json, &e, &[], Some(seed), None),
    };
    let commit = ix.commit.clone();
    let all = units::all(&ix);

    let mut aim_note = None;
    let pool: Vec<units::Unit> = match &args.aim {
        None => all,
        Some(aim) => match pick::resolve_aim(&ix, &all, aim) {
            Ok(m) => {
                aim_note = Some((aim.clone(), m.resolution.name(), m.unit_ids.len()));
                all.into_iter().filter(|u| m.unit_ids.contains(&u.id)).collect()
            }
            Err(tried) => return fail(2, json, &format!("--aim {aim:?} matched nothing at commit {commit}"), &tried, Some(seed), Some(&commit)),
        },
    };

    let (scored, ledger) = match pick::candidates(ports, &ix, pool, now) {
        Ok(c) => c,
        Err(e) => return fail(1, json, &e, &[], Some(seed), Some(&commit)),
    };
    let mut rng = SeededRng::new(seed);
    let Some((i, stratum)) = pick::draw(&scored, &ledger, &mut rng) else {
        let why = if args.aim.is_some() { "every unit the aim matched is excluded or reviewed at its current blob or in flight" } else { "no unit is drawable: all are excluded or reviewed at their current blobs or in flight" };
        return fail(1, json, why, &[], Some(seed), Some(&commit));
    };
    let chosen = &scored[i];
    let body = describe(chosen, &ix, &stratum, &aim_note, seed, &commit, ports.config.size_unit_lines);
    if json {
        Outcome { code: 0, stdout: format!("{}\n", body.0), stderr: String::new() }
    } else {
        Outcome { code: 0, stdout: body.1, stderr: String::new() }
    }
}

fn count_fail(code: i32, json: bool, msg: &str, commit: Option<&str>) -> Outcome {
    if json {
        let v = json!({"total": null, "files": null, "searched": null, "skipped_comments": null, "commit": commit, "sites": [], "more": null, "error": msg});
        Outcome { code, stdout: format!("{v}\n"), stderr: String::new() }
    } else {
        Outcome { code, stdout: String::new(), stderr: format!("cr: {msg}\n") }
    }
}

fn run_count(a: CountArgs, json: bool, ports: &Ports) -> Outcome {
    let Some(pattern) = a.regex else {
        return count_fail(2, json, "count needs --regex <re>", None);
    };
    let re = match regex::Regex::new(&pattern) {
        Ok(r) => r,
        Err(e) => return count_fail(2, json, &format!("--regex {pattern:?} is not a valid regular expression: {e}"), None),
    };
    let limit = a.limit.unwrap_or(ports.config.count_limit);
    let c = match count::count(ports.tree.as_ref(), &re, &a.paths, &ports.config.exclude, limit) {
        Ok(c) => c,
        Err(e) => return count_fail(1, json, &e, None),
    };
    if json {
        let sites: Vec<Value> = c.sites.iter().map(|s| json!({"path": s.path, "line": s.line, "text": s.text})).collect();
        let v = json!({"total": c.total, "files": c.files, "searched": c.searched, "skipped_comments": c.skipped_comments, "commit": c.commit, "sites": sites, "more": c.more});
        return Outcome { code: 0, stdout: format!("{v}\n"), stderr: String::new() };
    }
    let mut t = format!("total {} lines in {} files ({} files searched, {} comment lines skipped) at {}\n", c.total, c.files, c.searched, c.skipped_comments, c.commit);
    for s in &c.sites {
        t.push_str(&format!("{}:{}: {}\n", s.path, s.line, s.text));
    }
    if c.more > 0 {
        t.push_str(&format!("… {} more\n", c.more));
    }
    Outcome { code: 0, stdout: t, stderr: String::new() }
}

fn describe(c: &Scored, ix: &Index, st: &Stratum, aim: &Option<(String, &'static str, usize)>, seed: u64, commit: &str, size_unit_lines: u32) -> (Value, String) {
    let w = &c.weight;
    let review = match w.days_since_reviewed {
        None => "never reviewed".to_string(),
        Some(d) => format!("last reviewed {d:.1} days ago"),
    };
    let stratum_reason = match st.last_sent_ms {
        None => format!("{} has never been sent", st.kind.as_str()),
        Some(t) => format!("{} was last sent at {t} ms, the oldest of the kinds", st.kind.as_str()),
    };
    let files: Vec<Value> = c.unit.files.iter().map(|f| json!({"path": f.path, "lines": ix.lines(&f.path)})).collect();
    let v = json!({
        "ok": true,
        "unit": {"id": c.unit.id, "kind": c.unit.kind.as_str(), "title": c.unit.title, "files": files},
        "weight": {
            "review_factor": w.review_factor,
            "changed_lines": w.changed_lines,
            "changed_term": w.changed_term(),
            "attention_share": w.attention_share(),
            "attention_term": w.attention_term(),
            "review": review,
            "commits": w.commits,
            "churn_term": w.churn_term(),
            "dependents": w.dependents,
            "dependents_term": w.dependents_term(),
            "lines": w.lines,
            "size_term": w.size_term(size_unit_lines),
            "total": w.total,
        },
        "stratum": {"kind": st.kind.as_str(), "reason": stratum_reason, "skipped": st.skipped.iter().map(|k| k.as_str()).collect::<Vec<_>>()},
        "aim": aim.as_ref().map(|(a, r, n)| json!({"aim": a, "resolution": r, "matched_units": n})),
        "seed": seed,
        "commit": commit,
    });
    let mut t = format!("unit: {}\nkind: {}\ntitle: {}\nfiles:\n", c.unit.id, c.unit.kind.as_str(), c.unit.title);
    for f in &c.unit.files {
        t.push_str(&format!("  {} ({} lines)\n", f.path, ix.lines(&f.path)));
    }
    t.push_str(&format!(
        "weight: {:.3} = review {:.2} ({review}) x attention {:.3} ({} of his {} comments in {}) x changed {:.3} (1 + min({} changed lines / {} lines, 1)) x churn {:.0} (1 + {} commits) x dependents {:.3} (1 + ln(1 + {})) x size {:.3} (min({} lines / {size_unit_lines}, 3))\n",
        w.total, w.review_factor, w.attention_term(), w.area_comments, w.his_comments, w.area, w.changed_term(), w.changed_lines, w.lines.max(1), w.churn_term(), w.commits, w.dependents_term(), w.dependents, w.size_term(size_unit_lines), w.lines
    ));
    t.push_str(&format!("stratum: {stratum_reason}\n"));
    match aim {
        Some((a, r, n)) => t.push_str(&format!("aim: {a:?} resolved as {r}, {n} units matched\n")),
        None => t.push_str("aim: none\n"),
    }
    t.push_str(&format!("seed: {seed}\ncommit: {commit}\n"));
    (v, t)
}
