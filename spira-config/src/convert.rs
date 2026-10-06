//! The `spira.conf` / `repo-map` / `chamber/*.fayth` → `spira.toml` converter.
//!
//! `spira.conf` is explicitly not shell (conf.sh's own header: "IT IS NOT SOURCED"), so its
//! reader here is a direct port of `spira_conf_read`'s semantics — same trimming, same
//! one-layer quote strip, same `~`/`$HOME` expansion, same "unknown key is reported, not
//! obeyed." `repo-map` is pipe-delimited data; its reader is a port of `lib.sh`'s
//! `repo_field` column heuristic, kept identical so a row this converter reads the same way
//! the shell reader already does.
//!
//! `chamber/*.fayth` files ARE shell — sourced, with values built from `${VAR:+text}`
//! parameter expansion referencing `[spira]` keys like `SPIRA_SCOPE_LABEL`. Reimplementing
//! bash is out of scope; a small, bounded expander for exactly the forms these files use
//! (`${NAME:+repl}` and bare `$NAME`) is not.

use std::collections::BTreeMap;

use crate::{LandMode, Lane, Lease, OnOff, PersonaSection, RepoSection, SpiraSection, SpiraToml};

include!(concat!(env!("OUT_DIR"), "/spira_convert.rs"));

/// Everything that came from the inputs but had nowhere to go in the schema: an unknown
/// `spira.conf` key, a row with an unknown land mode, a fayth field this schema does not
/// carry. Never fatal — matching `spira_conf_read`'s own "report, don't refuse" — but
/// returned so a caller can show what a widened schema would still need to capture. An
/// unrecognized lane token is not among these: see [`convert`]'s `Err`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConvertWarnings(pub Vec<String>);

impl ConvertWarnings {
    fn push(&mut self, w: impl Into<String>) {
        self.0.push(w.into());
    }
}

/// Parses `spira.conf` text into raw `KEY -> value` pairs, applying exactly
/// `spira_conf_read`'s trimming, quote-stripping and `~`/`$HOME` expansion. Does not filter
/// by the allowlist — that happens in [`spira_section`], which is where "unknown key" is
/// reported instead of just "read."
pub fn read_conf(text: &str, home: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        let mut val = val.trim().to_string();
        if (val.starts_with('"') && val.ends_with('"') && val.len() >= 2)
            || (val.starts_with('\'') && val.ends_with('\'') && val.len() >= 2)
        {
            val = val[1..val.len() - 1].to_string();
        }
        if val == "~" || val.starts_with("~/") {
            val = format!("{home}{}", &val[1..]);
        }
        let val = val.replace("$HOME", home).replace("${HOME}", home);
        out.insert(key, val);
    }
    out
}

fn parse_u32(w: &mut ConvertWarnings, key: &str, val: &str) -> Option<u32> {
    val.parse()
        .map_err(|_| w.push(format!("spira.{key}: not an integer: {val:?}")))
        .ok()
}

fn parse_u64(w: &mut ConvertWarnings, key: &str, val: &str) -> Option<u64> {
    val.parse()
        .map_err(|_| w.push(format!("spira.{key}: not an integer: {val:?}")))
        .ok()
}

fn parse_bool01(w: &mut ConvertWarnings, key: &str, val: &str) -> Option<bool> {
    match val {
        "1" => Some(true),
        "0" => Some(false),
        other => {
            w.push(format!("spira.{key}: not 0/1: {other:?}"));
            None
        }
    }
}

/// Builds `[spira]` from the raw key/value pairs [`read_conf`] produced. The `KEY => field`
/// mapping is generated from the config key registry ([`apply_spira_key`]); only the retired
/// keys are written out here. A key neither arm recognises is refused rather than dropped,
/// because silently ignoring a key conf.sh accepts is how ~200 of them vanished (sp-9nljd).
pub fn spira_section(
    raw: &BTreeMap<String, String>,
    warnings: &mut ConvertWarnings,
) -> Result<SpiraSection, Vec<String>> {
    let mut s = SpiraSection::default();
    let mut errors = Vec::new();
    for (key, val) in raw {
        match key.as_str() {
            "SPIRA_GOAL" => warnings.push(format!(
                "spira.conf: {key} is retired (sp-k6m1m: there is no goal bead; set SPIRA_ID_PREFIX) and ignored — remove it"
            )),
            "SPIRA_AEON_CPU_QUOTA" | "SPIRA_LAND_CPU_QUOTA" => warnings.push(format!(
                "spira.conf: {key} is retired (sp-b4oct: no explicit CPU quotas) and ignored — remove it"
            )),
            "SPIRA_QUARANTINE_CLEAN_RUNS" => warnings.push(format!(
                "spira.conf: {key} is retired (sp-op2c2: flaky suites are deleted, not quarantined, so nothing reactivates) and ignored — remove it"
            )),
            "SPIRA_BATCHER_BIN" => {
                if crate::batcher_bin_means_off(val) && !raw.contains_key("SPIRA_BATCHER_ENABLE") {
                    s.batcher_enable = Some("0".into());
                    warnings.push(format!("spira.conf: {key} is retired (sp-gypjk); its non-batcher value is read as batcher_enable = \"0\""));
                } else {
                    warnings.push(format!("spira.conf: {key} is retired (sp-gypjk: tools are invoked by name) and ignored — remove it"));
                }
            }
            "SPIRA_LIFECYCLE_ENFORCE" => match crate::check_lifecycle_switch_env(Some(val)) {
                Ok(_) => warnings.push(format!(
                    "spira.conf: {key} is retired (sp-v62vn: the lifecycle machine is the only mode) and ignored — remove it"
                )),
                Err(e) => errors.push(format!("spira.conf: {e}")),
            },
            "SPIRA_LC_BIN" | "SPIRA_PANEL" | "SPIRA_BROKER_BIN" | "SPIRA_CZAR_PASS_BIN" | "SPIRA_QUEUE_WATCH_BIN"
            | "SPIRA_SUPERVISE_BIN" | "SPIRA_LANDING_PASS_BIN" | "SPIRA_TSD_BIN" | "SPIRA_TSD_LIFECYCLE_EXPORT_BIN"
            | "SPIRA_RECONCILER_BIN" | "SPIRA_TEST_PLAN_BIN" | "SPIRA_RECONCILER_FLOW_BIN" | "SPIRA_LOOM_BIN" => warnings.push(format!(
                "spira.conf: {key} is retired (sp-gypjk: tools are invoked by name) and ignored — remove it"
            )),
            other => {
                if !apply_spira_key(&mut s, other, val, warnings) {
                    errors.push(format!(
                        "spira.conf: unknown key {other}, refused (not in conf.sh's SPIRA_CONF_KEYS \
                         or a typo — declare the key in spira/conf.d)"
                    ));
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(s)
    } else {
        Err(errors)
    }
}

fn is_lane_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == ',' || c == '_' || c == '-')
}

fn expand_lane_mode(token: &str) -> Vec<Lane> {
    use Lane::*;
    match token {
        "consume" => vec![Plan],
        "develop" => vec![Plan, Incident, Groom, Spike],
        "self" => vec![Plan, Incident, Groom, Spike, MaechenSweep, CzarTrigger],
        _ => Vec::new(),
    }
}

fn parse_lane_token(tok: &str) -> Option<Lane> {
    Some(match tok {
        "plan" => Lane::Plan,
        "incident" => Lane::Incident,
        "groom" => Lane::Groom,
        // literal-ok: matches the repo-map lane token this old format actually writes
        "maechen-sweep" => Lane::MaechenSweep,
        "spike" => Lane::Spike,
        "czar-trigger" => Lane::CzarTrigger,
        _ => return None,
    })
}

/// One parsed `repo-map` row, before its `lanes` shorthand is expanded.
struct RepoRow {
    name: String,
    path: String,
    land: String,
    base: String,
    format: String,
    // The gate column is not carried (sp-quu2w: the tree under test owns its gate; the
    // column is read from repo-map itself for a repository that never adopted `gate.steps`).
    lanes_raw: String,
}

/// Splits one non-comment `repo-map` line into its columns, using the same "is the last
/// field a lane list or the tail of the gate command" heuristic as `lib.sh`'s `repo_field`:
/// a lanes column is empty or matches `^[A-Za-z][A-Za-z0-9,_-]*$`; anything else (spaces,
/// `$`, `/`, `&&`, a bare `|` from a shell pipe) is gate, however many `|` splits it cost.
fn parse_repo_row(line: &str) -> Option<RepoRow> {
    let fields: Vec<&str> = line.split('|').collect();
    let nf = fields.len();
    if nf < 2 {
        return None;
    }
    let name = fields[0].trim();
    if name.is_empty() {
        return None;
    }
    let lanes_idx = if nf >= 7 {
        let t = fields[nf - 1].trim();
        if t.is_empty() || is_lane_ident(t) {
            Some(nf)
        } else {
            None
        }
    } else {
        None
    };
    let base = if nf >= 6 { fields[3].trim() } else { "" };
    let format = if nf >= 6 {
        fields[4].trim()
    } else if nf == 5 {
        fields[3].trim()
    } else {
        ""
    };
    let lanes_raw = lanes_idx
        .map(|li| fields[li - 1].trim().to_string())
        .unwrap_or_default();
    Some(RepoRow {
        name: name.to_string(),
        path: fields
            .get(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        land: fields
            .get(2)
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        base: base.to_string(),
        format: format.to_string(),
        lanes_raw,
    })
}

/// Expands one row's raw `lanes` column, refusing an unrecognized mode word or lane label
/// instead of dropping it — the value's own source is where an agent that hallucinated a
/// token gets backpressure, not lib.sh's now-narrowed reading of an already-expanded list
/// (law-fail-closed-at-the-source). Empty means "no restriction": every lane.
fn parse_lanes(row_name: &str, raw: &str) -> Result<Vec<Lane>, String> {
    if raw.is_empty() {
        return Ok(vec![Lane::Plan]);
    }
    let expanded = expand_lane_mode(raw);
    if !expanded.is_empty() {
        return Ok(expanded);
    }
    let mut lanes = Vec::new();
    for tok in raw.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        match parse_lane_token(tok) {
            Some(l) => lanes.push(l),
            None => return Err(format!("repo-map: {row_name}: unknown lane {tok:?}")),
        }
    }
    Ok(lanes)
}

/// Parses `repo-map` text into `[repo.<name>]` tables. `Err` names every row whose `lanes`
/// column held an unrecognized mode word or lane label — collected across all rows, not just
/// the first, so a sweep of a bad map fixes every offender in one pass.
pub fn repo_sections(
    text: &str,
    warnings: &mut ConvertWarnings,
) -> Result<BTreeMap<String, RepoSection>, Vec<String>> {
    let mut out = BTreeMap::new();
    let mut errors = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some(row) = parse_repo_row(line) else {
            continue;
        };
        let mode = match row.land.as_str() {
            "push" => LandMode::Push,
            "pr" => LandMode::Pr,
            "hold" => LandMode::Hold,
            "queue" => LandMode::Queue,
            "queue.forge" => LandMode::QueueForge,
            "queue.local" => LandMode::QueueLocal,
            other => {
                warnings.push(format!(
                    "repo-map: {}: unknown land mode {other:?}",
                    row.name
                ));
                continue;
            }
        };
        let lanes = match parse_lanes(&row.name, &row.lanes_raw) {
            Ok(lanes) => lanes,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        out.insert(
            row.name.clone(),
            RepoSection {
                path: row.path,
                mode,
                base: if row.base.is_empty() {
                    None
                } else {
                    Some(row.base)
                },
                format: if row.format.is_empty() {
                    None
                } else {
                    Some(row.format)
                },
                lanes,
                forge: None,
                gate_mode: None,
            },
        );
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// Expands the small subset of shell parameter expansion the fayth files actually use:
/// `${NAME:+repl}` (repl, itself expanded, if NAME is set and non-empty; empty otherwise)
/// and bare `$NAME`. Not a shell interpreter — anything else passes through unexpanded.
fn shell_expand(input: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() && chars[i + 1] == '{' {
            if let Some(close) = chars[i..].iter().position(|&c| c == '}') {
                let inner: String = chars[i + 2..i + close].iter().collect();
                if let Some((name, repl)) = inner.split_once(":+") {
                    let set = vars.get(name).map(|v| !v.is_empty()).unwrap_or(false);
                    if set {
                        out.push_str(&shell_expand(repl, vars));
                    }
                } else {
                    out.push_str(vars.get(inner.as_str()).map(String::as_str).unwrap_or(""));
                }
                i += close + 1;
                continue;
            }
        }
        if chars[i] == '$' {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j > i + 1 {
                let name: String = chars[i + 1..j].iter().collect();
                out.push_str(vars.get(name.as_str()).map(String::as_str).unwrap_or(""));
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The `SPIRA_*_LABEL` defaults `conf.sh` derives when `[spira]` does not set them —
/// needed to resolve a fayth's `FAYTH_LABELS` expression even when the schema (deliberately
/// narrower than `conf.sh`'s full allowlist) carries only some of the label keys.
fn label_defaults() -> BTreeMap<String, String> {
    [
        // literal-ok: mirrors conf.sh's own derived default (this binary cannot source schema.sh)
        ("SPIRA_ASK_LABEL", "needs-operator"),
        // literal-ok: mirrors conf.sh's own derived default; see above
        ("SPIRA_CI_LABEL", "awaiting-ci"),
        ("SPIRA_SPIKE_LABEL", "spike"),
        ("SPIRA_GROOMER_LABEL", "groom"),
        // literal-ok: mirrors conf.sh's own derived defaults; see above
        ("SPIRA_MAECHEN_LABEL", "maechen-sweep"),
        ("SPIRA_PLAN_LABEL", "plan"),
        ("SPIRA_INCIDENT_LABEL", "incident"),
        ("SPIRA_CZAR_LABEL", "czar-trigger"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Parses one `chamber/*.fayth` file's `FAYTH_KEY=value` lines into `[persona.<name>]`.
/// `spira_vars` is `[spira]`'s own resolved values (e.g. `SPIRA_SCOPE_LABEL`), consulted by
/// [`shell_expand`] before falling back to [`label_defaults`].
pub fn persona_section(
    text: &str,
    spira: &SpiraSection,
    warnings: &mut ConvertWarnings,
) -> (String, PersonaSection) {
    let mut vars = label_defaults();
    if let Some(v) = &spira.scope_label {
        vars.insert("SPIRA_SCOPE_LABEL".to_string(), v.clone());
    }
    if let Some(v) = &spira.ask_label {
        vars.insert("SPIRA_ASK_LABEL".to_string(), v.clone());
    }
    if let Some(v) = &spira.ci_label {
        vars.insert("SPIRA_CI_LABEL".to_string(), v.clone());
    }

    let mut name = String::new();
    let mut model = String::new();
    let mut tools = Vec::new();
    let mut labels_raw = String::new();
    let mut lane = None;
    let mut lease_minutes = None;
    let mut heartbeat_seconds = None;
    let mut system_prompt = None;

    for raw_line in text.lines() {
        let line = raw_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.starts_with("FAYTH_") {
            continue;
        }
        let mut val = val.trim().to_string();
        if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
            val = val[1..val.len() - 1].to_string();
        }
        let val = shell_expand(&val, &vars);
        match key {
            "FAYTH_NAME" => name = val,
            "FAYTH_MODEL" => model = val,
            "FAYTH_TOOLS" => {
                tools = val
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "FAYTH_LABELS" => labels_raw = val,
            "FAYTH_LANE" => lane = Some(val),
            "FAYTH_LEASE_MINUTES" => {
                lease_minutes = val.parse().ok().or_else(|| {
                    warnings.push(format!(
                        "fayth: FAYTH_LEASE_MINUTES not an integer: {val:?}"
                    ));
                    None
                })
            }
            "FAYTH_HEARTBEAT_SECONDS" => {
                heartbeat_seconds = val.parse().ok().or_else(|| {
                    warnings.push(format!(
                        "fayth: FAYTH_HEARTBEAT_SECONDS not an integer: {val:?}"
                    ));
                    None
                })
            }
            "FAYTH_SYSTEM_PROMPT" => {
                system_prompt = match val.as_str() {
                    "append" => Some(crate::SystemPromptMode::Append),
                    "replace" => Some(crate::SystemPromptMode::Replace),
                    other => {
                        warnings.push(format!(
                            "fayth: FAYTH_SYSTEM_PROMPT not append/replace: {other:?}"
                        ));
                        None
                    }
                }
            }
            _ => {}
        }
    }

    let labels = labels_raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let lease = if lease_minutes.is_some() || heartbeat_seconds.is_some() {
        Some(Lease {
            minutes: lease_minutes,
            heartbeat_seconds,
        })
    } else {
        None
    };

    (
        name,
        PersonaSection {
            model,
            tools,
            labels,
            lane,
            lease,
            system_prompt,
        },
    )
}

/// Converts a full set of legacy inputs into one `SpiraToml`, plus every warning collected
/// along the way (an unknown `spira.conf` key, an unparseable repo-map row, ...). `Err` means
/// at least one repo-map row's `lanes` column named an unrecognized mode word or lane label —
/// refused rather than silently narrowed, on this path and every other caller of `convert`.
pub fn convert(
    conf_text: &str,
    home: &str,
    repo_map_text: &str,
    fayth_texts: &[(&str, &str)],
) -> Result<(SpiraToml, ConvertWarnings), Vec<String>> {
    let mut warnings = ConvertWarnings::default();
    let raw = read_conf(conf_text, home);
    let spira = spira_section(&raw, &mut warnings)?;
    let repo = repo_sections(repo_map_text, &mut warnings)?;
    let mut persona = BTreeMap::new();
    for (_path, text) in fayth_texts {
        let (name, section) = persona_section(text, &spira, &mut warnings);
        if name.is_empty() {
            warnings.push("fayth: no FAYTH_NAME, skipped".to_string());
            continue;
        }
        persona.insert(name, section);
    }
    let doc = SpiraToml {
        spira: Some(spira),
        repo,
        persona,
    };
    // A conf without SPIRA_ID_PREFIX still converts: the refusal is `spira-config validate`'s
    // (doctor, pre-activate), the one place the required key is checked (sp-k6m1m).
    Ok((doc, warnings))
}

/// What [`convert_legacy_dir`] found in a config directory.
#[derive(Debug, PartialEq, Eq)]
pub enum LegacyOutcome {
    /// A `spira.toml` already stands there; nothing was read or written.
    Present(std::path::PathBuf),
    /// No `spira.toml` and no `spira.conf`: a fresh install with nothing to convert.
    Nothing,
    /// `spira.conf` (and `repo-map`, when present) were converted into the returned `spira.toml`.
    Converted(std::path::PathBuf, ConvertWarnings),
}

/// Upgrade a pre-cutover config directory in place: when `dir` holds `spira.conf` but no
/// `spira.toml`, write the `spira.toml` that [`convert`] makes from the conf, the `repo-map`
/// beside it and `fayths`. An existing `spira.toml` is never read or overwritten, so a second
/// run converts nothing.
pub fn convert_legacy_dir(
    dir: &std::path::Path,
    home: &str,
    fayths: &[std::path::PathBuf],
) -> Result<LegacyOutcome, String> {
    let toml_path = crate::toml_path_at(dir);
    if toml_path.is_file() {
        return Ok(LegacyOutcome::Present(toml_path));
    }
    let read = |p: &std::path::Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let conf_path = dir.join("spira.conf");
    if !conf_path.is_file() {
        return Ok(LegacyOutcome::Nothing);
    }
    let conf = read(&conf_path)?;
    let map_path = dir.join("repo-map");
    let repo_map = if map_path.is_file() { read(&map_path)? } else { String::new() };
    let mut texts = Vec::new();
    for f in fayths {
        texts.push((f.display().to_string(), read(f)?));
    }
    let refs: Vec<(&str, &str)> = texts.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let (doc, warnings) = convert(&conf, home, &repo_map, &refs).map_err(|e| e.join("; "))?;
    let out = toml::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    crate::write_atomic(&toml_path, &out).map_err(|e| format!("{}: {e}", toml_path.display()))?;
    Ok(LegacyOutcome::Converted(toml_path, warnings))
}
