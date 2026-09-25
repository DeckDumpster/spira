//! `test-plan` — validate the use-case catalogue, list its ids, build the derived coverage
//! matrix, and render it as markdown.
//!
//!   test-plan catalogue-ids --catalogue-dir DIR
//!       one "<id> <tier>" line per declared use case (sorted); exit 1 naming the path on
//!       any catalogue error (unknown field, bad tier, duplicate id).
//!
//!   test-plan validate --catalogue-dir DIR [--suites FILE|-] [--prev-suites FILE|-]
//!       catalogue load errors, then (with --suites) unknown UC ids on a suite's # covers:,
//!       then (with --prev-suites too) use cases orphaned since --prev-suites' snapshot.
//!       "ok" and exit 0 when clean; each violation printed and exit 1 otherwise.
//!
//!   test-plan matrix --catalogue-dir DIR --suites FILE|- [--timings FILE]
//!       the derived coverage matrix as JSON, to stdout.
//!
//!   test-plan render [--matrix FILE|-]
//!       that JSON matrix, as markdown, to stdout.
//!
//!   test-plan schema catalogue|matrix
//!       the JSON Schema the named document is validated against.
//!
//! `FILE|-` means "read from stdin when the value is `-`".

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use test_plan::{
    build_matrix, catalogue_json_schema, load_catalogues, matrix_json_schema, orphan_violations,
    render_markdown, tier_budget_flags, unknown_uc_violations, LoadedCatalogue, MatrixDoc,
    SuiteCoverage,
};

fn read_text(spec: &str) -> Result<String, String> {
    if spec == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| e.to_string())?;
        Ok(s)
    } else {
        fs::read_to_string(spec).map_err(|e| format!("{spec}: {e}"))
    }
}

fn load_suites(spec: &str) -> Result<Vec<SuiteCoverage>, String> {
    let text = read_text(spec)?;
    serde_json::from_str(&text).map_err(|e| format!("{spec}: {e}"))
}

fn load_timings(spec: &str) -> Result<BTreeMap<String, f64>, String> {
    let text = read_text(spec)?;
    serde_json::from_str(&text).map_err(|e| format!("{spec}: {e}"))
}

struct Args {
    catalogue_dir: Option<String>,
    suites: Option<String>,
    prev_suites: Option<String>,
    timings: Option<String>,
    matrix: Option<String>,
}

fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut a = Args {
        catalogue_dir: None,
        suites: None,
        prev_suites: None,
        timings: None,
        matrix: None,
    };
    let mut i = 0;
    while i < rest.len() {
        let (flag, val) = (rest[i].as_str(), rest.get(i + 1));
        match flag {
            "--catalogue-dir" => a.catalogue_dir = val.cloned(),
            "--suites" => a.suites = val.cloned(),
            "--prev-suites" => a.prev_suites = val.cloned(),
            "--timings" => a.timings = val.cloned(),
            "--matrix" => a.matrix = val.cloned(),
            other => return Err(format!("unknown argument {other:?}")),
        }
        i += 2;
    }
    Ok(a)
}

fn load_or_report(dir: &str) -> Result<Vec<LoadedCatalogue>, ExitCode> {
    match load_catalogues(Path::new(dir)) {
        Ok(v) => Ok(v),
        Err(errs) => {
            for e in errs {
                eprintln!("test-plan: {e}");
            }
            Err(ExitCode::FAILURE)
        }
    }
}

fn cmd_catalogue_ids(rest: &[String]) -> ExitCode {
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("test-plan catalogue-ids: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(dir) = a.catalogue_dir else {
        eprintln!("usage: test-plan catalogue-ids --catalogue-dir DIR");
        return ExitCode::FAILURE;
    };
    let cats = match load_or_report(&dir) {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    let mut rows: Vec<(String, &'static str)> = cats
        .iter()
        .flat_map(|lc| lc.catalogue.use_case.iter())
        .map(|uc| (uc.id.clone(), uc.tier.as_str()))
        .collect();
    rows.sort();
    for (id, tier) in rows {
        println!("{id} {tier}");
    }
    ExitCode::SUCCESS
}

fn cmd_validate(rest: &[String]) -> ExitCode {
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("test-plan validate: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(dir) = a.catalogue_dir else {
        eprintln!("usage: test-plan validate --catalogue-dir DIR [--suites FILE|-] [--prev-suites FILE|-]");
        return ExitCode::FAILURE;
    };
    let cats = match load_or_report(&dir) {
        Ok(c) => c,
        Err(rc) => return rc,
    };

    let mut bad = false;

    let cur_suites = if let Some(spec) = &a.suites {
        match load_suites(spec) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("test-plan: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    if let Some(cur) = &cur_suites {
        for v in unknown_uc_violations(&cats, cur) {
            eprintln!("test-plan: {v}");
            bad = true;
        }
    }

    if let Some(prev_spec) = &a.prev_suites {
        let Some(cur) = &cur_suites else {
            eprintln!("test-plan validate: --prev-suites requires --suites");
            return ExitCode::FAILURE;
        };
        let prev = match load_suites(prev_spec) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("test-plan: {e}");
                return ExitCode::FAILURE;
            }
        };
        for v in orphan_violations(&cats, &prev, cur) {
            eprintln!("test-plan: {v}");
            bad = true;
        }
    }

    if bad {
        ExitCode::FAILURE
    } else {
        println!("ok");
        ExitCode::SUCCESS
    }
}

fn cmd_matrix(rest: &[String]) -> ExitCode {
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("test-plan matrix: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (Some(dir), Some(suites_spec)) = (a.catalogue_dir, a.suites) else {
        eprintln!("usage: test-plan matrix --catalogue-dir DIR --suites FILE|- [--timings FILE]");
        return ExitCode::FAILURE;
    };
    let cats = match load_or_report(&dir) {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    let suites = match load_suites(&suites_spec) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("test-plan: {e}");
            return ExitCode::FAILURE;
        }
    };
    let timings = match &a.timings {
        Some(spec) => match load_timings(spec) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("test-plan: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => BTreeMap::new(),
    };

    for f in tier_budget_flags(&suites, &timings) {
        eprintln!("test-plan: TIER HONESTY: {f}");
    }

    let doc = build_matrix(&cats, &suites, &timings);
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("test-plan: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_render(rest: &[String]) -> ExitCode {
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("test-plan render: {e}");
            return ExitCode::FAILURE;
        }
    };
    let spec = a.matrix.unwrap_or_else(|| "-".to_string());
    let text = match read_text(&spec) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("test-plan: {e}");
            return ExitCode::FAILURE;
        }
    };
    let doc: MatrixDoc = match serde_json::from_str(&text) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("test-plan: {spec}: {e}");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", render_markdown(&doc));
    ExitCode::SUCCESS
}

fn cmd_schema(which: Option<&str>) -> ExitCode {
    let value = match which {
        Some("catalogue") => serde_json::to_string_pretty(&catalogue_json_schema()),
        Some("matrix") => serde_json::to_string_pretty(&matrix_json_schema()),
        _ => {
            eprintln!("usage: test-plan schema <catalogue|matrix>");
            return ExitCode::FAILURE;
        }
    };
    match value {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("test-plan: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: test-plan <catalogue-ids|validate|matrix|render|schema> ...\n\
         \n\
         \x20 catalogue-ids --catalogue-dir DIR\n\
         \x20 validate --catalogue-dir DIR [--suites FILE|-] [--prev-suites FILE|-]\n\
         \x20 matrix --catalogue-dir DIR --suites FILE|- [--timings FILE]\n\
         \x20 render [--matrix FILE|-]\n\
         \x20 schema <catalogue|matrix>"
    );
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("catalogue-ids") => cmd_catalogue_ids(&args[1..]),
        Some("validate") => cmd_validate(&args[1..]),
        Some("matrix") => cmd_matrix(&args[1..]),
        Some("render") => cmd_render(&args[1..]),
        Some("schema") => cmd_schema(args.get(1).map(String::as_str)),
        _ => usage(),
    }
}
