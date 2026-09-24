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

/// Everything that came from the inputs but had nowhere to go in the schema: an unknown
/// `spira.conf` key, an unparseable repo-map row, a fayth field this schema does not carry.
/// Never fatal — matching `spira_conf_read`'s own "report, don't refuse" — but returned so a
/// caller can show what a widened schema would still need to capture.
#[derive(Debug, Default, Clone)]
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

/// Builds `[spira]` from the raw key/value pairs [`read_conf`] produced. A key not in the
/// schema is warned about and dropped, matching `spira_conf_read`'s own tolerance — a
/// converter that hard-fails on the box's own file is worse than the parser it replaces.
pub fn spira_section(
    raw: &BTreeMap<String, String>,
    warnings: &mut ConvertWarnings,
) -> SpiraSection {
    let mut s = SpiraSection::default();
    for (key, val) in raw {
        match key.as_str() {
            "SPIRA_HOME_REPO" => s.home_repo = Some(val.clone()),
            "SPIRA_DB" => s.db = Some(val.clone()),
            "SPIRA_RUN" => s.run = Some(val.clone()),
            "SPIRA_GOAL" => s.goal = Some(val.clone()),
            "SPIRA_PATH" => s.path = Some(val.clone()),
            "SPIRA_WORKSPACES" => s.workspaces = Some(val.clone()),
            "SPIRA_PROD" => s.prod = Some(val.clone()),
            "SPIRA_DOLT_DATA" => s.dolt_data = Some(val.clone()),
            "SPIRA_WIKI" => s.wiki = Some(val.clone()),
            "SPIRA_WIKI_HOOK" => s.wiki_hook = Some(val.clone()),
            "SPIRA_VIEW" => s.view = Some(val.clone()),
            "SPIRA_TZ" => s.tz = Some(val.clone()),
            "SPIRA_OPERATOR" => s.operator = Some(val.clone()),
            "SPIRA_OPERATOR_ACTOR" => s.operator_actor = Some(val.clone()),
            "SPIRA_ASK_LABEL" => s.ask_label = Some(val.clone()),
            "SPIRA_CI_LABEL" => s.ci_label = Some(val.clone()),
            "SPIRA_SCOPE_LABEL" => s.scope_label = Some(val.clone()),
            "SPIRA_ACTIONABLE" => s.actionable = Some(val.clone()),
            "SPIRA_NOTIFY_AGE" => s.notify_age = parse_u64(warnings, "notify_age", val),
            "SPIRA_VERDICT_TTL" => s.verdict_ttl = parse_u64(warnings, "verdict_ttl", val),
            "SPIRA_MAX_AEONS" => s.max_aeons = parse_u32(warnings, "max_aeons", val),
            "SPIRA_MAX_LIVE_AEONS" => s.max_live_aeons = parse_u32(warnings, "max_live_aeons", val),
            "SPIRA_FAYTHS" => s.fayths = val.split_whitespace().map(str::to_string).collect(),
            "SPIRA_BATCH_MAXPAR" => s.batch_maxpar = parse_u32(warnings, "batch_maxpar", val),
            "SPIRA_CERTIFY_PAR" => s.certify_par = parse_u32(warnings, "certify_par", val),
            "SPIRA_CERTIFY_SUITES" => {
                s.certify_suites = match val.as_str() {
                    "on" => Some(OnOff::On),
                    "off" => Some(OnOff::Off),
                    other => {
                        warnings.push(format!("spira.certify_suites: not on/off: {other:?}"));
                        None
                    }
                }
            }
            "SPIRA_CERT_IDLE_SKIP" => {
                s.cert_idle_skip = parse_bool01(warnings, "cert_idle_skip", val)
            }
            "SPIRA_QUEUE_BATCH_MAX" => {
                s.queue_batch_max = parse_u32(warnings, "queue_batch_max", val)
            }
            "SPIRA_QUEUE_BATCH_WAIT" => {
                s.queue_batch_wait = parse_u64(warnings, "queue_batch_wait", val)
            }
            "SPIRA_QUEUE_CI_MAXSEC" => {
                s.queue_ci_maxsec = parse_u64(warnings, "queue_ci_maxsec", val)
            }
            "SPIRA_QUEUE_LOCAL_GATE" => {
                s.queue_local_gate = parse_bool01(warnings, "queue_local_gate", val)
            }
            "SPIRA_QUEUE_THROTTLE_RELEASE_AT" => {
                s.queue_throttle_release_at = parse_u32(warnings, "queue_throttle_release_at", val)
            }
            "SPIRA_SUITES_BUDGET" => s.suites_budget = parse_u64(warnings, "suites_budget", val),
            "SPIRA_LOOM_ADDR" => s.loom_addr = Some(val.clone()),
            "SPIRA_LOOM_BUDGET_MS" => s.loom_budget_ms = parse_u64(warnings, "loom_budget_ms", val),
            "SPIRA_MAIL_READERS" => s.mail_readers = Some(val.clone()),
            "SPIRA_GH_INTAKE_REPO" => s.gh_intake_repo = Some(val.clone()),
            "COCKPIT_BOTTOM_PCT" => {
                s.cockpit_bottom_pct = parse_u32(warnings, "cockpit_bottom_pct", val)
            }
            "COCKPIT_RIGHT_PCT" => {
                s.cockpit_right_pct = parse_u32(warnings, "cockpit_right_pct", val)
            }
            "SPIRA_CZAR_STAGE_DEADLOCK" => {
                s.czar_stage_deadlock = match val.as_str() {
                    "shadow" => Some(crate::CzarStage::Shadow),
                    "act" => Some(crate::CzarStage::Act),
                    other => {
                        warnings.push(format!(
                            "spira.czar_stage_deadlock: not shadow/act: {other:?}"
                        ));
                        None
                    }
                }
            }
            other => warnings.push(format!(
                "spira.conf: {other} has no [spira] field yet (schema covers the keys this \
                 install sets; widen it in the cutover bead if a consumer still needs it)"
            )),
        }
    }
    s
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
    gate: String,
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
    let gate_start = if nf >= 6 {
        6
    } else if nf == 5 {
        5
    } else {
        4
    };
    let gate_end = lanes_idx.map(|li| li - 1).unwrap_or(nf);
    let gate = if gate_start <= gate_end && gate_start <= nf {
        fields[gate_start - 1..gate_end.min(nf)]
            .join("|")
            .trim()
            .to_string()
    } else {
        String::new()
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
        gate,
        lanes_raw,
    })
}

/// Parses `repo-map` text into `[repo.<name>]` tables.
pub fn repo_sections(text: &str, warnings: &mut ConvertWarnings) -> BTreeMap<String, RepoSection> {
    let mut out = BTreeMap::new();
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
            other => {
                warnings.push(format!(
                    "repo-map: {}: unknown land mode {other:?}",
                    row.name
                ));
                continue;
            }
        };
        let lanes = if row.lanes_raw.is_empty() {
            vec![Lane::Plan]
        } else if let Some(expanded) = {
            let e = expand_lane_mode(&row.lanes_raw);
            if e.is_empty() {
                None
            } else {
                Some(e)
            }
        } {
            expanded
        } else {
            let mut lanes = Vec::new();
            for tok in row.lanes_raw.split(',') {
                let tok = tok.trim();
                if tok.is_empty() {
                    continue;
                }
                match parse_lane_token(tok) {
                    Some(l) => lanes.push(l),
                    None => warnings.push(format!("repo-map: {}: unknown lane {tok:?}", row.name)),
                }
            }
            lanes
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
                gate: if row.gate.is_empty() {
                    None
                } else {
                    Some(row.gate)
                },
                lanes,
                forge: None,
            },
        );
    }
    out
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
        ("SPIRA_ASK_LABEL", "needs-operator"),
        ("SPIRA_CI_LABEL", "awaiting-ci"),
        ("SPIRA_SPIKE_LABEL", "spike"),
        ("SPIRA_GROOMER_LABEL", "groom"),
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
/// along the way (an unknown `spira.conf` key, an unparseable repo-map row, ...).
pub fn convert(
    conf_text: &str,
    home: &str,
    repo_map_text: &str,
    fayth_texts: &[(&str, &str)],
) -> (SpiraToml, ConvertWarnings) {
    let mut warnings = ConvertWarnings::default();
    let raw = read_conf(conf_text, home);
    let spira = spira_section(&raw, &mut warnings);
    let repo = repo_sections(repo_map_text, &mut warnings);
    let mut persona = BTreeMap::new();
    for (_path, text) in fayth_texts {
        let (name, section) = persona_section(text, &spira, &mut warnings);
        if name.is_empty() {
            warnings.push("fayth: no FAYTH_NAME, skipped".to_string());
            continue;
        }
        persona.insert(name, section);
    }
    (
        SpiraToml {
            spira: Some(spira),
            repo,
            persona,
        },
        warnings,
    )
}
