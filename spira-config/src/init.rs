//! A fresh box's first `spira.toml`, produced from the operator's answers (per Ryan
//! 2026-10-07: install PRODUCES spira.toml from user-provided inputs when none exists).
//!
//! The handful of values that genuinely vary per box are asked for — [`REQUIRED`]; every
//! other key keeps its registered default. Every path is pinned explicitly in the file, so
//! nothing silently falls back to a default store. An existing file is validated and used,
//! never overwritten. With an input missing and nobody to ask, the refusal names exactly
//! which inputs are missing.
//!
//! Answers come from an answers file (`key = value` lines, `#` comments, values optionally
//! quoted — the `[spira]` key names), from flags, or from a prompt. Beyond [`REQUIRED`] and
//! the home repository's row ([`HOME_REPO_ROW`]), an answers file may carry any other
//! registered `[spira]` key; it is written as given and validated like every other.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The inputs every fresh install must name: `(key, what it is)`.
pub const REQUIRED: &[(&str, &str)] = &[
    ("instance", "the name of this installation (prod for the only one on a box)"),
    ("id_prefix", "the prefix of this installation's bead ids (e.g. sp)"),
    ("home_repo", "the name of the repository this harness works on"),
    ("db", "the beads database directory"),
    ("run", "the runtime state directory"),
    ("releases", "the directory releases are installed under"),
    ("dolt_data", "the Dolt server's data directory"),
];

/// The home repository's row in the repo map: `(key, what it is, default)`. Asked only when
/// `home_repo_path` is answered — a box whose repo map already carries the row skips it.
pub const HOME_REPO_ROW: &[(&str, &str, &str)] = &[
    ("home_repo_path", "the home repository's checkout (blank: the repo map already names it)", ""),
    ("home_repo_land", "how its work lands: push, pr or queue.local", "push"),
    ("home_repo_base", "the ref its work lands on", "origin/main"),
];

/// What [`ensure`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A file was already there; it validated and is the one in force.
    Existing(PathBuf),
    /// A new file was written from the answers.
    Written(PathBuf),
}

/// Where a box's config is: the spec `SPIRA_TOML` names (one file, or base:override layers),
/// else the canonical `${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml`.
pub fn default_out(env: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    if let Some(t) = env.get("SPIRA_TOML").filter(|t| !t.is_empty()) {
        // One file, or base:override layers — the spec as named; [`ensure`] judges it.
        return Ok(PathBuf::from(t));
    }
    let base = match (env.get("XDG_CONFIG_HOME").filter(|v| !v.is_empty()), env.get("HOME").filter(|v| !v.is_empty())) {
        (Some(x), _) => PathBuf::from(x),
        (None, Some(h)) => PathBuf::from(h).join(".config"),
        (None, None) => return Err("neither XDG_CONFIG_HOME nor HOME is set — name the config file with --out".into()),
    };
    Ok(base.join("spira").join(crate::FILE_NAME))
}

/// An answers file: `key = value` per line; blank lines and `#` comments ignored; a value
/// may be wrapped in double quotes. A key may be written bare or as `spira.<key>`.
pub fn parse_answers(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let (k, v) = l.split_once('=').ok_or_else(|| format!("answers line {}: not key = value: {l}", n + 1))?;
        let k = k.trim();
        let k = k.strip_prefix("spira.").unwrap_or(k).to_string();
        let v = v.trim();
        let v = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(v).to_string();
        if k.is_empty() {
            return Err(format!("answers line {}: empty key", n + 1));
        }
        out.insert(k, v);
    }
    Ok(out)
}

/// The inputs `answers` still lacks, in [`REQUIRED`] order.
pub fn missing(answers: &BTreeMap<String, String>) -> Vec<&'static str> {
    REQUIRED.iter().map(|(k, _)| *k).filter(|k| answers.get(*k).is_none_or(|v| v.trim().is_empty())).collect()
}

/// What a fresh config's other keys are computed from: the release's key registry
/// (`spira/conf.d`) and the ambient `HOME`/`XDG_*` the registered defaults are written in.
pub struct Registry<'a> {
    pub conf_d: &'a Path,
    pub env: &'a BTreeMap<String, String>,
}

/// `SPIRA_FOO` / `COCKPIT_FOO` -> `spira.foo` / `spira.cockpit_foo`.
fn key_path(key: &str) -> String {
    format!("spira.{}", key.strip_prefix("SPIRA_").unwrap_or(key).to_ascii_lowercase())
}

/// The `spira.toml` text for `answers`: the answers themselves, `repo_map` pinned (default:
/// `repo-map` beside `out`), and every other registered key at its registered default as it
/// resolves on this box — every key declared, because a process reads nothing else. The
/// result must resolve under the reading rule (no defaults), or this refuses naming why.
pub fn render(answers: &BTreeMap<String, String>, out: &Path, reg: &Registry<'_>) -> Result<String, String> {
    let gone = missing(answers);
    if !gone.is_empty() {
        return Err(format!("missing required input(s): {}", gone.join(", ")));
    }
    let mut a = answers.clone();
    if a.get("repo_map").is_none_or(|v| v.is_empty()) {
        let dir = out.parent().unwrap_or(Path::new("."));
        a.insert("repo_map".into(), dir.join("repo-map").display().to_string());
    }
    // The lifecycle credential install creates (phase 1.5): pinned now, because its registered
    // default names the file only once it exists — here, it does not yet.
    if a.get("lc_password_file").is_none_or(|v| v.is_empty()) {
        a.insert("lc_password_file".into(), crate::resolve::lc_credential_default(reg.env));
    }
    // The bd every process runs: found once, here, on this box's PATH (what conf.sh's
    // env-bootstrap derived on every source); nothing derives it after the cutover. Not found
    // is an input the operator must answer.
    if a.get("bd").is_none_or(|v| v.is_empty()) {
        let path = crate::env_bootstrap::path_tail(
            reg.env.get("PATH").map(String::as_str).unwrap_or(""),
            a.get("path").map(String::as_str).unwrap_or(""),
            reg.env.get("HOME").map(String::as_str).unwrap_or(""),
        );
        match crate::env_bootstrap::resolve_bd(&path) {
            bd if bd.starts_with('/') => {
                a.insert("bd".into(), bd);
            }
            _ => return Err("no bd on PATH — install beads, or answer bd with its absolute path".into()),
        }
    }
    let row_keys: Vec<&str> = HOME_REPO_ROW.iter().map(|(k, _, _)| *k).collect();
    let mut doc = crate::SpiraToml::default();
    for (k, v) in &a {
        if row_keys.contains(&k.as_str()) {
            continue;
        }
        doc = crate::set_path(&doc, &format!("spira.{k}"), v).map_err(|e| format!("answer {k} = {v:?}: {e}"))?;
    }
    // The installed release's own tree is where every release-relative default points: the
    // stable `current` link, never the directory this file happened to be generated from.
    let home = PathBuf::from(&a["releases"]).join("current/spira");
    let repo = home.parent().unwrap_or(Path::new("/")).to_path_buf();
    macro_rules! input {
        ($toml:expr) => {
            crate::resolve::ResolveInput { env: reg.env, home: &home, repo: &repo, toml: $toml, conf_d: reg.conf_d }
        };
    }
    let all = crate::resolve::resolve_with_defaults(input!(Some(&doc))).map_err(|e| format!("cannot compute the registered defaults: {e}"))?;
    let declared = crate::spira_value_map(&doc, true);
    let mut unset = Vec::new();
    let mut no_default = Vec::new();
    for (key, v) in &all.values {
        let path = key_path(key);
        let field = path.trim_start_matches("spira.").to_ascii_uppercase();
        if declared.contains_key(&field) {
            continue;
        }
        // A list key resolves to its words; a key that is not a [spira] field (a fixed gate
        // constant) is not the operator's to declare.
        let as_list = || serde_json::to_string(&v.split_whitespace().collect::<Vec<_>>()).unwrap_or_default();
        let as_bool = || match v.as_str() {
            "0" => "false".to_string(),
            "1" => "true".to_string(),
            o => o.to_string(),
        };
        match crate::set_path(&doc, &path, v)
            .or_else(|e| crate::set_path(&doc, &path, &as_list()).or_else(|_| crate::set_path(&doc, &path, &as_bool())).map_err(|_| e)) {
            Ok(d) => doc = d,
            Err(e) if e.contains("unknown field") => {} // a fixed constant, not a [spira] key
            Err(_) if v.is_empty() => no_default.push(path.trim_start_matches("spira.").to_string()),
            Err(e) => unset.push(format!("{path} = {v:?} ({e})")),
        }
    }
    // The personas: one [persona.<name>] per `*.fayth` the release ships beside its registry
    // (spira/chamber), read the way convert reads them, against the [spira] just written.
    let chamber = reg.conf_d.parent().unwrap_or(Path::new("/")).join("chamber");
    let mut fayths: Vec<PathBuf> = std::fs::read_dir(&chamber)
        .map_err(|e| format!("no personas: {}: {e}", chamber.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "fayth"))
        .collect();
    fayths.sort();
    if fayths.is_empty() {
        return Err(format!("no personas: no *.fayth under {}", chamber.display()));
    }
    let spira_section = doc.spira.clone().unwrap_or_default();
    let mut warnings = crate::convert::ConvertWarnings::default();
    for f in &fayths {
        let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let (name, section) = crate::convert::persona_section(&text, &spira_section, &mut warnings);
        if name.is_empty() {
            return Err(format!("{}: no FAYTH_NAME", f.display()));
        }
        doc.persona.insert(name, section);
    }
    // A typed key with no registered default cannot be declared empty, and its readers need a
    // value: it is the operator's to answer, never this function's to invent.
    if !no_default.is_empty() {
        return Err(format!("no registered default for typed key(s) — answer them: {}", no_default.join(", ")));
    }
    let text = toml::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    let checked = crate::validate(&text)?;
    crate::require_id_prefix(&checked)?;
    crate::resolve::resolve(input!(Some(&checked))).map_err(|e| format!("the generated config does not resolve: {e}; defaults the schema refused: {}", unset.join("; ")))?;
    Ok(format!(
        "# Produced from the operator's answers at install (spira-config init): the answers, and\n# every other key at its registered default. Change one with `spira-config set`.\n{text}"
    ))
}

/// The repo-map row for the home repository, when `home_repo_path` was answered.
fn home_row(a: &BTreeMap<String, String>) -> Option<String> {
    let path = a.get("home_repo_path").filter(|p| !p.is_empty())?;
    let get = |k: &str, d: &str| a.get(k).filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| d.to_string());
    Some(format!("{} | {} | {} | {} | |\n", a.get("home_repo")?, path, get("home_repo_land", "push"), get("home_repo_base", "origin/main")))
}

/// Make sure the config at `out` exists: validate and use one already there, else ask
/// `ask` (a prompt, or `None` with nobody to ask) for each required input `answers` lacks,
/// then write the file — and, when the home repository's checkout was named, its repo-map
/// row (never a second row for a name the map already carries).
pub fn ensure(
    out: &Path,
    mut answers: BTreeMap<String, String>,
    ask: Option<&mut dyn FnMut(&str, &str, &str) -> Option<String>>,
    reg: &Registry<'_>,
) -> Result<Outcome, String> {
    // A layered spec (base:override) is a config already in force when every layer is there;
    // it is validated as the reader loads it, layers and all. A fresh config is one file.
    let spec = out.to_string_lossy().to_string();
    let layers: Vec<&str> = spec.split(':').filter(|p| !p.is_empty()).collect();
    if out.exists() || (layers.len() > 1 && layers.iter().all(|p| Path::new(p).is_file())) {
        crate::load_strict(out).map_err(|e| format!("{spec}: {e}"))?;
        return Ok(Outcome::Existing(out.to_path_buf()));
    }
    if layers.len() > 1 {
        let gone: Vec<&str> = layers.iter().copied().filter(|p| !Path::new(p).is_file()).collect();
        return Err(format!("SPIRA_TOML names layers ({spec}) and {} is missing; a fresh config is one file", gone.join(", ")));
    }
    if let Some(ask) = ask {
        for (k, what) in REQUIRED {
            if answers.get(*k).is_none_or(|v| v.trim().is_empty()) {
                if let Some(v) = ask(k, what, "") {
                    answers.insert(k.to_string(), v);
                }
            }
        }
        // The row's path first; land mode and base only when a path was given.
        for (k, what, d) in HOME_REPO_ROW {
            if *k != "home_repo_path" && answers.get("home_repo_path").is_none_or(|p| p.is_empty()) {
                break;
            }
            if !answers.contains_key(*k) {
                if let Some(v) = ask(k, what, d) {
                    answers.insert(k.to_string(), v);
                }
            }
        }
    }
    let gone = missing(&answers);
    if !gone.is_empty() {
        return Err(format!(
            "no config at {} and missing required input(s): {} — answer them (spira-config init --answers FILE, or --<key> VALUE)",
            out.display(),
            gone.join(", ")
        ));
    }
    let text = render(&answers, out, reg)?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if let Some(row) = home_row(&answers) {
        let map = crate::get_path(&crate::validate(&text)?, "spira.repo_map").unwrap_or_default();
        let map = PathBuf::from(map);
        let have = std::fs::read_to_string(&map).unwrap_or_default();
        let name = answers.get("home_repo").cloned().unwrap_or_default();
        let named = have.lines().any(|l| l.split('|').next().map(str::trim) == Some(name.as_str()));
        if !named {
            let mut body = have;
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(&row);
            crate::write_atomic(&map, &body).map_err(|e| format!("{}: {e}", map.display()))?;
        }
    }
    // Never overwrite: create_new refuses a file that appeared since the check above.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(out).map_err(|e| format!("{}: {e}", out.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(Outcome::Written(out.to_path_buf()))
}

/// [`ensure`] for a CLI: answers from `answers_file` (if any) overlaid by `flags`, a prompt on
/// the terminal when stdin is one, and `out` defaulting to [`default_out`].
pub fn ensure_from_cli(out: Option<PathBuf>, answers_file: Option<&Path>, flags: &BTreeMap<String, String>, conf_d: &Path) -> Result<Outcome, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let reg = Registry { conf_d, env: &env };
    let out = match out {
        Some(o) => o,
        None => default_out(&env)?,
    };
    let mut answers = match answers_file {
        Some(f) => parse_answers(&std::fs::read_to_string(f).map_err(|e| format!("answers file {}: {e}", f.display()))?)?,
        None => BTreeMap::new(),
    };
    answers.extend(flags.iter().map(|(k, v)| (k.clone(), v.clone())));
    use std::io::IsTerminal;
    let mut prompt = |k: &str, what: &str, default: &str| -> Option<String> {
        use std::io::{BufRead, Write};
        let d = if default.is_empty() { String::new() } else { format!(" [{default}]") };
        eprint!("{k} — {what}{d}: ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).ok()?;
        let v = line.trim();
        Some(if v.is_empty() { default.to_string() } else { v.to_string() })
    };
    let ask: Option<&mut dyn FnMut(&str, &str, &str) -> Option<String>> = if std::io::stdin().is_terminal() { Some(&mut prompt) } else { None };
    ensure(&out, answers, ask, &reg)
}

/// `--<key> VALUE` pairs out of `args` (dashes in a key become underscores); every other
/// argument comes back untouched, in order.
pub fn split_flags(args: &[String], keep: &[&str]) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let mut flags = BTreeMap::new();
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.strip_prefix("--") {
            Some(k) if !keep.contains(&a.as_str()) => {
                let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
                flags.insert(k.replace('-', "_"), v.clone());
            }
            _ => rest.push(a.clone()),
        }
    }
    Ok((flags, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real registry this tree ships, defaults written against a fake home.
    fn reg() -> Registry<'static> {
        let env: &'static BTreeMap<String, String> = Box::leak(Box::new([("HOME".to_string(), "/b/home".to_string())].into_iter().collect()));
        Registry { conf_d: Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../spira/conf.d")), env }
    }

    #[test]
    fn every_registered_key_is_declared_and_the_file_resolves_without_defaults() {
        let d = testkit::TempDir::new("spira-config-init-complete");
        let out = d.path().join("spira.toml");
        ensure(&out, full(), None, &reg()).unwrap();
        let doc = crate::load(&out).unwrap();
        let home = PathBuf::from("/b/rel/current/spira");
        let r = reg();
        let got = crate::resolve::resolve(crate::resolve::ResolveInput { env: r.env, home: &home, repo: Path::new("/b/rel/current"), toml: Some(&doc), conf_d: r.conf_d });
        assert!(got.is_ok(), "{:?}", got.err().map(|e| e.to_string()));
        assert_eq!(crate::get_path(&doc, "spira.instance").as_deref(), Some("acc"));
    }

    fn full() -> BTreeMap<String, String> {
        parse_answers(
            concat!(
                "# a box\ninstance = acc\nid_prefix = sp\nhome_repo = scratch\ndb = /b/db\nrun = /b/run\nreleases = \"/b/rel\"\nspira.dolt_data = /b/dolt\noperated = 0\n",
                "bd = /b/bin/bd\n",
            ),
        )
        .unwrap()
    }

    #[test]
    fn a_typed_key_with_no_registered_default_must_be_answered() {
        // The shipped registry gives every typed key a default; a registry that does not
        // (here, summon_lock_wait's default removed) must be answered, never invented.
        let d = testkit::TempDir::new("spira-config-init-nodefault");
        let conf_d = d.path().join("spira/conf.d");
        std::fs::create_dir_all(&conf_d).unwrap();
        std::os::unix::fs::symlink(reg().conf_d.parent().unwrap().join("chamber"), d.path().join("spira/chamber")).unwrap();
        for e in std::fs::read_dir(reg().conf_d).unwrap().flatten() {
            let mut t = std::fs::read_to_string(e.path()).unwrap();
            if e.file_name() == "SPIRA_SUMMON_LOCK_WAIT" {
                t = t.lines().filter(|l| !l.contains(":=")).map(|l| format!("{l}\n")).collect();
            }
            std::fs::write(conf_d.join(e.file_name()), t).unwrap();
        }
        let r = Registry { conf_d: &conf_d, env: reg().env };
        let e = render(&full(), &d.path().join("spira.toml"), &r).unwrap_err();
        assert!(e.contains("summon_lock_wait") && !e.contains("certify_par"), "{e}");
        let mut a = full();
        a.insert("summon_lock_wait".into(), "30".into());
        render(&a, &d.path().join("spira.toml"), &r).unwrap();
    }

    #[test]
    fn bd_is_pinned_from_path_or_refused_by_name() {
        let d = testkit::TempDir::new("spira-config-init-bd");
        std::fs::create_dir_all(d.path().join("bin")).unwrap();
        testkit::write_exe(&d.path().join("bin/bd"), "#!/bin/sh\n");
        let mut a = full();
        a.remove("bd");
        let env: BTreeMap<String, String> = [("HOME".to_string(), "/b/home".to_string()), ("PATH".to_string(), d.path().join("bin").display().to_string())].into_iter().collect();
        let r = Registry { conf_d: reg().conf_d, env: &env };
        let text = render(&a, &d.path().join("spira.toml"), &r).unwrap();
        assert!(text.contains(&format!("bd = \"{}\"", d.path().join("bin/bd").display())), "{text}");
        let none: BTreeMap<String, String> = [("HOME".to_string(), "/b/home".to_string()), ("PATH".to_string(), "/nonexistent".to_string())].into_iter().collect();
        let e = render(&a, &d.path().join("spira.toml"), &Registry { conf_d: reg().conf_d, env: &none }).unwrap_err();
        assert!(e.contains("no bd on PATH"), "{e}");
    }

    #[test]
    fn every_shipped_persona_gets_its_table_with_a_model() {
        let d = testkit::TempDir::new("spira-config-init-persona");
        let text = render(&full(), &d.path().join("spira.toml"), &reg()).unwrap();
        let doc = crate::validate(&text).unwrap();
        assert!(doc.persona.contains_key("builder"), "{:?}", doc.persona.keys().collect::<Vec<_>>());
        assert!(crate::get_path(&doc, "persona.builder.model").is_some_and(|m| !m.is_empty()));
    }

    #[test]
    fn answers_parse_bare_quoted_and_prefixed_keys() {
        let a = full();
        assert_eq!(a["releases"], "/b/rel");
        assert_eq!(a["dolt_data"], "/b/dolt");
        assert!(parse_answers("no equals here").is_err());
    }

    #[test]
    fn missing_inputs_are_named_exactly() {
        let mut a = full();
        a.remove("db");
        a.insert("run".into(), " ".into());
        assert_eq!(missing(&a), vec!["db", "run"]);
    }

    #[test]
    fn a_fresh_box_gets_every_path_pinned_and_a_valid_file() {
        let d = testkit::TempDir::new("spira-config-init-fresh");
        let out = d.path().join("cfg/spira.toml");
        let mut a = full();
        a.insert("home_repo_path".into(), "/b/scratch".into());
        assert_eq!(ensure(&out, a, None, &reg()).unwrap(), Outcome::Written(out.clone()));
        let doc = crate::load(&out).unwrap();
        assert_eq!(crate::get_path(&doc, "spira.db").as_deref(), Some("/b/db"));
        assert_eq!(crate::get_path(&doc, "spira.repo_map").as_deref(), Some(d.path().join("cfg/repo-map").to_str().unwrap()));
        let map = std::fs::read_to_string(d.path().join("cfg/repo-map")).unwrap();
        assert_eq!(map, "scratch | /b/scratch | push | origin/main | |\n");
    }

    #[test]
    fn an_existing_repo_map_row_is_never_duplicated() {
        let d = testkit::TempDir::new("spira-config-init-row");
        let out = d.path().join("spira.toml");
        std::fs::write(d.path().join("repo-map"), "scratch | /b/x | queue.local | local/main | |\n").unwrap();
        let mut a = full();
        a.insert("home_repo_path".into(), "/b/scratch".into());
        ensure(&out, a, None, &reg()).unwrap();
        assert_eq!(std::fs::read_to_string(d.path().join("repo-map")).unwrap(), "scratch | /b/x | queue.local | local/main | |\n");
    }

    #[test]
    fn an_existing_file_is_validated_and_never_overwritten() {
        let d = testkit::TempDir::new("spira-config-init-existing");
        let out = d.path().join("spira.toml");
        std::fs::write(&out, "[spira]\nid_prefix = \"zz\"\n").unwrap();
        assert_eq!(ensure(&out, full(), None, &reg()).unwrap(), Outcome::Existing(out.clone()));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "[spira]\nid_prefix = \"zz\"\n");
        std::fs::write(&out, "[spira]\nmax_aeons = true\n").unwrap();
        assert!(ensure(&out, full(), None, &reg()).is_err());
    }

    #[test]
    fn a_layered_spec_in_force_is_validated_and_used_and_one_with_a_missing_layer_refuses() {
        let d = testkit::TempDir::new("spira-config-init-layers");
        let base = d.path().join("base.toml");
        let over = d.path().join("over.toml");
        std::fs::write(&base, "[spira]\nid_prefix = \"zz\"\n").unwrap();
        std::fs::write(&over, "[spira]\ninstance = \"t\"\n").unwrap();
        let spec = PathBuf::from(format!("{}:{}", base.display(), over.display()));
        assert_eq!(ensure(&spec, full(), None, &reg()).unwrap(), Outcome::Existing(spec.clone()));
        assert_eq!(std::fs::read_to_string(&base).unwrap(), "[spira]\nid_prefix = \"zz\"\n", "never written");
        let gone = PathBuf::from(format!("{}:{}", base.display(), d.path().join("nope.toml").display()));
        let e = ensure(&gone, full(), None, &reg()).unwrap_err();
        assert!(e.contains("nope.toml"), "{e}");
    }

    #[test]
    fn nobody_to_ask_refuses_naming_the_missing_inputs_and_writes_nothing() {
        let d = testkit::TempDir::new("spira-config-init-refuse");
        let out = d.path().join("spira.toml");
        let mut a = full();
        a.remove("releases");
        a.remove("instance");
        let e = ensure(&out, a, None, &reg()).unwrap_err();
        assert!(e.contains("instance, releases"), "{e}");
        assert!(!out.exists());
    }

    #[test]
    fn a_prompt_fills_what_the_answers_lack() {
        let d = testkit::TempDir::new("spira-config-init-ask");
        let out = d.path().join("spira.toml");
        let mut a = full();
        a.remove("run");
        let mut ask = |k: &str, _: &str, _: &str| (k == "run").then(|| "/asked/run".to_string());
        ensure(&out, a, Some(&mut ask), &reg()).unwrap();
        assert_eq!(crate::get_path(&crate::load(&out).unwrap(), "spira.run").as_deref(), Some("/asked/run"));
    }

    #[test]
    fn an_unknown_answer_is_refused_by_name() {
        let mut a = full();
        a.insert("no_such_key".into(), "1".into());
        let e = render(&a, Path::new("/x/spira.toml"), &reg()).unwrap_err();
        assert!(e.contains("no_such_key"), "{e}");
    }

    #[test]
    fn default_out_prefers_spira_toml_then_xdg_then_home() {
        let m = |p: &[(&str, &str)]| p.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>();
        assert_eq!(default_out(&m(&[("SPIRA_TOML", "/a/s.toml"), ("HOME", "/h")])).unwrap(), PathBuf::from("/a/s.toml"));
        assert_eq!(default_out(&m(&[("XDG_CONFIG_HOME", "/x"), ("HOME", "/h")])).unwrap(), PathBuf::from("/x/spira/spira.toml"));
        assert_eq!(default_out(&m(&[("HOME", "/h")])).unwrap(), PathBuf::from("/h/.config/spira/spira.toml"));
        assert_eq!(default_out(&m(&[("SPIRA_TOML", "/a:/b")])).unwrap(), PathBuf::from("/a:/b"));
    }
}
