//! kit-bump pass [--config FILE] --registry URL --crate NAME --state FILE --site NAME:PATH:REF:FILE...
//!
//! `--config FILE` holds the same flags, one `name value` per line (`#` comments), so a site list
//! lives on the host and not in the unit.
//!
//! Exit 0 swept, 1 a site failed (retried next pass), 3 the registry's latest release could not be read.

use kit_bump::{pass, real::Real, Site};
use spira_otel::metrics::{MetricsSink, Point};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: kit-bump pass [--config FILE] --registry URL --crate NAME --state FILE [--persona NAME] --site NAME:PATH:REF:FILE...";
const KEYS: [&str; 5] = ["registry", "crate", "state", "persona", "site"];
pub const COMPONENT: &str = "kit-bump";

fn site(spec: &str) -> Result<Site, String> {
    let f: Vec<&str> = spec.splitn(4, ':').collect();
    match f.as_slice() {
        [n, p, r, file] if ![n, p, r, file].iter().any(|s| s.is_empty()) => {
            Ok(Site { name: n.to_string(), path: p.to_string(), git_ref: r.to_string(), file: file.to_string() })
        }
        _ => Err(format!("--site {spec:?} is not NAME:PATH:REF:FILE")),
    }
}

pub fn expand(argv: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if a != "--config" {
            out.push(a.clone());
            continue;
        }
        let f = it.next().ok_or("--config needs a value")?;
        let text = std::fs::read_to_string(f).map_err(|e| format!("cannot read {f}: {e}"))?;
        for l in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (k, v) = l.split_once(char::is_whitespace).ok_or_else(|| format!("{f}: {l:?} has no value"))?;
            let k = k.trim_start_matches('-');
            if !KEYS.contains(&k) {
                return Err(format!("{f}: unknown key {k:?} (known: {})", KEYS.join(", ")));
            }
            out.push(format!("--{k}"));
            out.push(v.trim().to_string());
        }
    }
    Ok(out)
}

fn unconfigured(argv: &[String]) -> Option<&str> {
    let f = argv.windows(2).find(|w| w[0] == "--config").map(|w| w[1].as_str())?;
    (!std::path::Path::new(f).exists()).then_some(f)
}

fn refresh(argv: &[String]) -> Result<(), String> {
    let Some(conf) = argv.windows(2).find(|w| w[0] == "--config").map(|w| std::path::Path::new(&w[1])) else {
        return Ok(());
    };
    let run = conf.parent().ok_or_else(|| format!("{} has no directory", conf.display()))?;
    let home = spira_config::resolve::locate_home_for_process()?;
    install::kit_bump::install(run, &home, &|k| spira_config::process::cfg(k)).map(|_| ())
}

fn parse(argv: &[String]) -> Result<(Real, Vec<Site>), String> {
    let argv = &expand(argv)?;
    let (mut registry, mut krate, mut state, mut persona) = (None, None, None, "builder".to_string());
    let mut sites = Vec::new();
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        let v = it.next().ok_or_else(|| format!("{a} needs a value"))?.clone();
        match a.as_str() {
            "--registry" => registry = Some(v),
            "--crate" => krate = Some(v),
            "--state" => state = Some(PathBuf::from(v)),
            "--persona" => persona = v,
            "--site" => sites.push(site(&v)?),
            _ => return Err(format!("unknown argument {a}")),
        }
    }
    if sites.is_empty() {
        return Err("at least one --site is required".into());
    }
    let need = |o: Option<String>, n: &str| o.ok_or_else(|| format!("{n} is required"));
    let real = Real::new(
        need(registry, "--registry")?,
        need(krate, "--crate")?,
        state.ok_or("--state is required")?,
        persona,
    );
    Ok((real, sites))
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) != Some("pass") {
        eprintln!("kit-bump: unknown command\n{USAGE}");
        return ExitCode::from(2);
    }
    if let Err(e) = refresh(&argv[1..]) {
        eprintln!("kit-bump: cannot render the config from SPIRA_KIT_*: {e}");
        return ExitCode::from(2);
    }
    if let Some(f) = unconfigured(&argv[1..]) {
        println!("kit-bump: unconfigured — {f} does not exist (set SPIRA_KIT_REGISTRY and the pass renders it)");
        return ExitCode::SUCCESS;
    }
    let (real, sites) = match parse(&argv[1..]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("kit-bump: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    spira_otel::oneshot::begin(endpoint.as_deref(), COMPONENT, "pass");
    let o = match pass(&real, &sites, &real.crate_name) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("kit-bump: cannot read the latest release: {e}");
            spira_otel::oneshot::end(3);
            return ExitCode::from(3);
        }
    };
    let sink = MetricsSink::for_component(endpoint.as_deref(), COMPONENT);
    for s in &o.filed {
        sink.push(&[Point::Count { name: "spira_kit_bumps_filed", value: 1 }], &[("site", s)]);
    }
    if o.unreleased {
        println!("kit-bump: no release yet — {} has no entry for `{}`", real.registry, real.crate_name);
        spira_otel::oneshot::end(0);
        return ExitCode::SUCCESS;
    }
    println!(
        "kit-bump: {} filed, {} amended, {} current, {} failed",
        o.filed.len(),
        o.amended.len(),
        o.current.len(),
        o.failed.len()
    );
    for f in &o.failed {
        eprintln!("kit-bump: failed — {f}");
    }
    let code = i32::from(!o.failed.is_empty());
    spira_otel::oneshot::end(code);
    ExitCode::from(code as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_missing_config_file_is_unconfigured_not_an_error() {
        let d = testkit::TempDir::new("kit-bump-unconfigured");
        let missing = d.path().join("kit-bump.conf");
        let m = missing.to_str().unwrap();
        assert_eq!(unconfigured(&v(&["--config", m])), Some(m));
        std::fs::write(&missing, "registry http://r\n").unwrap();
        assert_eq!(unconfigured(&v(&["--config", m])), None);
        assert_eq!(unconfigured(&v(&["--kit", "/k"])), None);
    }

    #[test]
    fn a_config_with_an_unknown_key_is_refused_naming_the_key_and_the_file() {
        let d = testkit::TempDir::new("kit-bump-oldconf");
        let p = d.path().join("kit-bump.conf");
        std::fs::write(&p, "kit ./kit-checkout\nkit-ref origin/main\n").unwrap();
        let e = expand(&v(&["--config", p.to_str().unwrap()])).unwrap_err();
        assert!(e.contains("\"kit\"") && e.contains(p.to_str().unwrap()), "{e}");
        std::fs::write(&p, "registry http://r\ncrate k\nsite a:/p:main:Cargo.toml\n").unwrap();
        assert_eq!(expand(&v(&["--config", p.to_str().unwrap()])).unwrap().len(), 6);
    }
}
