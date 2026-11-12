//! The registry of doors: resources that have a sanctioned tool, and the recognition of a
//! command segment that touches one by what it names rather than by its verb.

use serde::Deserialize;

const WRITERS: &[&str] = &["rm", "mv", "cp", "touch", "tee", "truncate", "ln", "install", "dd", "unlink", "shred"];
const UNIT_VERBS: &[&str] = &["start", "stop", "restart", "reload", "kill", "reset-failed", "disable", "enable", "mask", "unmask", "try-restart", "reload-or-restart", "set-property", "edit"];
const GIT_VALUE_OPTS: &[&str] = &["-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path"];

#[derive(Debug, Deserialize)]
pub struct Registry {
    pub resource: Vec<Resource>,
}

#[derive(Debug, Deserialize)]
pub struct Resource {
    pub name: String,
    pub what: String,
    pub writes_only: bool,
    pub door: String,
    #[serde(default)]
    pub door_commands: Vec<String>,
    pub usage: String,
    #[serde(default)]
    pub usage_in: Option<String>,
    pub why: String,
    pub touched_by: TouchedBy,
}

#[derive(Debug, Default, Deserialize)]
pub struct TouchedBy {
    #[serde(default)]
    pub verbs: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(default)]
    pub units: Vec<String>,
    #[serde(default)]
    pub binaries: Vec<String>,
    #[serde(default)]
    pub tracers: Vec<String>,
    #[serde(default)]
    pub traced: Vec<String>,
}

#[derive(Debug)]
pub enum Contact<'a> {
    Touches(&'a Resource),
    UsesDoor(&'a Resource),
}

pub fn parse(text: &str) -> Result<Registry, String> {
    toml::from_str(text).map_err(|e| format!("doors registry: {e}"))
}

impl Resource {
    pub fn pointer(&self) -> String {
        format!("This touches {}. Its tool is {}: {}. {}", self.name, self.door, self.usage, self.why)
    }

    fn door_words(&self) -> Vec<&str> {
        if self.door_commands.is_empty() {
            vec![self.door.as_str()]
        } else {
            self.door_commands.iter().map(String::as_str).collect()
        }
    }
}

impl Registry {
    /// What the segment `argv` (a `>` token marks a redirection) does with each resource.
    pub fn contacts(&self, argv: &[String]) -> Vec<Contact<'_>> {
        let argv = without_git_globals(argv);
        let Some(tool) = argv.first().map(|t| command_name(t)) else { return Vec::new() };
        self.resource
            .iter()
            .filter_map(|r| {
                if r.door_words().iter().any(|w| starts_with_words(&argv, w)) {
                    Some(Contact::UsesDoor(r))
                } else if touches(r, tool, &argv) {
                    Some(Contact::Touches(r))
                } else {
                    None
                }
            })
            .collect()
    }
}

fn command_name(t: &str) -> &str {
    let b = t.rsplit('/').next().unwrap_or(t);
    b.strip_suffix(".sh").unwrap_or(b)
}

fn starts_with_words(argv: &[String], words: &str) -> bool {
    let mut it = argv.iter();
    words.split_whitespace().enumerate().all(|(i, w)| it.next().is_some_and(|t| if i == 0 { command_name(t) == command_name(w) } else { t == w }))
}

fn without_git_globals(argv: &[String]) -> Vec<String> {
    if argv.first().map(|t| command_name(t)) != Some("git") {
        return argv.to_vec();
    }
    let mut out = vec![argv[0].clone()];
    let mut i = 1;
    while i < argv.len() && argv[i].starts_with('-') {
        i += if GIT_VALUE_OPTS.contains(&argv[i].as_str()) { 2 } else { 1 };
    }
    out.extend(argv.iter().skip(i).cloned());
    out
}

/// `*` stays inside one path part, `**` and a leading `*/` cross parts, `?` is one character; a pattern without `/` is matched against the last part.
pub fn glob(pat: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.split_first() {
            None => t.is_empty(),
            Some(('*', rest)) if rest.first() == Some(&'*') => {
                let rest = &rest[1..];
                (0..=t.len()).any(|k| go(rest, &t[k..]))
            }
            Some(('*', rest)) => (0..=t.len()).take_while(|&k| k == 0 || t[k - 1] != '/').any(|k| go(rest, &t[k..])),
            Some(('?', rest)) => t.first().is_some_and(|&c| c != '/') && go(rest, &t[1..]),
            Some((c, rest)) => t.first() == Some(c) && go(rest, &t[1..]),
        }
    }
    let (pat, text) = if let Some(rest) = pat.strip_prefix("*/") {
        (format!("**/{rest}"), if text.starts_with('/') { text.to_string() } else { format!("/{text}") })
    } else if !pat.contains('/') {
        (pat.to_string(), text.rsplit('/').next().unwrap_or(text).to_string())
    } else {
        (pat.to_string(), text.to_string())
    };
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), text.chars().collect());
    go(&p, &t)
}

fn writes_arg(argv: &[String], i: usize) -> bool {
    let tool = command_name(&argv[0]);
    (i > 0 && argv[i - 1] == ">")
        || WRITERS.contains(&tool)
        || (tool == "sed" && argv.iter().any(|t| t.starts_with("-i") || t == "--in-place"))
}

fn touches(r: &Resource, tool: &str, argv: &[String]) -> bool {
    let t = &r.touched_by;
    let args = &argv[1..];
    let verb = t.verbs.iter().any(|v| starts_with_words(argv, v));
    let path = t.paths.iter().any(|p| args.iter().enumerate().any(|(i, a)| glob(p, a) && (!r.writes_only || writes_arg(argv, i + 1))));
    let git_ref = tool == "git" && !t.refs.is_empty() && git_moves_ref(argv, &t.refs, r.writes_only);
    let unit = tool == "systemctl"
        && t.units.iter().any(|u| args.iter().any(|a| glob(u, a)))
        && (!r.writes_only || args.iter().any(|a| UNIT_VERBS.contains(&a.as_str())));
    let binary = t.binaries.iter().any(|b| match b.split_once(':') {
        Some((bin, g)) => bin == tool && args.iter().any(|a| glob(g, a)),
        None => b == tool,
    });
    let traced = t.tracers.iter().any(|x| x == tool)
        && args.iter().any(|a| t.traced.iter().any(|n| command_name(a) == n.as_str()) || a == "-p" || a == "--pid" || (a.starts_with("-p") && a[2..].chars().all(|c| c.is_ascii_digit())));
    verb || path || git_ref || unit || binary || traced
}

fn ref_dest(arg: &str) -> &str {
    let dest = arg.rsplit(':').next().unwrap_or(arg);
    dest.strip_prefix('+').unwrap_or(dest)
}

fn git_moves_ref(argv: &[String], refs: &[String], writes_only: bool) -> bool {
    let sub = argv.get(1).map(String::as_str).unwrap_or("");
    let rest: Vec<&str> = argv.iter().skip(2).map(String::as_str).collect();
    let named = |a: &&str| !a.starts_with('-') && refs.iter().any(|r| glob(r, ref_dest(a)));
    if !writes_only {
        return rest.iter().any(named);
    }
    let flag = |long: &[&str], short: &str| rest.iter().any(|a| long.contains(a) || (a.starts_with('-') && !a.starts_with("--") && a[1..].chars().any(|c| short.contains(c))));
    match sub {
        "update-ref" | "symbolic-ref" => rest.iter().any(named),
        "branch" => flag(&["--force", "--delete", "--move", "--copy"], "fFDdmMcC") && rest.iter().any(named),
        "checkout" => flag(&[], "B") && rest.iter().any(named),
        "switch" => flag(&["--force-create"], "C") && rest.iter().any(named),
        "push" | "fetch" => rest.iter().filter(|a| !a.starts_with('-')).skip(1).any(|a| refs.iter().any(|r| glob(r, ref_dest(a)))),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
