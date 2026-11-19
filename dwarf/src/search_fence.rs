//! The search fence: a builder session's recursive search of the run directory is refused
//! before it runs. Pure: the payload in, the refusal out; `dwarf search-fence` does the I/O.

use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

struct Segment {
    words: Vec<String>,
    heredoc: String,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    segs: Vec<Segment>,
    words: Vec<String>,
    word: String,
    has_word: bool,
    skip_next: bool,
    pending: Vec<(String, usize)>,
}

impl Lexer<'_> {
    fn end_word(&mut self) {
        if self.has_word {
            let w = std::mem::take(&mut self.word);
            if self.skip_next {
                self.skip_next = false;
            } else {
                self.words.push(w);
            }
        }
        self.word.clear();
        self.has_word = false;
    }

    fn end_segment(&mut self) {
        self.end_word();
        if !self.words.is_empty() {
            self.segs.push(Segment { words: std::mem::take(&mut self.words), heredoc: String::new() });
        }
    }

    fn quoted(&mut self, q: char) {
        self.has_word = true;
        while let Some(c) = self.chars.next() {
            if c == q {
                return;
            }
            if c == '\\' && q == '"' {
                if let Some(n) = self.chars.next() {
                    self.word.push(n);
                }
                continue;
            }
            self.word.push(c);
        }
    }

    fn redirect(&mut self, first: char) {
        if self.word.chars().all(|c| c.is_ascii_digit() || c == '&') {
            self.word.clear();
            self.has_word = false;
        }
        if first == '<' && self.chars.peek() == Some(&'<') {
            self.chars.next();
            if self.chars.peek() == Some(&'<') {
                self.chars.next();
                self.skip_next = true;
                return;
            }
            if self.chars.peek() == Some(&'-') {
                self.chars.next();
            }
            self.end_word();
            while self.chars.peek().is_some_and(|c| c.is_whitespace() && *c != '\n') {
                self.chars.next();
            }
            let mut delim = String::new();
            while let Some(&c) = self.chars.peek() {
                if c.is_whitespace() || ";&|()<>".contains(c) {
                    break;
                }
                self.chars.next();
                if c != '\'' && c != '"' && c != '\\' {
                    delim.push(c);
                }
            }
            self.pending.push((delim, self.segs.len() + usize::from(!self.words.is_empty())));
            return;
        }
        while self.chars.peek().is_some_and(|c| matches!(c, '<' | '>' | '&')) {
            self.chars.next();
        }
        self.end_word();
        self.skip_next = true;
    }

    fn heredoc_bodies(&mut self) {
        for (delim, at) in std::mem::take(&mut self.pending) {
            let mut body = String::new();
            let mut line = String::new();
            loop {
                match self.chars.next() {
                    Some('\n') | None => {
                        if line.trim() == delim {
                            break;
                        }
                        body.push_str(&line);
                        body.push('\n');
                        if self.chars.peek().is_none() && line.is_empty() {
                            break;
                        }
                        line.clear();
                    }
                    Some(c) => line.push(c),
                }
            }
            if let Some(s) = self.segs.get_mut(at.saturating_sub(1)) {
                s.heredoc = body;
            }
        }
    }
}

fn lex(cmd: &str) -> Vec<Segment> {
    let mut l = Lexer { chars: cmd.chars().peekable(), segs: vec![], words: vec![], word: String::new(), has_word: false, skip_next: false, pending: vec![] };
    while let Some(c) = l.chars.next() {
        match c {
            '\'' | '"' => l.quoted(c),
            '\\' => {
                if let Some(n) = l.chars.next() {
                    l.word.push(n);
                    l.has_word = true;
                }
            }
            '\n' => {
                l.end_segment();
                if !l.pending.is_empty() {
                    l.heredoc_bodies();
                }
            }
            ';' | '&' | '|' | '(' | ')' | '`' => l.end_segment(),
            '<' | '>' => l.redirect(c),
            c if c.is_whitespace() => l.end_word(),
            c => {
                l.word.push(c);
                l.has_word = true;
            }
        }
    }
    l.end_segment();
    l.segs
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

fn has_glob(s: &str) -> bool {
    s.contains(['*', '?', '[', '{'])
}

/// The lexical root an operand names; `None` when it cannot be known without a shell.
fn resolve(operand: &str, cwd: Option<&Path>, home: &str) -> Option<PathBuf> {
    let mut s = operand.to_string();
    if s == "~" || s.starts_with("~/") {
        s = format!("{home}{}", &s[1..]);
    } else if let Some(r) = s.strip_prefix("${HOME}").or_else(|| s.strip_prefix("$HOME")) {
        if r.is_empty() || r.starts_with('/') {
            s = format!("{home}{r}");
        }
    }
    if s.contains(['$', '`']) {
        return None;
    }
    if has_glob(&s) {
        let mut dir = Vec::new();
        for part in s.split('/') {
            if has_glob(part) {
                break;
            }
            dir.push(part);
        }
        s = dir.join("/");
        if s.is_empty() {
            s = ".".into();
        }
    }
    let p = Path::new(&s);
    if p.is_absolute() {
        Some(normalize(p))
    } else {
        cwd.map(|c| normalize(&c.join(p)))
    }
}

/// The hook's whole stdout for one stdin payload: the block JSON on a refusal, else nothing.
/// An unparseable payload passes; the override lifts the search fence only, never the store fence.
pub fn hook_output(raw: &str, search: Option<(&Path, &Path)>, home: &str, overridden: bool) -> Option<String> {
    let payload: Value = serde_json::from_str(raw).ok()?;
    let search = search.filter(|_| !overridden);
    refusal(&payload, search, home).map(|reason| json!({"decision": "block", "reason": reason}).to_string())
}

const GREP_ARG_SHORT: &str = "efABCmdD";
const GREP_ARG_LONG: &[&str] = &["include", "exclude", "exclude-dir", "exclude-from", "regexp", "file", "max-count", "after-context", "before-context", "context", "directories", "devices", "label", "binary-files"];
const RG_ARG_SHORT: &str = "efgtTABCmjMEdr";
const RG_ARG_LONG: &[&str] = &["regexp", "file", "glob", "iglob", "type", "type-not", "max-count", "max-depth", "maxdepth", "threads", "replace", "after-context", "before-context", "context", "max-filesize", "encoding", "sort", "sortr", "ignore-file", "pre", "pre-glob"];

/// The path operands of a grep or rg command line, and whether the search is recursive.
fn grep_operands(args: &[String], rg: bool) -> (bool, Vec<String>) {
    let (short_arg, long_arg) = if rg { (RG_ARG_SHORT, RG_ARG_LONG) } else { (GREP_ARG_SHORT, GREP_ARG_LONG) };
    let (mut recursive, mut have_pattern, mut files_mode) = (rg, false, false);
    let mut operands = Vec::new();
    let mut i = 0;
    let mut flags_done = false;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if flags_done || !a.starts_with('-') || a == "-" {
            operands.push(a.clone());
            continue;
        }
        if a == "--" {
            flags_done = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = long.split_once('=').map_or((long, false), |(n, _)| (n, true));
            match name {
                "recursive" | "dereference-recursive" => recursive = true,
                "files" => files_mode = true,
                "regexp" | "file" => have_pattern = true,
                _ => {}
            }
            if name == "directories" && (long.ends_with("=recurse") || args.get(i).is_some_and(|v| v == "recurse")) {
                recursive = true;
            }
            if !inline && long_arg.contains(&name) {
                i += 1;
            }
            continue;
        }
        let cluster: Vec<char> = a[1..].chars().collect();
        for (k, &c) in cluster.iter().enumerate() {
            if !rg && (c == 'r' || c == 'R') {
                recursive = true;
            }
            if short_arg.contains(c) {
                let val: String = if k + 1 < cluster.len() { cluster[k + 1..].iter().collect() } else { i += 1; args.get(i - 1).cloned().unwrap_or_default() };
                if c == 'e' || c == 'f' {
                    have_pattern = true;
                }
                if !rg && c == 'd' && val == "recurse" {
                    recursive = true;
                }
                break;
            }
        }
    }
    if !have_pattern && !files_mode && !operands.is_empty() {
        operands.remove(0);
    }
    (recursive, operands)
}

/// The path operands of a `find`, or `None` when `-maxdepth 0|1` bounds it.
fn find_operands(args: &[String]) -> Option<Vec<String>> {
    if args.windows(2).any(|w| w[0] == "-maxdepth" && (w[1] == "0" || w[1] == "1")) {
        return None;
    }
    let mut i = 0;
    while i < args.len() && matches!(args[i].as_str(), "-H" | "-L" | "-P" | "-D" | "-O1" | "-O2" | "-O3") {
        i += if args[i] == "-D" { 2 } else { 1 };
    }
    let ops = args[i.min(args.len())..].iter().take_while(|a| !(a.starts_with('-') || *a == "(" || *a == "!")).cloned().collect();
    Some(ops)
}

fn strip_wrappers(words: &[String]) -> &[String] {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        let is_assign = w.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !k.starts_with(|c: char| c.is_ascii_digit()));
        match w {
            _ if is_assign => i += 1,
            "command" | "nohup" | "exec" | "time" => i += 1,
            "xargs" => {
                i += 1;
                while i < words.len() && words[i].starts_with('-') {
                    let takes_value = matches!(words[i].as_str(), "-n" | "-I" | "-L" | "-P" | "-d" | "-E" | "-s" | "-a");
                    i += if takes_value { 2 } else { 1 };
                }
            }
            "env" => {
                i += 1;
                while i < words.len() && words[i].starts_with('-') {
                    i += 1;
                }
            }
            "timeout" => {
                i += 1;
                while i < words.len() && words[i].starts_with('-') {
                    i += 1;
                }
                i += 1;
            }
            "nice" => {
                i += 1;
                if words.get(i).is_some_and(|a| a == "-n") {
                    i += 2;
                } else if words.get(i).is_some_and(|a| a.starts_with('-')) {
                    i += 1;
                }
            }
            _ => break,
        }
    }
    &words[i.min(words.len())..]
}

const STORE_BINARIES: &[&str] = &["bd", "bdq", "bd-embedded", "dolt"];

const STORE_REFUSAL: &str = "dwarf store-fence: bd is the store server's alone (law-only-spira-lc-talks-to-the-stores). Your bead: `work show|note|ask|done`. Anything else: `spira-lc <verb>`; a missing verb is a bead, not a side door.";

struct Ctx<'a> {
    search: Option<(&'a Path, &'a Path)>,
    home: &'a str,
}

impl Ctx<'_> {
    fn refused_root(run: &Path, worktree: &Path, root: &Path) -> bool {
        run.starts_with(root) || (root.starts_with(run) && !root.starts_with(worktree))
    }

    fn check(&self, head: &str, operands: &[String], cwd: Option<&Path>) -> Option<String> {
        let (run, worktree) = self.search?;
        let roots: Vec<Option<PathBuf>> = if operands.is_empty() { vec![cwd.map(normalize)] } else { operands.iter().filter(|o| *o != "-").map(|o| resolve(o, cwd, self.home)).collect() };
        roots.into_iter().flatten().find(|r| Self::refused_root(run, worktree, r)).map(|r| Self::reason(run, worktree, head, &r))
    }

    fn reason(run: &Path, worktree: &Path, head: &str, root: &Path) -> String {
        let (run, wt) = (run.display(), worktree.display());
        format!(
            "dwarf search-fence: {head} searches {}, which holds every session's transcripts and logs (sp-7lcsyn: one such search took 30 minutes of a session). For a round or gate red, run `work show`: its LAST RED section is the failure the round recorded. To read one known file, name it without a recursive flag (grep <pattern> {run}/<file>; rg always recurses, so use grep or the Read tool). Your worktree {wt} is searched freely.",
            root.display()
        )
    }

    fn command(&self, cmd: &str, mut cwd: Option<PathBuf>, depth: u32) -> Option<String> {
        if depth > 4 {
            return None;
        }
        for seg in lex(cmd) {
            let words = strip_wrappers(&seg.words);
            let Some(head) = words.first() else { continue };
            let args = &words[1..];
            let base = head.rsplit('/').next().unwrap_or(head);
            match base {
                b if STORE_BINARIES.contains(&b) => return Some(STORE_REFUSAL.to_string()),
                "eval" => {
                    if let Some(r) = self.command(&args.join(" "), cwd.clone(), depth + 1) {
                        return Some(r);
                    }
                }
                "cd" | "pushd" => {
                    let target = args.iter().find(|a| !a.starts_with('-') || *a == "-");
                    cwd = match target {
                        None => Some(PathBuf::from(self.home)),
                        Some(t) => resolve(t, cwd.as_deref(), self.home),
                    };
                }
                "popd" => cwd = None,
                "bash" | "sh" | "zsh" | "dash" => {
                    let c = args.iter().position(|a| a.starts_with('-') && !a.starts_with("--") && a.ends_with('c'));
                    let body = match c.and_then(|i| args.get(i + 1)) {
                        Some(b) => b.as_str(),
                        None => seg.heredoc.as_str(),
                    };
                    if let Some(r) = self.command(body, cwd.clone(), depth + 1) {
                        return Some(r);
                    }
                }
                "grep" | "egrep" | "fgrep" | "rg" => {
                    let (recursive, ops) = grep_operands(args, base == "rg");
                    if recursive {
                        if let Some(r) = self.check(base, &ops, cwd.as_deref()) {
                            return Some(r);
                        }
                    }
                }
                "find" => {
                    if let Some(ops) = find_operands(args) {
                        if let Some(r) = self.check("find", &ops, cwd.as_deref()) {
                            return Some(r);
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }
}

/// The refusal for a PreToolUse payload, or `None` when the call may run.
pub fn refusal(payload: &Value, search: Option<(&Path, &Path)>, home: &str) -> Option<String> {
    let ctx = Ctx { search, home };
    let cwd = payload["cwd"].as_str().map(PathBuf::from).or_else(|| search.map(|(_, w)| w.to_path_buf()));
    let input = &payload["tool_input"];
    match payload["tool_name"].as_str()? {
        "Bash" => ctx.command(input["command"].as_str()?, cwd, 0),
        "Grep" => {
            let ops: Vec<String> = input["path"].as_str().map(|p| p.to_string()).into_iter().collect();
            ctx.check("the Grep tool", &ops, cwd.as_deref())
        }
        "Glob" => {
            let pattern = input["pattern"].as_str()?;
            if !pattern.contains("**") {
                return None;
            }
            let root = input["path"].as_str();
            let ops: Vec<String> = if pattern.starts_with(['/', '~']) { vec![pattern.to_string()] } else { root.map(String::from).into_iter().collect() };
            ctx.check("the Glob tool", &ops, cwd.as_deref())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static ROOT: testkit::TempDir = testkit::TempDir::new("search-fence");
    }

    fn root() -> String {
        ROOT.with(|r| r.path().display().to_string())
    }

    fn at(s: &str) -> String {
        s.replace("@R", &root())
    }

    fn run_dir() -> String {
        at("@R/spira/run")
    }

    fn wt_dir() -> String {
        at("@R/spira/run/worktree/sp-a")
    }

    fn verdict(payload: Value) -> Option<String> {
        refusal(&payload, Some((Path::new(&run_dir()), Path::new(&wt_dir()))), &root())
    }

    #[test]
    fn the_hook_prints_block_json_on_a_refusal_and_nothing_otherwise() {
        let (run, wt) = (run_dir(), wt_dir());
        let (run, wt) = (Path::new(&run), Path::new(&wt));
        let call = |cmd: &str| json!({"tool_name": "Bash", "cwd": wt_dir(), "tool_input": {"command": cmd}}).to_string();
        let out = hook_output(&call(&at("grep -rl x @R/spira/run")), Some((run, wt)), &root(), false).expect("refused");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["decision"], "block");
        assert!(v["reason"].as_str().unwrap().contains("work show"));
        assert_eq!(hook_output(&call("grep -rn x ."), Some((run, wt)), &root(), false), None);
        assert_eq!(hook_output(&call(&at("grep -rl x @R/spira/run")), Some((run, wt)), &root(), true), None, "override passes the search fence");
        assert!(hook_output(&call("bd show sp-a"), Some((run, wt)), &root(), true).is_some(), "override does not lift the store fence");
        assert!(hook_output(&call("bd show sp-a"), None, &root(), false).is_some(), "store fence needs no run dir");
        assert_eq!(hook_output(&call(&at("grep -rl x @R/spira/run")), None, &root(), false), None, "no search fence without a run dir");
        assert_eq!(hook_output("not json", Some((run, wt)), &root(), false), None, "unparseable passes");
    }

    #[test]
    fn rg_on_one_run_file_is_refused_and_the_reason_points_at_grep() {
        let r = bash("rg foo @R/spira/run/sp-x.log").expect("rg always recurses");
        assert!(r.contains("use grep or the Read tool"), "{r}");
    }

    fn bash(cmd: &str) -> Option<String> {
        verdict(json!({"tool_name": "Bash", "cwd": wt_dir(), "tool_input": {"command": at(cmd)}}))
    }

    fn refused(cmd: &str) {
        let r = bash(cmd).unwrap_or_else(|| panic!("not refused: {cmd}"));
        assert!(r.contains("work show"), "{r}");
    }

    fn allowed(cmd: &str) {
        assert_eq!(bash(cmd), None, "{cmd}");
    }

    fn store_refused(cmd: &str) {
        let r = bash(cmd).unwrap_or_else(|| panic!("not refused: {cmd}"));
        assert!(r.contains("spira-lc") && r.contains("work"), "{r}");
    }

    #[test]
    fn every_way_of_reaching_a_store_binary_is_refused() {
        for c in ["bd show sp-a", "/opt/x/bin/bdq list", "env A=1 bd create x", "timeout 5 bd-embedded", "bash -c 'bd dep add a b'", "sh <<'EOF'\nbdq ready\nEOF", "echo sp-a | xargs bd show", "echo sp-a | xargs -n 1 bd show", "eval \"bd close sp-a\"", "dolt sql", "cd /tmp && ./bd list"] {
            store_refused(c);
        }
    }

    #[test]
    fn commands_that_only_mention_a_store_binary_run() {
        for c in ["work show", "spira-lc show sp-a", "grep bd file.rs", "git grep bdq", "echo bd", "cat bead/src/bin/bdq.rs"] {
            allowed(c);
        }
    }

    #[test]
    fn a_recursive_search_with_no_operand_searches_the_cwd_and_is_refused_in_the_run_dir() {
        refused("cd @R/spira/run && rg foo");
        refused("cd @R/spira/run && grep -r foo");
        allowed("cd @R/spira/run/worktree/sp-a/dwarf && rg foo");
    }

    #[test]
    fn the_scar_command_is_refused_through_its_cd() {
        refused(r#"cd @R/spira/run; ls -dt */ 2>/dev/null | head -5; grep -rl "spira-20261114T055931Z" . 2>/dev/null | head"#);
    }

    #[test]
    fn recursive_searches_of_the_run_dir_or_above_are_refused() {
        refused("grep -rn foo @R/spira/run");
        refused("rg foo @R/spira");
        refused("grep -R foo ~/spira");
        refused("grep -R foo $HOME/spira");
        refused("find @R/spira/run -name '*.log'");
        refused("cd @R/spira/run/worktree/sp-b && rg x");
        refused("bash -c 'grep -r x @R/spira/run'");
        refused("timeout 5 env A=b grep -rl x @R/spira/run/*.log");
        refused("grep --recursive x @R/spira/run");
        refused("grep -d recurse x @R/spira/run");
        refused("grep -e x -r @R/spira/run");
        refused("find @R/spira/run -maxdepth 2 -name x");
        refused("rg --files @R/spira/run");
    }

    #[test]
    fn the_grep_tool_and_a_recursive_glob_are_refused() {
        let r = verdict(json!({"tool_name": "Grep", "tool_input": {"pattern": "x", "path": run_dir()}})).unwrap();
        assert!(r.contains("work show"));
        assert!(verdict(json!({"tool_name": "Glob", "tool_input": {"pattern": "**/*.log", "path": run_dir()}})).is_some());
        assert!(verdict(json!({"tool_name": "Glob", "tool_input": {"pattern": at("@R/spira/run/**/*.log")}})).is_some());
        assert_eq!(verdict(json!({"tool_name": "Glob", "tool_input": {"pattern": "**/*.rs"}})), None);
        assert!(verdict(json!({"tool_name": "Glob", "cwd": run_dir(), "tool_input": {"pattern": "**/*.rs"}})).is_some());
        refused("find @R/spira/run/* -name x");
    }

    #[test]
    fn searches_inside_the_worktree_and_single_files_run() {
        allowed("grep -rn foo .");
        allowed("rg foo dwarf/src");
        allowed("find . -name x");
        allowed("grep -rn foo @R/spira/run/worktree/sp-a/dwarf");
        allowed("grep foo @R/spira/run/sp-x.log");
        allowed("find @R/spira/run -maxdepth 1 -name 'sp-x*'");
        allowed("grep -rn foo $(pwd)");
        allowed("cd dwarf && grep -rn foo .");
        allowed("grep -rn foo . 2>/dev/null | head");
    }

    #[test]
    fn prose_and_heredoc_bodies_are_inert() {
        allowed(r#"echo "grep -r x @R/spira/run""#);
        allowed("work note - <<'EOF'\nran grep -r x @R/spira/run\nEOF");
        allowed("cat <<EOF\ngrep -r x @R/spira/run\nEOF\nls");
        refused("bash <<'EOF'\ngrep -r x @R/spira/run\nEOF");
        refused("cat <<EOF >/dev/null\nx\nEOF\ngrep -r x @R/spira");
    }

    #[test]
    fn the_grep_tool_without_a_path_and_other_tools_pass() {
        assert_eq!(verdict(json!({"tool_name": "Grep", "tool_input": {"pattern": "x"}})), None);
        assert_eq!(verdict(json!({"tool_name": "Read", "tool_input": {"file_path": run_dir()}})), None);
        assert_eq!(verdict(json!({"tool_name": "Glob", "tool_input": {"pattern": "*.log", "path": run_dir()}})), None);
    }

    #[test]
    fn an_unreadable_payload_passes() {
        assert_eq!(verdict(json!({})), None);
        assert_eq!(verdict(json!({"tool_name": "Bash"})), None);
    }

    #[test]
    fn cd_forms_and_command_wrappers_are_followed() {
        refused("cd && grep -r x spira");
        refused("pushd @R/spira/run && rg x");
        refused("cd @R/spira/run && popd && grep -r x @R/spira/run");
        refused("nice -n 5 grep -r x @R/spira/run");
        refused("nohup command rg x @R/spira");
        refused("find -L @R/spira/run -name x");
        refused("find @R/spira/run \\( -name a -o -name b \\)");
        allowed("popd && grep -r x .");
        allowed("cd dwarf && cd .. && grep -r x .");
    }

    #[test]
    fn a_subshell_cd_is_taken_to_change_the_directory() {
        refused("(cd @R/spira/run && ls); grep -rn x .");
    }
}
