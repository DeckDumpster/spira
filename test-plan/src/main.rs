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
//!       NOTE: the unknown-UC check here is whole-corpus and not yet gate-wired (sp-94lbj);
//!       the gate uses `orphans` below, which is narrower on purpose.
//!
//!   test-plan orphans --catalogue-dir DIR --suites FILE|- --prev-suites FILE|-
//!       catalogue load errors, then use cases orphaned since --prev-suites' snapshot only —
//!       never the whole-corpus unknown-UC check `validate` also runs, which would fail on
//!       every branch today against areas `docs/test-plan/*.toml` hasn't migrated yet.
//!       "ok" and exit 0 when clean; each violation printed and exit 1 otherwise.
//!
//!   test-plan launcher-gaps --catalogue-dir DIR --suites FILE|-
//!       one line per declared launcher (a use case with a `launcher` table) no suite covers.
//!       Reported, exit 0.
//!
//!   test-plan launcher-sites --catalogue-dir DIR --root DIR
//!       each declared launcher's site must exist under DIR and contain its needle; exit 1
//!       naming each that does not.
//!
//!   test-plan matrix --catalogue-dir DIR --suites FILE|- [--timings FILE]
//!       the derived coverage matrix as JSON, to stdout.
//!
//!   test-plan render [--matrix FILE|-]
//!       that JSON matrix, as markdown, to stdout.
//!
//!   test-plan write-matrix --catalogue-dir DIR --suites FILE|- [--timings FILE]
//!                          --out-json PATH --out-md PATH
//!       matrix + render in one process, written to PATH and PATH instead of stdout.
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
    build_matrix, catalogue_json_schema, coverage_gaps, launcher_gaps, launcher_site_violations, load_catalogues, matrix_json_schema, orphan_violations,
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
    root: Option<String>,
    out_json: Option<String>,
    out_md: Option<String>,
}

fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut a = Args {
        catalogue_dir: None,
        suites: None,
        prev_suites: None,
        timings: None,
        matrix: None,
        root: None,
        out_json: None,
        out_md: None,
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
            "--root" => a.root = val.cloned(),
            "--out-json" => a.out_json = val.cloned(),
            "--out-md" => a.out_md = val.cloned(),
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

fn cmd_orphans(rest: &[String]) -> ExitCode {
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("test-plan orphans: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (Some(dir), Some(suites_spec), Some(prev_spec)) =
        (a.catalogue_dir, a.suites, a.prev_suites)
    else {
        eprintln!("usage: test-plan orphans --catalogue-dir DIR --suites FILE|- --prev-suites FILE|-");
        return ExitCode::FAILURE;
    };
    let cats = match load_or_report(&dir) {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    let cur = match load_suites(&suites_spec) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("test-plan: {e}");
            return ExitCode::FAILURE;
        }
    };
    let prev = match load_suites(&prev_spec) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("test-plan: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut bad = false;
    for v in orphan_violations(&cats, &prev, &cur) {
        eprintln!("test-plan: {v}");
        bad = true;
    }

    if bad {
        ExitCode::FAILURE
    } else {
        println!("ok");
        ExitCode::SUCCESS
    }
}

fn cmd_gaps(rest: &[String]) -> ExitCode {
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("test-plan gaps: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (Some(dir), Some(suites_spec)) = (a.catalogue_dir, a.suites) else {
        eprintln!("usage: test-plan gaps --catalogue-dir DIR --suites FILE|-");
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
    let gaps = coverage_gaps(&cats, &suites);
    for g in &gaps {
        println!("{g}");
    }
    if gaps.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn cmd_launcher_gaps(rest: &[String]) -> ExitCode {
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("test-plan launcher-gaps: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (Some(dir), Some(suites_spec)) = (a.catalogue_dir, a.suites) else {
        eprintln!("usage: test-plan launcher-gaps --catalogue-dir DIR --suites FILE|-");
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
    for g in launcher_gaps(&cats, &suites) {
        println!("{g}");
    }
    ExitCode::SUCCESS
}

fn cmd_launcher_sites(rest: &[String]) -> ExitCode {
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("test-plan launcher-sites: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (Some(dir), Some(root)) = (a.catalogue_dir, a.root) else {
        eprintln!("usage: test-plan launcher-sites --catalogue-dir DIR --root DIR");
        return ExitCode::FAILURE;
    };
    let cats = match load_or_report(&dir) {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    let bad = launcher_site_violations(&cats, Path::new(&root));
    for v in &bad {
        eprintln!("test-plan: {v}");
    }
    if bad.is_empty() {
        println!("ok");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
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

/// `write-matrix` — build the coverage matrix and render it, in one process, and write both
/// documents to disk. Replaces the two-binary-call-plus-cp shape `spira/plan-matrix.sh` used
/// to do by hand (sp-pppt0): gathering the suite/timings JSON stays the caller's job (git
/// history and `run/tsd/` are its concerns, not this crate's), but building, rendering and
/// writing the two derived documents is now one call, so they can never disagree with each
/// other the way two separate writes briefly could.
fn cmd_write_matrix(rest: &[String]) -> ExitCode {
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("test-plan write-matrix: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (Some(dir), Some(suites_spec), Some(out_json), Some(out_md)) =
        (a.catalogue_dir, a.suites, a.out_json, a.out_md)
    else {
        eprintln!(
            "usage: test-plan write-matrix --catalogue-dir DIR --suites FILE|- [--timings FILE] --out-json PATH --out-md PATH"
        );
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
    let json = match serde_json::to_string_pretty(&doc) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("test-plan: {e}");
            return ExitCode::FAILURE;
        }
    };
    let md = render_markdown(&doc);
    if let Err(e) = fs::write(&out_json, json) {
        eprintln!("test-plan: {out_json}: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = fs::write(&out_md, md) {
        eprintln!("test-plan: {out_md}: {e}");
        return ExitCode::FAILURE;
    }
    println!("test-plan write-matrix: wrote {out_json} and {out_md}");
    ExitCode::SUCCESS
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
        "usage: test-plan <catalogue-ids|validate|orphans|gaps|launcher-gaps|launcher-sites|matrix|render|write-matrix|schema> ...\n\
         \n\
         \x20 catalogue-ids --catalogue-dir DIR\n\
         \x20 validate --catalogue-dir DIR [--suites FILE|-] [--prev-suites FILE|-]\n\
         \x20 orphans --catalogue-dir DIR --suites FILE|- --prev-suites FILE|-\n\
         \x20 gaps --catalogue-dir DIR --suites FILE|-\n\
         \x20 launcher-gaps --catalogue-dir DIR --suites FILE|-\n\
         \x20 launcher-sites --catalogue-dir DIR --root DIR\n\
         \x20 matrix --catalogue-dir DIR --suites FILE|- [--timings FILE]\n\
         \x20 render [--matrix FILE|-]\n\
         \x20 write-matrix --catalogue-dir DIR --suites FILE|- [--timings FILE] --out-json PATH --out-md PATH\n\
         \x20 schema <catalogue|matrix>"
    );
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("catalogue-ids") => cmd_catalogue_ids(&args[1..]),
        Some("validate") => cmd_validate(&args[1..]),
        Some("orphans") => cmd_orphans(&args[1..]),
        Some("gaps") => cmd_gaps(&args[1..]),
        Some("launcher-gaps") => cmd_launcher_gaps(&args[1..]),
        Some("launcher-sites") => cmd_launcher_sites(&args[1..]),
        Some("matrix") => cmd_matrix(&args[1..]),
        Some("render") => cmd_render(&args[1..]),
        Some("write-matrix") => cmd_write_matrix(&args[1..]),
        Some("schema") => cmd_schema(args.get(1).map(String::as_str)),
        _ => usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `write-matrix` builds and renders in one call and writes both files — the shape
    /// `plan-matrix.sh` used to get by calling `matrix` then `render` then `cp`-ing twice.
    #[test]
    fn write_matrix_builds_renders_and_writes_both_files() {
        let t = testkit::TempDir::new("test-plan-write-matrix");
        let dir = t.join("docs/test-plan");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("demo.toml"),
            "api_version = \"test-plan/v1\"\narea = \"demo\"\n\n[[use_case]]\nid = \"UC-demo-01\"\ntier = \"T0\"\nstatement = \"one\"\n",
        )
        .unwrap();
        let suites = t.join("suites.json");
        std::fs::write(&suites, r#"[{"path":"spira/test-a.sh","tier":"T0","covers":["UC-demo-01"]}]"#).unwrap();
        let out_json = t.join("coverage.json");
        let out_md = t.join("COVERAGE.md");

        let rc = cmd_write_matrix(&[
            "--catalogue-dir".into(),
            dir.to_str().unwrap().into(),
            "--suites".into(),
            suites.to_str().unwrap().into(),
            "--out-json".into(),
            out_json.to_str().unwrap().into(),
            "--out-md".into(),
            out_md.to_str().unwrap().into(),
        ]);
        assert_eq!(format!("{rc:?}"), format!("{:?}", ExitCode::SUCCESS));

        let json = std::fs::read_to_string(&out_json).unwrap();
        assert!(json.contains("UC-demo-01"), "{json}");
        let doc: MatrixDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(render_markdown(&doc), std::fs::read_to_string(&out_md).unwrap());
    }

    /// A malformed catalogue is reported and neither output file is written.
    #[test]
    fn write_matrix_reports_a_malformed_catalogue_and_writes_nothing() {
        let t = testkit::TempDir::new("test-plan-write-matrix-bad");
        let dir = t.join("docs/test-plan");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("demo.toml"), "api_version = \"test-plan/v1\"\narea = \"demo\"\nbogus = 1\n").unwrap();
        let suites = t.join("suites.json");
        std::fs::write(&suites, "[]").unwrap();
        let out_json = t.join("coverage.json");
        let out_md = t.join("COVERAGE.md");

        let rc = cmd_write_matrix(&[
            "--catalogue-dir".into(),
            dir.to_str().unwrap().into(),
            "--suites".into(),
            suites.to_str().unwrap().into(),
            "--out-json".into(),
            out_json.to_str().unwrap().into(),
            "--out-md".into(),
            out_md.to_str().unwrap().into(),
        ]);
        assert_eq!(format!("{rc:?}"), format!("{:?}", ExitCode::FAILURE));
        assert!(!out_json.exists());
        assert!(!out_md.exists());
    }

    #[test]
    fn write_matrix_without_out_paths_is_a_usage_error() {
        let rc = cmd_write_matrix(&["--catalogue-dir".into(), "x".into(), "--suites".into(), "-".into()]);
        assert_eq!(format!("{rc:?}"), format!("{:?}", ExitCode::FAILURE));
    }
}
