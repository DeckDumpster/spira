//! `RUN:` and `DEADLINE:` — the tool a watcher may run on an SOP's behalf. The program is a
//! bare name resolved only inside the active release's `bin/`; the arguments may name the
//! incident's fields in braces. Nothing here is shell: the line is split on whitespace.

use std::collections::BTreeMap;

pub const FIELDS: [&str; 3] = ["bead", "batch", "repo"];
pub const MAX_DEADLINE_SECS: u64 = 900;

#[derive(Debug, PartialEq, Eq)]
pub struct RunSpec {
    pub program: String,
    pub args: Vec<String>,
    pub deadline_secs: u64,
}

fn field_value(text: &str, name: &str) -> Option<String> {
    let re = regex::Regex::new(&format!(r"(?m)^\s*{name}:\s*(.+)$")).expect("static");
    re.captures(text).map(|c| c[1].trim().to_string())
}

fn bare_program(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with(['-', '.'])
        && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn arg_faults(arg: &str) -> Option<String> {
    if let Some(c) = arg.chars().find(|c| ";|&$`<>()\\\"'*?~#".contains(*c)) {
        return Some(format!("RUN is not shell; '{c}' in `{arg}` would never be interpreted"));
    }
    let mut rest = arg;
    while let Some(open) = rest.find(['{', '}']) {
        let tail = &rest[open..];
        let named = tail.strip_prefix('{').and_then(|t| t.split_once('}')).map(|(n, _)| n);
        match named {
            Some(n) if FIELDS.contains(&n) => rest = &tail[n.len() + 2..],
            _ => return Some(format!("RUN names an unknown field in `{arg}` (known: {})", FIELDS.join(" "))),
        }
    }
    None
}

/// `Ok(None)` when the SOP declares no `RUN:`; `Err` names the first rule it breaks.
pub fn parse(text: &str) -> Result<Option<RunSpec>, String> {
    let Some(line) = field_value(text, "RUN") else {
        return match field_value(text, "DEADLINE") {
            Some(_) => Err("DEADLINE without RUN".to_string()),
            None => Ok(None),
        };
    };
    if crate::validate::match_line(text).is_none() {
        return Err("RUN needs a MATCH: line — a key-token match is too weak to run a tool on".to_string());
    }
    let mut words = line.split_whitespace();
    let program = words.next().unwrap_or_default();
    if !bare_program(program) {
        return Err(format!("RUN must name a bare program from the release's bin/, not `{program}`"));
    }
    let args: Vec<String> = words.map(str::to_string).collect();
    if let Some(f) = args.iter().find_map(|a| arg_faults(a)) {
        return Err(f);
    }
    let Some(d) = field_value(text, "DEADLINE") else {
        return Err("RUN needs a DEADLINE: line — whole seconds the tool may run".to_string());
    };
    match d.parse::<u64>() {
        Ok(n) if (1..=MAX_DEADLINE_SECS).contains(&n) => Ok(Some(RunSpec { program: program.to_string(), args, deadline_secs: n })),
        _ => Err(format!("DEADLINE must be whole seconds, 1 to {MAX_DEADLINE_SECS}; got: {d}")),
    }
}

impl RunSpec {
    /// The arguments with each `{field}` replaced; a field the incident does not carry is an error.
    pub fn argv(&self, fields: &BTreeMap<String, String>) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        for a in &self.args {
            let mut s = a.clone();
            for f in FIELDS {
                let token = format!("{{{f}}}");
                if s.contains(&token) {
                    let v = fields.get(f).ok_or_else(|| format!("the incident carries no `{f}` for RUN to use"))?;
                    s = s.replace(&token, v);
                }
            }
            out.push(s);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sop(extra: &str) -> String {
        format!("MATCH: x\nSYMPTOM: s\nCHECK: c\nFIX: f\n{extra}\n")
    }

    #[test]
    fn a_sop_without_run_declares_no_tool() {
        assert_eq!(parse(&sop("")), Ok(None));
    }

    #[test]
    fn a_bare_program_with_fields_and_a_deadline_parses() {
        let s = parse(&sop("RUN: round-recert --batch {batch} --repo {repo}\nDEADLINE: 120")).unwrap().unwrap();
        assert_eq!(s.program, "round-recert");
        assert_eq!(s.deadline_secs, 120);
        let f: BTreeMap<String, String> = [("batch".into(), "b7".into()), ("repo".into(), "r".into())].into();
        assert_eq!(s.argv(&f).unwrap(), vec!["--batch", "b7", "--repo", "r"]);
        assert!(s.argv(&BTreeMap::new()).unwrap_err().contains("batch"));
    }

    #[test]
    fn a_program_outside_the_release_bin_is_refused() {
        for bad in ["/usr/bin/true", "../bin/x", "./x", "bin/x", "-x", "x;y"] {
            let e = parse(&sop(&format!("RUN: {bad}\nDEADLINE: 5"))).unwrap_err();
            assert!(e.contains("bare program"), "{bad}: {e}");
        }
    }

    #[test]
    fn a_run_with_no_deadline_or_a_bad_one_is_refused() {
        assert!(parse(&sop("RUN: tool")).unwrap_err().contains("DEADLINE"));
        for bad in ["0", "soon", "901", "-3"] {
            assert!(parse(&sop(&format!("RUN: tool\nDEADLINE: {bad}"))).unwrap_err().contains("DEADLINE"), "{bad}");
        }
    }

    #[test]
    fn shell_syntax_and_unknown_fields_in_arguments_are_refused() {
        assert!(parse(&sop("RUN: tool a;b\nDEADLINE: 5")).unwrap_err().contains("not shell"));
        assert!(parse(&sop("RUN: tool {nope}\nDEADLINE: 5")).unwrap_err().contains("unknown field"));
        assert!(parse(&sop("RUN: tool {bead\nDEADLINE: 5")).unwrap_err().contains("unknown field"));
    }

    #[test]
    fn a_run_without_a_match_line_is_refused() {
        assert!(parse("SYMPTOM: s\nCHECK: c\nFIX: f\nRUN: tool\nDEADLINE: 5\n").unwrap_err().contains("MATCH"));
    }

    #[test]
    fn a_deadline_without_a_run_is_refused() {
        assert!(parse(&sop("DEADLINE: 5")).unwrap_err().contains("without RUN"));
    }
}
