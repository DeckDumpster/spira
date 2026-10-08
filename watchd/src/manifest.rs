//! The watcher manifest: `name|kind|target[|health]` rows, `@KEY@` placeholder expansion,
//! and the rule that one malformed line refuses the whole file (and every overlay file
//! beside it) rather than silently dropping it. Ported from `watchd.sh` `_wd_parse_file` /
//! `_wd_expand` / `watchd_rows`. Pure — no filesystem here; `context.rs` reads the files.

use std::collections::HashSet;
use std::fmt;

/// The allowlisted `@KEY@` placeholders a target or health command may name
/// (`WATCHD_KEYS` in the bash). An unknown placeholder refuses the manifest; this list is
/// answered by `watchd keys`, never scraped out of source, so a test can enumerate it.
pub const KEYS: &[&str] = &[
    "SPIRA_HOME",
    "SPIRA_REPO",
    "SPIRA_RUN",
    "SPIRA_COCKPIT",
    "SPIRA_DB",
    "SPIRA_WORKSPACES",
    "SPIRA_TOWN",
    "SPIRA_WIKI",
    "SPIRA_VIEW",
    "SPIRA_VIEW_SESSION",
    "SPIRA_CONCIERGE_INBOX",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Daemon,
    Log,
    Extern,
    /// Not a row the manifest named — an optional row whose key this installation has not
    /// set. `target` for an `Off` row is `@<the empty key>@`, not the row's original target
    /// (DESIGN.md: "the target becomes literally the one empty key name").
    Off,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        match s {
            "daemon" => Some(Kind::Daemon),
            "log" => Some(Kind::Log),
            "extern" => Some(Kind::Extern),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Daemon => "daemon",
            Kind::Log => "log",
            Kind::Extern => "extern",
            Kind::Off => "off",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub kind: Kind,
    pub target: String,
    pub health: String,
}

fn trim(s: &str) -> &str {
    s.trim()
}

/// Bash's `IFS='|' read -r -a f <<< "$line"` drops exactly one trailing empty field (a
/// trailing `|` is a terminator, not a separator introducing an empty last field) — verified
/// against bash 5: `a|b|` splits to 2 fields, `a|b||` to 3. Anything before the last field is
/// kept exactly, including empty ones in the middle (`a||c` splits to 3).
fn split_fields(line: &str) -> Vec<&str> {
    let mut f: Vec<&str> = line.split('|').collect();
    if matches!(f.last(), Some(s) if s.is_empty()) {
        f.pop();
    }
    f
}

/// Resolves one `@KEY@` reference. `Ok(None)` is the optional-and-unset case ("empty");
/// `Err` names an unknown placeholder.
pub trait Resolver {
    /// `None` means the key is a real placeholder with nothing set for it ("empty" in the
    /// bash — `${!key-}` unset or empty). `Some(v)` is its value, including `Some("")`
    /// only if the caller's environment legitimately set it to empty (never happens for the
    /// ten keys above in practice, but the type does not assume it).
    fn get(&self, key: &str) -> Option<String>;
}

pub struct MapResolver<'a>(pub &'a std::collections::HashMap<String, String>);
impl Resolver for MapResolver<'_> {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).filter(|v| !v.is_empty()).cloned()
    }
}

enum ExpandErr {
    /// The key named is a real placeholder but nothing is set for it — an optional row
    /// becomes `off`, naming this key; a required row is refused, naming it in the message.
    Empty(String),
    /// Not one of `KEYS` at all — always refused, optional row or not.
    Unknown(String),
    /// More than 20 placeholders deep — a value containing its own `@KEY@` would spin
    /// forever otherwise.
    TooDeep,
}

impl ExpandErr {
    fn message(&self) -> String {
        match self {
            ExpandErr::Empty(k) => format!("@{k}@ is empty — set it in spira.conf or drop the row"),
            ExpandErr::Unknown(k) => format!("unknown placeholder @{k}@ (known: {})", KEYS.join(" ")),
            ExpandErr::TooDeep => "placeholders nest more than 20 deep".to_string(),
        }
    }
}

/// Bash's placeholder regex is `@([A-Z_]+)@` — letters and underscore only — matched
/// leftmost, same as `[[ "$s" =~ @([A-Z_]+)@ ]]`.
fn placeholder_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"@([A-Z_]+)@").unwrap())
}

fn expand(s: &str, resolver: &dyn Resolver) -> Result<String, ExpandErr> {
    let mut out = s.to_string();
    for _ in 0..20 {
        let Some(m) = placeholder_re().captures(&out) else { return Ok(out) };
        let whole = m.get(0).unwrap().as_str().to_string();
        let key = m.get(1).unwrap().as_str().to_string();
        if !KEYS.contains(&key.as_str()) {
            return Err(ExpandErr::Unknown(key));
        }
        match resolver.get(&key) {
            None => return Err(ExpandErr::Empty(key)),
            Some(val) => {
                out = out.replacen(&whole, &val, 1);
            }
        }
    }
    Err(ExpandErr::TooDeep)
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Parses one manifest file's text into `rows`, `seen` and `faults` — the caller's
/// accumulators, because an overlay file and the harness's own file are one manifest for
/// every purpose downstream (`watchd.sh`'s dynamic-scope `_wd_parse_file`).
pub fn parse_file(
    file_label: &str,
    text: &str,
    resolver: &dyn Resolver,
    rows: &mut Vec<Row>,
    seen: &mut HashSet<String>,
    faults: &mut Vec<String>,
) {
    for (idx, raw_line) in text.lines().enumerate() {
        let n = idx + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let trimmed = trim(line);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let f = split_fields(trimmed);
        if f.len() < 3 {
            faults.push(format!(
                "{file_label}:{n}: expected name|kind|target[|health], got {} field(s): {trimmed}",
                f.len()
            ));
            continue;
        }
        let mut name = trim(f[0]).to_string();
        let kind_s = trim(f[1]);
        let target_in = trim(f[2]).to_string();
        let optional = name.starts_with('?');
        if optional {
            name = name[1..].to_string();
        }
        let health_in = if f.len() > 3 { trim(&f[3..].join("|")).to_string() } else { String::new() };

        if !valid_name(&name) {
            faults.push(format!(
                "{file_label}:{n}: '{name}' is not a usable watcher name (letters, digits, _ and - only)"
            ));
            continue;
        }
        if seen.contains(&name) {
            faults.push(format!(
                "{file_label}:{n}: '{name}' is already defined above — a duplicate would render one unit for two rows"
            ));
            continue;
        }
        let Some(kind) = Kind::parse(kind_s) else {
            faults.push(format!(
                "{file_label}:{n}: '{kind_s}' is not a kind (daemon: we run it; log: something else writes it; extern: an existing unit we monitor)"
            ));
            continue;
        };
        if target_in.is_empty() {
            faults.push(format!("{file_label}:{n}: '{name}' has no target"));
            continue;
        }

        let target = match expand(&target_in, resolver) {
            Ok(t) => t,
            Err(ExpandErr::Empty(k)) if optional => {
                seen.insert(name.clone());
                rows.push(Row { name, kind: Kind::Off, target: format!("@{k}@"), health: String::new() });
                continue;
            }
            Err(e) => {
                faults.push(format!("{file_label}:{n}: '{name}': {}", e.message()));
                continue;
            }
        };

        match kind {
            Kind::Extern => {}
            Kind::Daemon => {
                let first = target.split(' ').next().unwrap_or("");
                if !first.starts_with('/') && first.contains('/') {
                    faults.push(format!(
                        "{file_label}:{n}: '{name}': a daemon's program is a bare name or an absolute path, got '{target}'"
                    ));
                    continue;
                }
            }
            _ => {
                if !target.starts_with('/') {
                    faults.push(format!(
                        "{file_label}:{n}: '{name}': target must be an absolute path, got '{target}'"
                    ));
                    continue;
                }
            }
        }

        let health = if health_in.is_empty() {
            String::new()
        } else {
            match expand(&health_in, resolver) {
                Ok(h) => h,
                Err(ExpandErr::Empty(k)) if optional => {
                    seen.insert(name.clone());
                    rows.push(Row { name, kind: Kind::Off, target: format!("@{k}@"), health: String::new() });
                    continue;
                }
                Err(e) => {
                    faults.push(format!("{file_label}:{n}: '{name}' health: {}", e.message()));
                    continue;
                }
            }
        };

        seen.insert(name.clone());
        rows.push(Row { name, kind, target, health });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn resolver(pairs: &[(&str, &str)]) -> MapResolver<'static> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        MapResolver(Box::leak(Box::new(m)))
    }

    fn parse(text: &str, r: &dyn Resolver) -> (Vec<Row>, Vec<String>) {
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        let mut faults = Vec::new();
        parse_file("m", text, r, &mut rows, &mut seen, &mut faults);
        (rows, faults)
    }

    #[test]
    fn a_well_formed_daemon_row_parses() {
        let r = resolver(&[]);
        let (rows, faults) = parse("pool|daemon|pool.sh watch|pool.sh health\n", &r);
        assert!(faults.is_empty());
        assert_eq!(rows, vec![Row { name: "pool".into(), kind: Kind::Daemon, target: "pool.sh watch".into(), health: "pool.sh health".into() }]);
    }

    #[test]
    fn blank_lines_and_comments_are_skipped() {
        let r = resolver(&[]);
        let (rows, faults) = parse("\n  \n# a comment\npool|daemon|pool.sh\n", &r);
        assert!(faults.is_empty());
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn fewer_than_three_fields_is_a_fault() {
        let r = resolver(&[]);
        let (rows, faults) = parse("pool|daemon\n", &r);
        assert!(rows.is_empty());
        assert_eq!(faults, vec!["m:1: expected name|kind|target[|health], got 2 field(s): pool|daemon"]);
    }

    #[test]
    fn an_unusable_name_is_a_fault() {
        let r = resolver(&[]);
        let (_, faults) = parse("po ol|daemon|pool.sh\n", &r);
        assert_eq!(faults.len(), 1);
        assert!(faults[0].contains("not a usable watcher name"));
    }

    #[test]
    fn a_duplicate_name_is_a_fault_and_the_first_row_stands() {
        let r = resolver(&[]);
        let (rows, faults) = parse("pool|daemon|a.sh\npool|daemon|b.sh\n", &r);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target, "a.sh");
        assert_eq!(faults.len(), 1);
        assert!(faults[0].contains("already defined above"));
    }

    #[test]
    fn an_unknown_kind_is_a_fault() {
        let r = resolver(&[]);
        let (_, faults) = parse("pool|process|pool.sh\n", &r);
        assert_eq!(faults.len(), 1);
        assert!(faults[0].contains("not a kind"));
    }

    #[test]
    fn an_empty_target_is_a_fault() {
        // A trailing `|` with nothing after it is dropped as a field the way bash's own
        // `read -a` drops it (`split_fields`'s own doc comment) — `pool|daemon|` is 2
        // fields, not 3 with an empty third, and is caught by the field-count fault
        // instead. A third field that is present but blank (here, a single space that
        // trims to empty) is what exercises "has no target".
        let r = resolver(&[]);
        let (_, faults) = parse("pool|daemon| |\n", &r);
        assert_eq!(faults.len(), 1);
        assert!(faults[0].contains("has no target"), "{:?}", faults);
    }

    #[test]
    fn a_trailing_bare_pipe_with_nothing_after_it_is_a_field_count_fault_not_an_empty_target() {
        let r = resolver(&[]);
        let (_, faults) = parse("pool|daemon|\n", &r);
        assert_eq!(faults.len(), 1);
        assert!(faults[0].contains("got 2 field(s)"), "{:?}", faults);
    }

    #[test]
    fn a_daemon_target_may_not_be_a_relative_path_with_a_slash() {
        let r = resolver(&[]);
        let (_, faults) = parse("pool|daemon|sub/pool.sh\n", &r);
        assert_eq!(faults.len(), 1);
        assert!(faults[0].contains("bare name or an absolute path"));
    }

    #[test]
    fn a_daemon_target_may_be_a_bare_name_or_absolute() {
        let r = resolver(&[]);
        let (rows, faults) = parse("a|daemon|pool.sh\nb|daemon|/usr/bin/pool.sh\n", &r);
        assert!(faults.is_empty());
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn a_log_target_must_be_absolute() {
        let r = resolver(&[]);
        let (_, faults) = parse("a|log|relative.log\n", &r);
        assert_eq!(faults.len(), 1);
        assert!(faults[0].contains("must be an absolute path"));
    }

    #[test]
    fn an_unknown_placeholder_refuses_even_an_optional_row() {
        let r = resolver(&[]);
        let (rows, faults) = parse("?a|daemon|@NOT_A_KEY@ watch\n", &r);
        assert!(rows.is_empty());
        assert_eq!(faults.len(), 1);
        assert!(faults[0].contains("unknown placeholder"));
    }

    #[test]
    fn a_required_row_with_an_unset_key_is_a_fault() {
        let r = resolver(&[]);
        let (rows, faults) = parse("a|daemon|@SPIRA_VIEW@ watch\n", &r);
        assert!(rows.is_empty());
        assert_eq!(faults.len(), 1);
        assert!(faults[0].contains("@SPIRA_VIEW@ is empty"));
    }

    #[test]
    fn an_optional_row_with_an_unset_key_becomes_off_naming_that_key() {
        let r = resolver(&[]);
        let (rows, faults) = parse("?view|daemon|@SPIRA_VIEW@ watch|watchd health-view @SPIRA_VIEW@ @SPIRA_VIEW_SESSION@\n", &r);
        assert!(faults.is_empty());
        assert_eq!(rows, vec![Row { name: "view".into(), kind: Kind::Off, target: "@SPIRA_VIEW@".into(), health: String::new() }]);
    }

    #[test]
    fn an_optional_row_whose_health_key_is_unset_becomes_off() {
        let r = resolver(&[("SPIRA_VIEW", "/usr/bin/view")]);
        let (rows, faults) = parse("?view|daemon|@SPIRA_VIEW@ watch|watchd health-view @SPIRA_VIEW@ @SPIRA_VIEW_SESSION@\n", &r);
        assert!(faults.is_empty());
        assert_eq!(rows[0].kind, Kind::Off);
        assert_eq!(rows[0].target, "@SPIRA_VIEW_SESSION@");
    }

    #[test]
    fn placeholders_expand_when_set() {
        let r = resolver(&[("SPIRA_VIEW", "/usr/bin/view"), ("SPIRA_VIEW_SESSION", "ops")]);
        let (rows, faults) = parse("?view|daemon|@SPIRA_VIEW@ watch|watchd health-view @SPIRA_VIEW@ @SPIRA_VIEW_SESSION@\n", &r);
        assert!(faults.is_empty());
        assert_eq!(rows[0].kind, Kind::Daemon);
        assert_eq!(rows[0].target, "/usr/bin/view watch");
        assert_eq!(rows[0].health, "watchd health-view /usr/bin/view ops");
    }

    #[test]
    fn one_bad_line_does_not_stop_the_rest_of_the_file_from_being_checked() {
        let r = resolver(&[]);
        let (rows, faults) = parse("bad line with no pipes\ngood|daemon|good.sh\n", &r);
        // the caller refuses the WHOLE manifest on any fault; parsing itself still reports
        // every fault and every good row, which is what the caller's refusal message is
        // built from.
        assert_eq!(rows.len(), 1);
        assert_eq!(faults.len(), 1);
    }

    #[test]
    fn health_may_itself_contain_pipes_and_is_rejoined() {
        let r = resolver(&[]);
        let (rows, faults) = parse("a|daemon|a.sh|a.sh check | grep -v ok\n", &r);
        assert!(faults.is_empty());
        assert_eq!(rows[0].health, "a.sh check | grep -v ok");
    }

    #[test]
    fn keys_list_matches_the_eleven_placeholders() {
        assert_eq!(KEYS.len(), 11);
        assert!(KEYS.contains(&"SPIRA_VIEW_SESSION"));
    }
}
