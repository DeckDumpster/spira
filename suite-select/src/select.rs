//! The selection algorithm (DESIGN.md "Selection") — pure. What `spira/select.sh` carried,
//! over a loaded [`Corpus`] and a list of [`Change`]s; git reads live in `io`.

use crate::corpus::{Corpus, Suite};
use crate::glob::case_match;
use crate::Refusal;

/// One changed path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    /// Diff status `A`.
    pub added: bool,
    /// Diff status `D`.
    pub deleted: bool,
    /// The file mode changed (`git diff --raw`, an existing file).
    pub mode_changed: bool,
}

impl Change {
    pub fn modified(path: &str) -> Change {
        Change {
            path: path.to_string(),
            ..Change::default()
        }
    }
}

/// `git diff --name-status` lines, or a `--files` list: `STATUS<TAB>PATH[<TAB>PATH]`, or a bare
/// path (status unknown: treated as modified). A rename or copy names both paths, each a
/// change. Blank lines are skipped. The same path twice is one change, its flags merged.
pub fn parse_name_status(text: &str) -> Vec<Change> {
    let mut out: Vec<Change> = Vec::new();
    let mut push = |c: Change| {
        if let Some(e) = out.iter_mut().find(|e| e.path == c.path) {
            e.added |= c.added;
            e.deleted |= c.deleted;
            e.mode_changed |= c.mode_changed;
        } else {
            out.push(c);
        }
    };
    for l in text.lines() {
        if l.is_empty() {
            continue;
        }
        let mut f = l.split('\t');
        let first = f.next().unwrap_or("");
        let rest: Vec<&str> = f.filter(|p| !p.is_empty()).collect();
        if rest.is_empty() {
            push(Change::modified(first));
            continue;
        }
        for p in rest {
            push(Change {
                path: p.to_string(),
                added: first == "A",
                deleted: first.starts_with('D'),
                mode_changed: false,
            });
        }
    }
    out
}

/// `git diff --raw` (not `-z`): the paths whose mode changed on an existing file
/// (`:<old> <new> …`, old not `000000`, old ≠ new). The path is the last tab field.
pub fn parse_mode_changes(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for l in raw.lines() {
        let Some(r) = l.strip_prefix(':') else { continue };
        let mut w = r.split(' ');
        let (Some(om), Some(nm)) = (w.next(), w.next()) else { continue };
        if om != "000000" && om != nm {
            if let Some(p) = l.rsplit('\t').next() {
                out.push(p.to_string());
            }
        }
    }
    out
}

/// The file buckets (DESIGN.md): inert, source, plumbing, as case patterns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buckets {
    pub inert: Vec<String>,
    pub source: Vec<String>,
    pub plumbing: Vec<String>,
}

pub const DEFAULT_INERT: &str = "*.md *.txt";
pub const DEFAULT_SOURCE: &str = "spira/*.sh";
pub const DEFAULT_PLUMBING: &str = "Makefile Cargo.toml Cargo.lock */Cargo.toml install/src/bin/install.rs systemd/* spira/conf.sh spira/lib.sh skew/src/* spira/testenv*.sh .github/*";

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

impl Default for Buckets {
    fn default() -> Self {
        Buckets {
            inert: words(DEFAULT_INERT),
            source: words(DEFAULT_SOURCE),
            plumbing: words(DEFAULT_PLUMBING),
        }
    }
}

impl Buckets {
    /// `SPIRA_SELECT_INERT`, `SPIRA_SELECT_SOURCE`, `SPIRA_SELECT_PLUMBING`, each replacing
    /// its default when set and non-empty (the bash's `${VAR:-default}`).
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Buckets {
        let pick = |k: &str, d: &str| {
            words(&get(k).filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string()))
        };
        Buckets {
            inert: pick("SPIRA_SELECT_INERT", DEFAULT_INERT),
            source: pick("SPIRA_SELECT_SOURCE", DEFAULT_SOURCE),
            plumbing: pick("SPIRA_SELECT_PLUMBING", DEFAULT_PLUMBING),
        }
    }
}

fn any_match(pats: &[String], path: &str) -> bool {
    pats.iter().any(|p| case_match(p, path))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// An unplaced or plumbing file does not select the whole corpus.
    pub no_all_fallback: bool,
    /// Leave out the always-run (no `# covers:`) suites.
    pub no_nocov: bool,
    /// Keep only suites whose tier is listed (and every untiered suite). `None`: no filter.
    pub tiers: Option<Vec<String>>,
}

/// `--tiers a,b`: `None` when it names nothing.
pub fn parse_tiers(csv: &str) -> Option<Vec<String>> {
    let v: Vec<String> = csv
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    (!v.is_empty()).then_some(v)
}

impl Options {
    fn tier_ok(&self, s: &Suite) -> bool {
        match (&self.tiers, s.tier) {
            (None, _) | (_, None) => true,
            (Some(ts), Some(t)) => ts.iter().any(|x| x == t.as_str()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Selected from the diff.
    Diff,
    /// The whole corpus (`--all`, or the fallback).
    All,
}

impl Mode {
    pub fn word(self) -> &'static str {
        match self {
            Mode::Diff => "diff",
            Mode::All => "all",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub suites: Vec<String>,
    pub mode: Mode,
    /// Live changed files no suite claims (not source files).
    pub unplaced: Vec<String>,
    /// What was decided, for stderr.
    pub log: Vec<String>,
}

/// Why a selection was not produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// The selector could not compute a selection (exit 75).
    Refused(Refusal),
    /// Changed source files no suite claims (exit 1); `unplaced` as in [`Selection`].
    Unclaimed {
        files: Vec<String>,
        unplaced: Vec<String>,
    },
}

impl From<Refusal> for Fail {
    fn from(r: Refusal) -> Fail {
        Fail::Refused(r)
    }
}

fn push_once(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

/// `--all`: every suite (tier-filtered).
pub fn all(corpus: &Corpus, opts: &Options) -> Selection {
    Selection {
        suites: corpus
            .suites
            .iter()
            .filter(|s| opts.tier_ok(s))
            .map(|s| s.name.clone())
            .collect(),
        mode: Mode::All,
        unplaced: vec![],
        log: vec![],
    }
}

fn nocov(corpus: &Corpus, opts: &Options) -> Vec<String> {
    if opts.no_nocov {
        return vec![];
    }
    corpus
        .suites
        .iter()
        .filter(|s| s.covers.is_none())
        .map(|s| s.name.clone())
        .collect()
}

/// THE SELECTION (DESIGN.md "Selection", steps 1–7). `fn_changed(file)` names the shell
/// functions with a changed line in `file` (empty: none, or unknown — every suite naming a
/// function of the file then runs).
pub fn select(
    corpus: &Corpus,
    changes: &[Change],
    fn_changed: &mut dyn FnMut(&str) -> Vec<String>,
    buckets: &Buckets,
    opts: &Options,
) -> Result<Selection, Fail> {
    let tier_filter = |names: Vec<String>| -> Vec<String> {
        names
            .into_iter()
            .filter(|n| corpus.get(n).is_some_and(|s| opts.tier_ok(s)))
            .collect()
    };
    // 1. Inert files select nothing and never fall back.
    let live: Vec<&Change> = changes
        .iter()
        .filter(|c| !any_match(&buckets.inert, &c.path))
        .collect();
    if live.is_empty() {
        return Ok(Selection {
            suites: tier_filter(nocov(corpus, opts)),
            mode: Mode::Diff,
            unplaced: vec![],
            log: vec![],
        });
    }
    let plumbing: Vec<&str> = live
        .iter()
        .filter(|c| any_match(&buckets.plumbing, &c.path))
        .map(|c| c.path.as_str())
        .collect();

    // 2. Covers match.
    let mut selected: Vec<String> = Vec::new();
    let mut unmapped: Vec<&Change> = Vec::new();
    for c in &live {
        let f = c.path.as_str();
        let mut hit = false;
        let mut fn_pairs: Vec<(&str, &str)> = Vec::new();
        for s in &corpus.suites {
            let Some(cov) = &s.covers else { continue };
            for pat in cov {
                if let Some((fpat, fname)) = pat.split_once('#') {
                    if case_match(fpat, f) {
                        hit = true;
                        fn_pairs.push((s.name.as_str(), fname));
                    }
                } else if case_match(pat, f) {
                    hit = true;
                    // A selects-on suite's covers claims the file but selects only on its
                    // events (step 5).
                    if s.selects_on.is_empty() {
                        push_once(&mut selected, &s.name);
                    }
                }
            }
        }
        if !fn_pairs.is_empty() {
            let fns = fn_changed(f);
            let mut any = false;
            for (s, fname) in &fn_pairs {
                if fns.iter().any(|x| x == fname) {
                    any = true;
                    push_once(&mut selected, s);
                }
            }
            if !any {
                for (s, _) in &fn_pairs {
                    push_once(&mut selected, s);
                }
            }
        }
        if !hit {
            unmapped.push(c);
        }
    }

    // 3. Unclaimed source files are the branch's fault; a deleted one needs no claim.
    let mut unclaimed = Vec::new();
    let mut unplaced = Vec::new();
    for c in unmapped {
        if !c.deleted && any_match(&buckets.source, &c.path) {
            unclaimed.push(c.path.clone());
        } else {
            unplaced.push(c.path.clone());
        }
    }
    if !unclaimed.is_empty() {
        return Err(Fail::Unclaimed {
            files: unclaimed,
            unplaced,
        });
    }

    // 4. The all-suites fallback.
    if (!unplaced.is_empty() || !plumbing.is_empty()) && !opts.no_all_fallback {
        let mut log: Vec<String> = unplaced
            .iter()
            .map(|f| format!("select: {f} → [all: unmapped]"))
            .collect();
        log.extend(plumbing.iter().map(|f| format!("select: {f} → [all: plumbing]")));
        log.push(format!(
            "select: fallback — running all {} suites",
            corpus.suites.len()
        ));
        let mut sel = all(corpus, opts);
        sel.log = log;
        sel.unplaced = unplaced;
        return Ok(sel);
    }

    // 5. Selects-on events. Added and mode-changed files are every change, inert included.
    for s in &corpus.suites {
        if s.selects_on.is_empty() {
            continue;
        }
        let Some(cov) = &s.covers else { continue };
        let plain: Vec<&String> = cov.iter().filter(|p| !p.contains('#')).collect();
        let fires = s.selects_on.iter().any(|ev| {
            let files: Vec<&str> = match ev.as_str() {
                "added" => changes.iter().filter(|c| c.added).map(|c| c.path.as_str()).collect(),
                "mode" => changes
                    .iter()
                    .filter(|c| c.mode_changed)
                    .map(|c| c.path.as_str())
                    .collect(),
                _ => vec![],
            };
            files.iter().any(|f| plain.iter().any(|p| case_match(p, f)))
        });
        if fires {
            push_once(&mut selected, &s.name);
        }
    }

    // 6–7. Always-run suites, then the tier filter.
    for n in nocov(corpus, opts) {
        push_once(&mut selected, &n);
    }
    let suites = tier_filter(selected);
    let log = vec![format!("select: {} suite(s) selected", suites.len())];
    Ok(Selection {
        suites,
        mode: Mode::Diff,
        unplaced,
        log,
    })
}

/// The report's `unclaimed:` half: tracked `*.sh`/`*.py` files (not suites) that no suite's
/// `# covers:` glob claims (a `file#func` token claims its file).
pub fn unclaimed_tracked(corpus: &Corpus, tracked: &[String]) -> Vec<String> {
    let pats: Vec<&str> = corpus
        .suites
        .iter()
        .filter_map(|s| s.covers.as_ref())
        .flatten()
        .map(|p| p.split('#').next().unwrap_or(p))
        .filter(|p| !p.is_empty())
        .collect();
    tracked
        .iter()
        .filter(|f| !(case_match("*/test-*.sh", f) || case_match("test-*.sh", f)))
        .filter(|f| f.ends_with(".sh") || f.ends_with(".py"))
        .filter(|f| !pats.iter().any(|p| case_match(p, f)))
        .cloned()
        .collect()
}

/// The report file's text: `unplaced:<f>` then `unclaimed:<f>`, one per line.
pub fn report_text(unplaced: &[String], unclaimed: &[String]) -> String {
    let mut s = String::new();
    for f in unplaced {
        s.push_str(&format!("unplaced:{f}\n"));
    }
    for f in unclaimed {
        s.push_str(&format!("unclaimed:{f}\n"));
    }
    s
}

/// `_fn_changed_in`: the shell functions (`name()` … `}` blocks of `head_text`) that hold a
/// line of the `git diff --unified=0` hunks in `diff_u0`, in order of first appearance.
pub fn changed_functions(diff_u0: &str, head_text: &str) -> Vec<String> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for l in diff_u0.lines() {
        if !l.starts_with("@@") {
            continue;
        }
        let Some(i) = l.find('+') else { continue };
        let spec: String = l[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == ',')
            .collect();
        let mut p = spec.split(',');
        let Some(Ok(start)) = p.next().map(|s| s.parse::<usize>()) else {
            continue;
        };
        let count = match p.next() {
            Some(c) => c.parse::<usize>().unwrap_or(0),
            None => 1,
        };
        if count == 0 {
            continue;
        }
        ranges.push((start, start + count - 1));
    }
    if ranges.is_empty() {
        return vec![];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, line) in head_text.lines().enumerate() {
        let nr = i + 1;
        if line.starts_with('}') {
            cur.clear();
        }
        if let Some(name) = fn_def(line) {
            cur = name;
        }
        if !cur.is_empty() && ranges.iter().any(|(a, b)| nr >= *a && nr <= *b) {
            push_once(&mut out, &cur);
        }
    }
    out
}

/// `^[A-Za-z_][A-Za-z0-9_]*\(\)` → the name.
fn fn_def(line: &str) -> Option<String> {
    let b = line.as_bytes();
    if b.is_empty() || !(b[0].is_ascii_alphabetic() || b[0] == b'_') {
        return None;
    }
    let n = b
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
        .count();
    line[n..].starts_with("()").then(|| line[..n].to_string())
}

#[cfg(test)]
mod tests;
