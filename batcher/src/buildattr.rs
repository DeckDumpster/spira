//! Attribution of a workspace-build red: which member's diff the compiler's errors land in.
//!
//! An error at a file one member touches ejects it. When the error names an item another member's
//! diff defines, the member whose code lacks it is sequenced behind the definer instead. Any
//! error that maps to no single member leaves the whole verdict empty — a partial ejection
//! would re-cut a tree that still does not build.

use crate::core::Id;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberDiff {
    pub id: Id,
    pub paths: Vec<String>,
    /// The member's added lines, as `git diff` prints them (`+` prefixed).
    pub added: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    pub head: String,
    pub message: String,
    pub file: String,
    pub line: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Eject {
        id: Id,
        line: String,
    },
    Sequence {
        id: Id,
        behind: Vec<Id>,
        line: String,
    },
}

pub fn compile_errors(stderr: &str) -> Vec<CompileError> {
    let mut out = Vec::new();
    let mut lines = stderr.lines().peekable();
    while let Some(l) = lines.next() {
        let Some(rest) = l.strip_prefix("error") else {
            continue;
        };
        let Some((head, message)) = rest.split_once(": ") else {
            continue;
        };
        if !(head.is_empty() || (head.starts_with('[') && head.ends_with(']'))) {
            continue;
        }
        let at = lines
            .by_ref()
            .take(3)
            .find_map(|n| n.trim_start().strip_prefix("--> ").map(str::to_string));
        let Some(at) = at else { continue };
        let mut p = at.rsplitn(3, ':');
        let (_col, line, file) = (p.next(), p.next().and_then(|n| n.parse().ok()), p.next());
        if let (Some(line), Some(file)) = (line, file) {
            out.push(CompileError {
                head: format!("error{head}"),
                message: message.to_string(),
                file: file.to_string(),
                line,
            });
        }
    }
    out
}

fn quoted(message: &str) -> Vec<String> {
    let mut out = Vec::new();
    for q in ['`', '\''] {
        for (i, part) in message.split(q).enumerate() {
            let ident = part.rsplit("::").next().unwrap_or(part);
            if i % 2 == 1
                && !ident.is_empty()
                && ident.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
                out.push(ident.to_string());
            }
        }
    }
    out
}

fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        let part = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        !part(before) && !part(after)
    })
}

/// Whether the added line `line` defines `item`: an item keyword before it, or a field
/// declaration `item: Type`. An initializer `item: value` does not.
fn defines(line: &str, item: &str) -> bool {
    let line = line.trim_start_matches('+').trim();
    const KEYWORDS: [&str; 8] = [
        "fn", "struct", "enum", "trait", "type", "const", "static", "mod",
    ];
    let words: Vec<&str> = line
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    if let Some(i) = words.iter().position(|w| *w == item) {
        if i > 0 && KEYWORDS.contains(&words[i - 1]) {
            return true;
        }
    }
    let tail = match line.strip_prefix("pub") {
        Some(r) => r
            .trim_start()
            .strip_prefix('(')
            .and_then(|r| r.split_once(')'))
            .map_or(r.trim_start(), |(_, t)| t.trim_start()),
        None => line,
    };
    let Some(ty) = tail
        .strip_prefix(item)
        .and_then(|r| r.trim_start().strip_prefix(':'))
    else {
        return false;
    };
    let ty = ty.trim();
    const PRIMITIVES: [&str; 14] = [
        "bool", "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize", "f32",
        "f64", "str",
    ];
    let name: String = ty
        .trim_start_matches(['&', '[', '\''])
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let after = &ty
        .trim_start_matches(['&', '[', '\''])
        .get(name.len()..)
        .unwrap_or("");
    let is_type =
        PRIMITIVES.contains(&name.as_str()) || name.chars().next().is_some_and(char::is_uppercase);
    is_type
        && !after.starts_with("::")
        && !after.starts_with('(')
        && !after.starts_with(" {")
        && !after.starts_with('{')
}

fn provides(m: &MemberDiff, item: &str) -> bool {
    m.added
        .lines()
        .any(|l| l.starts_with('+') && has_word(l, item) && defines(l, item))
}

fn touches(m: &MemberDiff, file: &str) -> bool {
    m.paths
        .iter()
        .any(|p| p == file || p.ends_with(&format!("/{file}")) || file.ends_with(&format!("/{p}")))
}

fn one(e: &CompileError, members: &[MemberDiff]) -> Option<Action> {
    let touchers: Vec<&MemberDiff> = members.iter().filter(|m| touches(m, &e.file)).collect();
    let items = quoted(&e.message);
    let krate = e.file.split('/').next().unwrap_or_default();
    let in_crate = |m: &MemberDiff| m.paths.iter().any(|p| p.split('/').next() == Some(krate));
    let providers: Vec<&MemberDiff> = members
        .iter()
        .filter(|m| in_crate(m) && items.iter().any(|i| provides(m, i)))
        .collect();
    let line = format!("{} at {}:{}: {}", e.head, e.file, e.line, e.message);
    let dependents: Vec<&&MemberDiff> = touchers
        .iter()
        .filter(|t| !providers.iter().any(|p| p.id == t.id))
        .collect();
    let behind: Vec<Id> = providers
        .iter()
        .filter(|p| dependents.iter().all(|d| d.id != p.id))
        .map(|p| p.id.clone())
        .collect();
    match (&dependents[..], touchers.len()) {
        ([d], _) if !behind.is_empty() => Some(Action::Sequence {
            id: d.id.clone(),
            behind,
            line,
        }),
        ([d], 1) => Some(Action::Eject {
            id: d.id.clone(),
            line,
        }),
        ([], 1) => Some(Action::Eject {
            id: touchers[0].id.clone(),
            line,
        }),
        _ => None,
    }
}

/// The paths whose change makes the round image build, as `round-ami-build.yml` triggers on them.
pub const RECIPE_PATHS: [&str; 8] = [
    "round-vm/ami-setup.sh",
    "round-vm/src/template.sh",
    "round-vm/src/recipe.rs",
    "rust-toolchain.toml",
    "flake.nix",
    "flake.lock",
    "spira/testenv/Containerfile",
    ".github/workflows/round-ami-build.yml",
];

/// Whether the round's build output is the image build's failure rather than the compiler's.
pub fn is_image_build_red(output: &str) -> bool {
    output
        .lines()
        .any(|l| l.contains("round image build") && l.contains("failed"))
}

/// The members whose diff touches the image recipe.
pub fn recipe_touchers(members: &[MemberDiff]) -> Vec<&MemberDiff> {
    members
        .iter()
        .filter(|m| m.paths.iter().any(|p| RECIPE_PATHS.contains(&p.as_str())))
        .collect()
}

const RUNNER_PATHS: [&str; 2] = ["round-vm/", "testenv/"];

/// The members whose diff touches the code that runs the suites in the round VM.
pub fn runner_touchers(members: &[MemberDiff]) -> Vec<&MemberDiff> {
    members
        .iter()
        .filter(|m| {
            m.paths
                .iter()
                .any(|p| RUNNER_PATHS.iter().any(|r| p.starts_with(r)))
        })
        .collect()
}

/// Whether the failure text is the merged tree's config refusing to resolve a key it reads.
pub fn is_config_fault(err: &str) -> bool {
    err.contains("is not declared")
}

/// Whether the runner's failure text shows testenv killed by a signal (exit 128+n), which is
/// outside its contract and so never a verdict.
pub fn is_signal_death(err: &str) -> bool {
    if err.contains("over-cap") {
        return false;
    }
    let after = |key: &str| {
        err.match_indices(key).any(|(i, _)| {
            let n: String = err[i + key.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            n.parse::<u32>().is_ok_and(|rc| (129..=192).contains(&rc))
        })
    };
    after("PHASE-FAILED phase=suites rc=") || after("the remote runner exited ")
}

/// The last lines of the build output, as the evidence an image-build ejection carries.
pub fn error_tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(8)..].join(" | ")
}

/// One action per implicated member, or empty when any error maps to no single member.
pub fn attribute(errors: &[CompileError], members: &[MemberDiff]) -> Vec<Action> {
    let mut out: Vec<Action> = Vec::new();
    for e in errors {
        let Some(a) = one(e, members) else {
            return vec![];
        };
        let id = match &a {
            Action::Eject { id, .. } | Action::Sequence { id, .. } => id.clone(),
        };
        if !out.iter().any(|o| matches!(o, Action::Eject { id: i, .. } | Action::Sequence { id: i, .. } if *i == id)) {
            out.push(a);
        }
    }
    out
}

#[cfg(test)]
mod tests;
