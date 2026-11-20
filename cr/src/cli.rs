//! `cr` command path: argv in, `Outcome` out. `main` only builds the real ports.

use crate::count;
use crate::index::Index;
use crate::pick::{self, Scored, Shaped, Stratum};
use crate::ports::{Ports, SeededRng};
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

  --aim <X>   aim the draw: a path, a workspace crate, a struct/enum/trait/type name, or words
              (every word must appear in a unit's paths or module docs, case-folded)
  --seed <n>  the draw's seed; default SPIRA_SEED, else the clock's milliseconds; always printed
  --json      print one JSON object on every path, errors included
  --regex <re>   (count) the pattern; matched per line against code in .rs .sh .toml .py files
  --path <glob>  (count) search only paths matching the glob; repeatable
  --limit <n>    (count) most sites listed; default SPIRA_CR_COUNT_LIMIT. The total is never cut
  -h, --help  this text

count reads the home repository at its landing ref and prints the total of matching code lines,
the files matched and the files searched. Comment lines (// in .rs, # in .sh .toml .py) and
SPIRA_CR_EXCLUDE paths are not counted. Sites are path:line: text, sorted, then `… <k> more`.

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
}

/// The one reader of argv: the command, and whether `--json` stood in a flag position.
fn parse(argv: &[String]) -> (Result<Cmd, String>, bool) {
    let mut json = false;
    let mut flags = argv.iter();
    while let Some(a) = flags.next() {
        match a.as_str() {
            "--aim" | "--seed" | "--regex" | "--path" | "--limit" => {
                flags.next();
            }
            "--json" => json = true,
            _ => {}
        }
    }
    let mut it = argv.iter();
    let first = it.next().map(String::as_str);
    if matches!(first, Some("-h" | "--help")) {
        return (Ok(Cmd::Help), false);
    }
    let r = (|| {
        let count = match first {
            Some("pick") => false,
            Some("count") => true,
            Some(other) => return Err(format!("unknown command `{other}`; the commands are `cr pick` and `cr count` (see -h)")),
            None => return Err("no command; use `cr pick` or `cr count` (see -h)".to_string()),
        };
        let mut a = Args { aim: None, seed: None };
        let mut c = CountArgs { regex: None, paths: vec![], limit: None };
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => return Ok(Cmd::Help),
                "--aim" if !count => a.aim = Some(it.next().ok_or("--aim needs a value")?.clone()),
                "--seed" if !count => {
                    let v = it.next().ok_or("--seed needs a value")?;
                    a.seed = Some(v.parse().map_err(|_| format!("--seed {v:?} is not a non-negative integer"))?);
                }
                "--regex" if count => c.regex = Some(it.next().ok_or("--regex needs a value")?.clone()),
                "--path" if count => c.paths.push(it.next().ok_or("--path needs a value")?.clone()),
                "--limit" if count => {
                    let v = it.next().ok_or("--limit needs a value")?;
                    c.limit = Some(v.parse().map_err(|_| format!("--limit {v:?} is not a non-negative integer"))?);
                }
                "--json" => {}
                f if f.starts_with('-') => return Err(format!("unknown flag `{f}` (see -h)")),
                positional if count => return Err(format!("positional argument `{positional}` is not accepted; name it: `--regex {positional}`")),
                positional => return Err(format!("positional argument `{positional}` is not accepted; name it: `--aim {positional}`")),
            }
        }
        Ok(if count { Cmd::Count(c) } else { Cmd::Pick(a) })
    })();
    (r, json)
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

    let scored = pick::score_all(&ix, ports.tree.as_ref(), pool, &ports.config, &ports.ledger, now);
    let mut rng = SeededRng::new(seed);
    let Some((i, stratum)) = pick::draw(&scored, &ports.ledger, &mut rng) else {
        let why = if args.aim.is_some() { "every unit the aim matched is excluded or already reviewed at its current blob" } else { "no unit is drawable: all are excluded or already reviewed at their current blobs" };
        return fail(1, json, why, &[], Some(seed), Some(&commit));
    };
    let chosen = &scored[i];
    let shaped = pick::shape(&ix, &chosen.unit, &ports.config, &mut rng);
    let body = describe(chosen, &shaped, &stratum, &aim_note, seed, &commit, ports.config.page_lines);
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

fn ranges_text(r: &[(u32, u32)]) -> String {
    r.iter().map(|(a, b)| if a == b { a.to_string() } else { format!("{a}-{b}") }).collect::<Vec<_>>().join(",")
}

fn describe(c: &Scored, shaped: &[Shaped], st: &Stratum, aim: &Option<(String, &'static str, usize)>, seed: u64, commit: &str, page_lines: u32) -> (Value, String) {
    let w = &c.weight;
    let review = match w.days_since_sent {
        None => "never sent".to_string(),
        Some(d) => format!("last sent {d:.1} days ago"),
    };
    let stratum_reason = match st.last_sent_ms {
        None => format!("{} has never been sent", st.kind.as_str()),
        Some(t) => format!("{} was last sent at {t} ms, the oldest of the kinds", st.kind.as_str()),
    };
    let files: Vec<Value> = shaped
        .iter()
        .map(|f| {
            json!({
                "path": f.path,
                "lines": f.lines,
                "regions": f.regions,
                "oversized": f.part.as_ref().map(|p| format!("oversized: public surface plus {p}")),
            })
        })
        .collect();
    let v = json!({
        "ok": true,
        "unit": {"id": c.unit.id, "kind": c.unit.kind.as_str(), "title": c.unit.title, "files": files},
        "weight": {
            "review_factor": w.review_factor,
            "review": review,
            "commits": w.commits,
            "churn_term": w.churn_term(),
            "dependents": w.dependents,
            "dependents_term": w.dependents_term(),
            "lines": w.lines,
            "size_term": w.size_term(page_lines),
            "total": w.total,
        },
        "stratum": {"kind": st.kind.as_str(), "reason": stratum_reason, "skipped": st.skipped.iter().map(|k| k.as_str()).collect::<Vec<_>>()},
        "aim": aim.as_ref().map(|(a, r, n)| json!({"aim": a, "resolution": r, "matched_units": n})),
        "seed": seed,
        "commit": commit,
    });
    let mut t = format!("unit: {}\nkind: {}\ntitle: {}\nfiles:\n", c.unit.id, c.unit.kind.as_str(), c.unit.title);
    for f in shaped {
        t.push_str(&format!("  {} ({} lines)", f.path, f.lines));
        if let (Some(r), Some(p)) = (&f.regions, &f.part) {
            t.push_str(&format!(" oversized: public surface plus {p}; lines {}", ranges_text(r)));
        }
        t.push('\n');
    }
    t.push_str(&format!(
        "weight: {:.3} = review {:.2} ({review}) x churn {:.0} (1 + {} commits) x dependents {:.3} (1 + ln(1 + {})) x size {:.3} (min({} lines / {page_lines}, 3))\n",
        w.total, w.review_factor, w.churn_term(), w.commits, w.dependents_term(), w.dependents, w.size_term(page_lines), w.lines
    ));
    t.push_str(&format!("stratum: {stratum_reason}\n"));
    match aim {
        Some((a, r, n)) => t.push_str(&format!("aim: {a:?} resolved as {r}, {n} units matched\n")),
        None => t.push_str("aim: none\n"),
    }
    t.push_str(&format!("seed: {seed}\ncommit: {commit}\n"));
    (v, t)
}
