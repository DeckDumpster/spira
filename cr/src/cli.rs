//! `cr` command path: argv in, `Outcome` out. `main` only builds the real ports.

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

  cr pick [--aim <X>] [--seed <n>] [--json]

  --aim <X>   aim the draw: a path, a workspace crate, a struct/enum/trait/type name, or words
              (every word must appear in a unit's paths or module docs, case-folded)
  --seed <n>  the draw's seed; default SPIRA_SEED, else the clock's milliseconds; always printed
  --json      print one JSON object on every path, errors included
  -h, --help  this text

Arguments are named; a bare positional is refused.

Exit codes:
  0  a unit was drawn
  1  the draw could not run: tree, ledger or config unreadable, or no unit is drawable
  2  usage: unknown or malformed argument, or --aim matched nothing
";

pub fn wants_help(argv: &[String]) -> bool {
    argv.iter().any(|a| a == "-h" || a == "--help")
}

pub fn wants_json(argv: &[String]) -> bool {
    argv.iter().any(|a| a == "--json")
}

struct Args {
    aim: Option<String>,
    seed: Option<u64>,
    json: bool,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut it = argv.iter();
    match it.next().map(String::as_str) {
        Some("pick") => {}
        Some(other) => return Err(format!("unknown command `{other}`; the only command is `cr pick` (see -h)")),
        None => return Err("no command; use `cr pick` (see -h)".to_string()),
    }
    let mut a = Args { aim: None, seed: None, json: false };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--aim" => a.aim = Some(it.next().ok_or("--aim needs a value")?.clone()),
            "--seed" => {
                let v = it.next().ok_or("--seed needs a value")?;
                a.seed = Some(v.parse().map_err(|_| format!("--seed {v:?} is not a non-negative integer"))?);
            }
            "--json" => a.json = true,
            f if f.starts_with('-') => return Err(format!("unknown flag `{f}` (see -h)")),
            positional => return Err(format!("positional argument `{positional}` is not accepted; name it: `--aim {positional}`")),
        }
    }
    Ok(a)
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
    if wants_help(argv) {
        return Outcome { code: 0, stdout: HELP.to_string(), stderr: String::new() };
    }
    let json = wants_json(argv);
    let args = match parse(argv) {
        Ok(a) => a,
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
