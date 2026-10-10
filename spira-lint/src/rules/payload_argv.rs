//! `payload-argv-lint` — no JSON payload handed to python3/jq/awk through argv or the
//! environment (law-payloads-go-on-stdin). Contract: DESIGN.md.

use regex::bytes::{Regex, RegexBuilder};

use crate::lex::shell::{self, Command, Word};
use crate::{allow_lines, lines, pathspec_match, scope, trim_lead, Entry, Finding, LintError, Rule, Tree};

pub struct PayloadArgv;

const NAME: &str = "payload-argv-lint";
const ALLOW_FILE: &str = "spira/payload-argv-lint-allow";
const TARGETS: &[&str] = &["python3", "python", "jq", "awk", "gawk", "mawk"];

/// `spira/payload-argv-lint-allow`: one regex per line, matched case-insensitively against
/// `<path>:<line>: <source line, leading whitespace stripped>`.
pub struct PayloadAllow(Vec<Regex>);

impl PayloadAllow {
    pub fn parse(text: &str) -> Result<PayloadAllow, LintError> {
        let mut v = Vec::new();
        let mut n = 0;
        for (i, raw) in text.lines().enumerate() {
            let t = raw.trim_start();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            n += 1;
            let re = RegexBuilder::new(raw).case_insensitive(true).build().map_err(|e| LintError::BadAllow {
                file: ALLOW_FILE.to_string(),
                line: i + 1,
                reason: format!("not a regex: {e}"),
            })?;
            v.push(re);
        }
        debug_assert_eq!(n, allow_lines(text).len());
        Ok(PayloadAllow(v))
    }
    pub fn covers(&self, subject: &[u8]) -> bool {
        self.0.iter().any(|r| r.is_match(subject))
    }
}

fn json_named(name: &str) -> bool {
    name.to_ascii_lowercase().contains("json")
}

fn json_vars(w: &Word) -> Vec<&str> {
    w.vars.iter().map(String::as_str).filter(|v| json_named(v)).collect()
}

/// One offending command: (kind, line of its first offending word, the json vars, the
/// command's name).
#[derive(Debug, PartialEq, Eq)]
pub struct Hit {
    pub kind: &'static str,
    pub line: usize,
    pub vars: Vec<String>,
    pub cmd: String,
}

/// Resolve the environment and argv of `cmd`, unwrapping env/command/exec/nohup. Shared
/// with `script-callers` (sp-9y0gf), which needs the same unwrap to find the word a
/// suite actually executes under an `env -i …` prefix.
pub(crate) fn resolve(cmd: &Command) -> (Vec<&Word>, Vec<&Word>) {
    let (env, argv) = cmd.split_env();
    let mut env: Vec<&Word> = env.iter().collect();
    let mut argv: Vec<&Word> = argv.iter().collect();
    loop {
        let Some(head) = argv.first() else { break };
        match head.unquoted().as_str() {
            "env" => {
                let mut i = 1;
                while i < argv.len() {
                    let a = argv[i].unquoted();
                    if a == "-u" || a == "--unset" {
                        i += 2;
                    } else if a.starts_with('-') {
                        i += 1;
                    } else if argv[i].assignment_name().is_some() {
                        env.push(argv[i]);
                        i += 1;
                    } else {
                        break;
                    }
                }
                argv.drain(..i.min(argv.len()));
            }
            "command" | "exec" | "nohup" => {
                let mut i = 1;
                while i < argv.len() && argv[i].unquoted().starts_with('-') {
                    i += 1;
                }
                argv.drain(..i);
            }
            _ => break,
        }
    }
    (env, argv)
}

pub fn check_command(cmd: &Command) -> Vec<Hit> {
    let (env, argv) = resolve(cmd);
    let Some(head) = argv.first() else { return Vec::new() };
    let name = head.unquoted();
    let base = name.rsplit('/').next().unwrap_or("");
    if !TARGETS.contains(&base) {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let mut collect = |kind: &'static str, words: &mut dyn Iterator<Item = (&Word, Vec<&str>)>| {
        let mut line = None;
        let mut vars: Vec<String> = Vec::new();
        for (w, vs) in words {
            if vs.is_empty() {
                continue;
            }
            line.get_or_insert(w.line);
            for v in vs {
                if !vars.iter().any(|x| x == v) {
                    vars.push(v.to_string());
                }
            }
        }
        if let Some(line) = line {
            hits.push(Hit { kind, line, vars, cmd: base.to_string() });
        }
    };
    collect(
        "env",
        &mut env.iter().map(|w| {
            let n = w.assignment_name().unwrap_or("");
            let mut vs = json_vars(w);
            if json_named(n) && !vs.contains(&n) {
                vs.insert(0, n);
            }
            (*w, vs)
        }),
    );
    collect("argv", &mut argv[1..].iter().map(|w| (*w, json_vars(w))));
    hits
}

/// Every hit in one shell source, in line order.
pub fn scan(content: &[u8]) -> Vec<Hit> {
    let mut hits: Vec<Hit> = shell::parse(content).iter().flat_map(check_command).collect();
    hits.sort_by_key(|h| h.line);
    hits
}

impl Rule for PayloadArgv {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.tracked && pathspec_match("spira/*.sh", &e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = scope(tree, self)?;
        let allow = PayloadAllow::parse(&tree.read_text(ALLOW_FILE))?;
        let mut out = Vec::new();
        for e in files {
            let Some(content) = tree.content(e) else { continue };
            let src_lines = lines(content);
            for h in scan(content) {
                let text = src_lines.get(h.line - 1).map(|l| trim_lead(l)).unwrap_or_default();
                if allow.covers(format!("{}:{}: {}", e.path, h.line, text).as_bytes()) {
                    continue;
                }
                let vars: Vec<String> = h.vars.iter().map(|v| format!("${v}")).collect();
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(h.line),
                    message: format!("{}: {} handed to {}", h.kind, vars.join(", "), h.cmd),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A JSON-named payload handed to python3/jq/awk through argv or the environment crosses \
MAX_ARG_STRLEN (128 KiB) silently: the exec dies, the caller reads empty output, and empty \
reads as \"nothing to do\" (law-payloads-go-on-stdin). Pass it on stdin, or write it to a \
temp file and pass the PATH. A genuine exception belongs in spira/payload-argv-lint-allow."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn kinds(src: &str) -> Vec<(&'static str, usize, Vec<String>)> {
        scan(src.as_bytes()).into_iter().map(|h| (h.kind, h.line, h.vars)).collect()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    // ── the bash suite's cases ────────────────────────────────────────────────────────

    #[test]
    fn positive_control_argv_and_env_shapes() {
        let src = r#"#!/usr/bin/env bash
epic_rank_rows() {
    local ready_json="$1" lookup_json="$2"
    python3 -c '
import json, sys
ready = json.loads(sys.argv[1])
print(ready)
' "$ready_json" "$lookup_json"
}
mark_queue_waiters() {
    LABELED_JSON="${labeled_json:-[]}" python3 -c '
import os
print(os.environ["LABELED_JSON"])
'
}
"#;
        assert_eq!(
            kinds(src),
            vec![("argv", 8, s(&["ready_json", "lookup_json"])), ("env", 11, s(&["LABELED_JSON", "labeled_json"]))]
        );
    }

    #[test]
    fn the_fix_is_silent() {
        let src = r#"#!/usr/bin/env bash
epic_rank_rows() {
    local ready_json="$1" lookup_json="$2"
    local _lkf; _lkf="$(mktemp)"
    printf '%s' "$lookup_json" > "$_lkf"
    printf '%s' "$ready_json" | LOOKUP_FILE="$_lkf" python3 -c '
import json, os, sys
ready = json.loads(sys.stdin.read())
with open(os.environ["LOOKUP_FILE"]) as f:
    lookup = json.load(f)
print(ready, lookup)
' "$resume_csv"
    rm -f "$_lkf"
}
report() {
    # A printf argument list that merely NAMES python3 and a *_json var elsewhere on the
    # line, with no inline script of its own, must not be mistaken for an invocation.
    printf 'fmt: %s %s\n' \
        "$(some_helper "$x")" \
        "$uc_json"
}
"#;
        assert_eq!(kinds(src), vec![]);
    }

    #[test]
    fn a_bounded_non_json_argv_token_is_silent() {
        let src = "epic_parent_lookup() {\n    python3 -c '\nimport sys\nprint(sys.argv[1])\n' \"$started_csv\"\n}\n";
        assert_eq!(kinds(src), vec![]);
    }

    // ── where the bash heuristic was wrong ────────────────────────────────────────────

    #[test]
    fn misses_of_the_bash_regex_are_caught() {
        assert_eq!(kinds("X_JSON=\"$(bd list)\" python3 -c 'p'\n"), vec![("env", 1, s(&["X_JSON"]))]);
        assert_eq!(kinds("DATA=\"$x_json\" python3 -c 'p'\n"), vec![("env", 1, s(&["x_json"]))]);
        assert_eq!(kinds("env -u HOME X_JSON=1 jq -n .\n"), vec![("env", 1, s(&["X_JSON"]))]);
        assert_eq!(kinds("jq --argjson x \"$y_json\" '.'\n"), vec![("argv", 1, s(&["y_json"]))]);
        assert_eq!(kinds("awk -v x=\"$y_json\" '{print x}' f\n"), vec![("argv", 1, s(&["y_json"]))]);
        assert_eq!(kinds("jq . $bare_json\n"), vec![("argv", 1, s(&["bare_json"]))]);
        assert_eq!(kinds("out=\"$(jq -r .a \"${rows_json}\")\"\n"), vec![("argv", 1, s(&["rows_json"]))]);
        assert_eq!(kinds("command /usr/bin/python3 - \"$x_json\" <f\n"), vec![("argv", 1, s(&["x_json"]))]);
    }

    #[test]
    fn false_positives_of_the_bash_regex_are_silent() {
        assert_eq!(kinds("local a_json=\"$1\"; python3 -c 'print(1)'\n"), vec![], "not python3's environment");
        assert_eq!(kinds("A_JSON=\"x\" some_cmd | jq .\n"), vec![], "the prefix belongs to some_cmd");
        assert_eq!(kinds("ready_json=\"$(SPIRA_SCOPE_LABEL=x python3 -c 'p')\"\n"), vec![], "an outer capture");
        assert_eq!(kinds("jq . <<< \"$x_json\"\n"), vec![], "a here-string is stdin");
        assert_eq!(kinds("python3 - \"$(printf '%s' \"$x_json\" | wc -c)\"\n"), vec![], "nested, and not python3's");
        assert_eq!(kinds("cat <<EOF\nX_JSON=\"$a\" python3 -c 'p'\nEOF\n"), vec![], "a heredoc body is data");
        assert_eq!(kinds("# python3 -c 'p' \"$a_json\"\n"), vec![], "a comment");
    }

    // ── the tree walk, allow list and scope ───────────────────────────────────────────

    #[test]
    fn walk_allow_and_scope() {
        let t = TempDir::new("pal");
        t.write("spira/a.sh", "#!/bin/sh\n  jq . \"$a_json\"\n");
        t.write("spira/sub/b.sh", "jq . \"$b_json\"\n");
        t.write("other/c.sh", "jq . \"$c_json\"\n");
        let tracked = ["spira/a.sh", "spira/sub/b.sh", "other/c.sh"];
        let check = || {
            let tree = Tree::from_paths(t.path(), tracked, ["spira/untracked.sh"]);
            PayloadArgv.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(
            check().unwrap(),
            vec![
                "payload-argv-lint: spira/a.sh:2: argv: $a_json handed to jq",
                "payload-argv-lint: spira/sub/b.sh:1: argv: $b_json handed to jq",
            ]
        );
        t.write(ALLOW_FILE, "# comment\nSPIRA/A\\.SH:2: jq\n");
        assert_eq!(check().unwrap().len(), 1, "allow regex is case-insensitive over path:line: text");
        t.write(ALLOW_FILE, "(unclosed\n");
        assert!(matches!(check(), Err(LintError::BadAllow { line: 1, .. })));

        let empty = Tree::from_paths(t.path(), ["other/c.sh"], Vec::<String>::new());
        assert_eq!(PayloadArgv.check(&empty), Err(LintError::EmptyScope));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(PayloadArgv),
    ]
}
