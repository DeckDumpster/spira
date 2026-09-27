use crate::finding::{Class, Finding};
use crate::rules::{
    landstate_path_allowed, Rules, CREDENTIAL_TOKENS, FORBIDDEN_BARE_VERBS, FORBIDDEN_UPDATE_FLAGS,
    READ_VERBS,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser, Tree};

struct ParsedFile {
    rel_path: String,
    dir: PathBuf,
    text: String,
    tree: Tree,
}

#[derive(Clone, Debug)]
enum Arg {
    Literal(String),
    Dynamic(String),
}

impl Arg {
    fn literal(&self) -> Option<&str> {
        match self {
            Arg::Literal(s) => Some(s.as_str()),
            Arg::Dynamic(_) => None,
        }
    }
}

/// A scope is either a named function or the top level of one file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ScopeId {
    file_idx: usize,
    function: Option<String>,
}

struct CallSite {
    scope: ScopeId,
    callee: String,
    args: Vec<Arg>,
    line: usize,
    /// set once, directly during the walk, when this call's own output is captured by a
    /// substitution that is itself compared inside a test/case in the same scope.
    in_conditional: bool,
    /// set when this call's output is captured into a variable instead; resolved to
    /// `in_conditional` afterwards by a second, scope-local pass.
    captured_into: Option<String>,
}

pub struct ShellScan {
    findings: Vec<Finding>,
}

impl ShellScan {
    pub fn into_findings(self) -> Vec<Finding> {
        self.findings
    }
}

pub fn scan(files: &[PathBuf], root: &Path, rules: &Rules) -> Result<ShellScan, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::language())
        .map_err(|e| format!("loading bash grammar: {e}"))?;

    let mut parsed = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let tree = parser
            .parse(&text, None)
            .ok_or_else(|| format!("parsing {}: tree-sitter gave up", path.display()))?;
        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        parsed.push(ParsedFile {
            rel_path,
            dir,
            text,
            tree,
        });
    }

    let mut findings = Vec::new();

    // Pass 1: per file, collect function definitions, source edges, and call sites.
    let mut func_defs: HashMap<String, Vec<usize>> = HashMap::new();
    let mut source_edges: Vec<(usize, usize)> = Vec::new();
    let mut all_calls: Vec<CallSite> = Vec::new();

    for (file_idx, pf) in parsed.iter().enumerate() {
        let source = pf.text.as_bytes();
        let root_node = pf.tree.root_node();

        collect_funcs(root_node, source, file_idx, &mut func_defs);

        let mut calls = Vec::new();
        walk_scope(
            root_node,
            source,
            &ScopeId {
                file_idx,
                function: None,
            },
            false,
            &mut calls,
        );
        resolve_captured_conditionals(root_node, source, &mut calls);

        for c in &calls {
            if c.callee == "source" || c.callee == "." {
                if let Some(rendered) = c.args.first().and_then(|a| render_source_arg(a, &pf.dir)) {
                    if let Some(target_idx) = resolve_source_target(&rendered, &pf.dir, &parsed, root) {
                        source_edges.push((file_idx, target_idx));
                    }
                }
            }
        }
        all_calls.extend(calls);
    }

    // Reachability: for each file, the set of files whose top-level definitions are visible
    // to it once shell execution has inlined every transitively-sourced file.
    let reachable = reachable_sets(parsed.len(), &source_edges);

    // Resolve every call site's callee to a concrete function definition, if any is visible.
    let resolve = |file_idx: usize, name: &str| -> bool {
        func_defs
            .get(name)
            .map(|defs| defs.iter().any(|f| reachable[file_idx].contains(f)))
            .unwrap_or(false)
    };

    // Forwarding wrappers: a function whose entire body is one call passing `"$@"`/`$@`
    // straight to something that (transitively) resolves to bd/bdq. The verb lives at the
    // call site, not in the wrapper, so it can only be judged there.
    let mut forward_target: HashMap<String, String> = HashMap::new(); // wrapper name -> bd|bdq
    for (name, defs) in &func_defs {
        for file_idx in defs {
            let scope = ScopeId {
                file_idx: *file_idx,
                function: Some(name.clone()),
            };
            let body_calls: Vec<&CallSite> =
                all_calls.iter().filter(|c| c.scope == scope).collect();
            if body_calls.len() == 1 {
                let call = body_calls[0];
                let forwards_all_args = call.args.iter().any(|a| {
                    matches!(a, Arg::Dynamic(t) if t == "$@" || t == "\"$@\"" || t == "$*" || t == "\"$*\"")
                });
                if forwards_all_args && (call.callee == "bd" || call.callee == "bdq") {
                    forward_target.insert(name.clone(), call.callee.clone());
                }
            }
        }
    }
    // Chase forwarding-through-forwarding (wrapper of a wrapper) to a fixed point.
    loop {
        let mut changed = false;
        let snapshot: Vec<(String, String)> = forward_target
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (wrapper, target) in &snapshot {
            if let Some(next) = forward_target.get(target).cloned() {
                if &next != target {
                    forward_target.insert(wrapper.clone(), next);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Direct sinks: a call site whose callee resolves to bd/bdq (directly, or through a
    // forwarding wrapper) with a literal forbidden verb, or an unresolvable dynamic verb.
    // Track which (file, function) scopes contain a sink, for the wrapper-reachability pass.
    let mut sink_scopes: HashSet<ScopeId> = HashSet::new();

    for call in &all_calls {
        let ultimate = if call.callee == "bd" || call.callee == "bdq" {
            Some(call.callee.clone())
        } else {
            forward_target.get(&call.callee).cloned()
        };
        let Some(ultimate) = ultimate else {
            continue;
        };
        let is_forwarded = ultimate != call.callee;
        let class = if is_forwarded {
            Class::WrapperWrite
        } else {
            Class::DirectWrite
        };

        let Some(first) = call.args.first() else {
            continue;
        };
        match first {
            Arg::Dynamic(_) => {
                findings.push(Finding {
                    class: Class::DynamicVerb,
                    file: parsed[call.scope.file_idx].rel_path.clone(),
                    line: call.line,
                    function: call.scope.function.clone(),
                    callee: Some(call.callee.clone()),
                    detail: format!(
                        "{} invoked with a verb that cannot be resolved statically",
                        call.callee
                    ),
                });
            }
            Arg::Literal(verb) => {
                let flagged = if FORBIDDEN_BARE_VERBS.contains(&verb.as_str()) {
                    Some(verb.clone())
                } else if verb == "update" {
                    let flag = call
                        .args
                        .iter()
                        .skip(1)
                        .filter_map(|a| a.literal())
                        .find(|a| FORBIDDEN_UPDATE_FLAGS.contains(a));
                    flag.map(|f| format!("update {f}"))
                } else {
                    None
                };
                if let Some(verb) = flagged {
                    if !is_forwarded {
                        sink_scopes.insert(call.scope.clone());
                    }
                    findings.push(Finding {
                        class,
                        file: parsed[call.scope.file_idx].rel_path.clone(),
                        line: call.line,
                        function: call.scope.function.clone(),
                        callee: Some(call.callee.clone()),
                        detail: format!("{ultimate} {verb} on a work bead"),
                    });
                }
            }
        }

        // A lifecycle read (show/list/status) whose output feeds a decision.
        if !is_forwarded {
            if let Arg::Literal(verb) = first {
                if READ_VERBS.contains(&verb.as_str()) && call.in_conditional {
                    findings.push(Finding {
                        class: Class::LifecycleRead,
                        file: parsed[call.scope.file_idx].rel_path.clone(),
                        line: call.line,
                        function: call.scope.function.clone(),
                        callee: Some(call.callee.clone()),
                        detail: format!("{ultimate} {verb} read used in a conditional"),
                    });
                }
            }
        }
    }

    // Reverse reachability: every scope that (transitively, via non-forwarding wrappers)
    // calls a scope in sink_scopes gets a wrapper-write finding at its own call site.
    let calls_scope_to_function: Vec<(&ScopeId, &str, &CallSite)> = all_calls
        .iter()
        .filter_map(|c| {
            if resolve(c.scope.file_idx, &c.callee) && !forward_target.contains_key(&c.callee) {
                Some((&c.scope, c.callee.as_str(), c))
            } else {
                None
            }
        })
        .collect();

    let mut already_flagged: HashSet<(usize, usize)> = HashSet::new();
    let mut frontier: HashSet<String> = HashSet::new();
    for scope in &sink_scopes {
        if let Some(name) = &scope.function {
            frontier.insert(name.clone());
        }
    }
    // Fixed point: grow the set of function names known to (transitively) reach a sink.
    loop {
        let mut grew = false;
        for (scope, callee, _call) in &calls_scope_to_function {
            if let Some(caller_fn) = &scope.function {
                if frontier.contains(*callee) && !frontier.contains(caller_fn) {
                    frontier.insert(caller_fn.clone());
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    for (scope, callee, call) in &calls_scope_to_function {
        if frontier.contains(*callee) {
            let key = (call.line, scope.file_idx);
            if already_flagged.insert(key) {
                findings.push(Finding {
                    class: Class::WrapperWrite,
                    file: parsed[scope.file_idx].rel_path.clone(),
                    line: call.line,
                    function: scope.function.clone(),
                    callee: Some(callee.to_string()),
                    detail: format!("reaches a lifecycle write through {callee}"),
                });
            }
        }
    }

    // Direct calls into the landstate ledger: land_mark/landed/landed_sha, the shell half of
    // this bead's read barrier. Independent of the bd/bdq call graph above — these are lib.sh
    // functions, never bd/bdq itself, and every call site outside the allow-list is a finding.
    for call in &all_calls {
        if !matches!(call.callee.as_str(), "land_mark" | "landed" | "landed_sha") {
            continue;
        }
        let rel = &parsed[call.scope.file_idx].rel_path;
        if landstate_path_allowed(rel) {
            continue;
        }
        findings.push(Finding {
            class: Class::LandstateCall,
            file: rel.clone(),
            line: call.line,
            function: call.scope.function.clone(),
            callee: Some(call.callee.clone()),
            detail: format!(
                "{} is a direct call into the landstate ledger; read it through spira-lc show/list instead",
                call.callee
            ),
        });
    }

    // Credential / DSN references, and retired-label / deleted-path checks: independent of
    // the call graph, so a single textual+structural pass per file is enough.
    for pf in &parsed {
        let lower = pf.text.to_ascii_lowercase();
        for token in CREDENTIAL_TOKENS {
            if let Some(byte_off) = lower.find(&token.to_ascii_lowercase()) {
                let line = pf.text[..byte_off].matches('\n').count() + 1;
                findings.push(Finding {
                    class: Class::Credential,
                    file: pf.rel_path.clone(),
                    line,
                    function: None,
                    callee: None,
                    detail: format!("references the lifecycle machine's own credential surface ({token})"),
                });
            }
        }
        for label in &rules.retired_labels {
            if let Some(byte_off) = pf.text.find(label.as_str()) {
                let line = pf.text[..byte_off].matches('\n').count() + 1;
                findings.push(Finding {
                    class: Class::RetiredLabel,
                    file: pf.rel_path.clone(),
                    line,
                    function: None,
                    callee: None,
                    detail: format!("names retired lifecycle label {label}"),
                });
            }
        }
        find_deleted_path_writes(
            pf.tree.root_node(),
            pf.text.as_bytes(),
            &pf.rel_path,
            &rules.deleted_state_paths,
            &mut findings,
        );
        if !landstate_path_allowed(&pf.rel_path) {
            find_landstate_reads(pf.tree.root_node(), pf.text.as_bytes(), &pf.rel_path, &mut findings);
        }
    }

    Ok(ShellScan { findings })
}

fn collect_funcs(
    node: Node,
    source: &[u8],
    file_idx: usize,
    out: &mut HashMap<String, Vec<usize>>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_definition" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name = text_of(name_node, source);
                out.entry(name).or_default().push(file_idx);
            }
        }
        collect_funcs(child, source, file_idx, out);
    }
}

/// Walks one scope (a function body, or the whole file for the top level), recording every
/// command invocation. Descends into nested function definitions as their own new scope.
fn walk_scope(
    node: Node,
    source: &[u8],
    scope: &ScopeId,
    in_conditional: bool,
    out: &mut Vec<CallSite>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let child_conditional =
            in_conditional || child.kind() == "test_command" || child.kind() == "case_statement";
        match child.kind() {
            "function_definition" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = text_of(name_node, source);
                    if let Some(body) = child.child_by_field_name("body") {
                        walk_scope(
                            body,
                            source,
                            &ScopeId {
                                file_idx: scope.file_idx,
                                function: Some(name),
                            },
                            false,
                            out,
                        );
                    }
                }
            }
            "command" => {
                if let Some(call) = command_call(child, source, scope, child_conditional) {
                    out.push(call);
                }
                walk_scope(child, source, scope, child_conditional, out);
            }
            _ => {
                walk_scope(child, source, scope, child_conditional, out);
            }
        }
    }
}

fn command_call(
    node: Node,
    source: &[u8],
    scope: &ScopeId,
    in_conditional: bool,
) -> Option<CallSite> {
    let name_node = node.child_by_field_name("name")?;
    let callee = text_of(name_node, source);
    let mut args = Vec::new();
    let mut cursor = node.walk();
    for arg_node in node.children_by_field_name("argument", &mut cursor) {
        args.push(classify_arg(arg_node, source));
    }

    let captured_into = capture_target(node, source);
    let directly_conditional = in_conditional && captured_into.is_none() && is_captured(node);

    Some(CallSite {
        scope: scope.clone(),
        callee,
        args,
        line: node.start_position().row + 1,
        in_conditional: directly_conditional,
        captured_into,
    })
}

fn classify_arg(node: Node, source: &[u8]) -> Arg {
    let raw = text_of(node, source);
    let stripped = strip_quotes(&raw);
    if stripped.contains('$') || stripped.contains('`') {
        Arg::Dynamic(stripped)
    } else {
        Arg::Literal(stripped)
    }
}

fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn text_of(node: Node, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or_default().to_string()
}

/// True when `node` sits directly inside a `command_substitution` (i.e. its stdout is being
/// captured for immediate use, such as `[ "$(bd show ...)" = closed ]`).
fn is_captured(node: Node) -> bool {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if p.kind() == "command_substitution" {
            return true;
        }
        // Stop at the nearest statement boundary; a substitution used somewhere else in the
        // same compound statement is a different call entirely.
        if matches!(
            p.kind(),
            "compound_statement" | "program" | "do_group" | "case_item"
        ) {
            return false;
        }
        cur = p.parent();
    }
    false
}

/// When this call's substituted output is assigned to a variable (`x=$(bd show ...)`),
/// returns that variable's name so its later use can be checked for a conditional context.
fn capture_target(node: Node, source: &[u8]) -> Option<String> {
    if !is_captured(node) {
        return None;
    }
    let mut cur = node.parent();
    while let Some(p) = cur {
        if p.kind() == "variable_assignment" {
            let name_node = p.child_by_field_name("name")?;
            return Some(text_of(name_node, source));
        }
        if matches!(p.kind(), "test_command" | "case_statement") {
            // Already directly inside a conditional: no variable indirection needed.
            return None;
        }
        cur = p.parent();
    }
    None
}

/// Second pass over a whole file: for every call that captured its output into a variable,
/// look for that variable being read inside a `test_command`/`case_statement` anywhere in the
/// same file, and promote it to `in_conditional` if so.
fn resolve_captured_conditionals(root: Node, source: &[u8], calls: &mut [CallSite]) {
    for call in calls.iter_mut() {
        if let Some(var) = call.captured_into.clone() {
            if var_used_in_conditional(root, source, &var) {
                call.in_conditional = true;
            }
        }
    }
}

fn var_used_in_conditional(node: Node, source: &[u8], var: &str) -> bool {
    fn walk(node: Node, source: &[u8], var: &str, in_cond: bool) -> bool {
        let now_cond = in_cond || node.kind() == "test_command" || node.kind() == "case_statement";
        if now_cond
            && matches!(node.kind(), "variable_name" | "special_variable_name")
            && node.utf8_text(source).unwrap_or_default() == var
        {
            return true;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if walk(child, source, var, now_cond) {
                return true;
            }
        }
        false
    }
    walk(node, source, var, false)
}

/// A source/`.` target is usually written against the caller's own directory
/// (`. "$HERE/lib.sh"`, following the convention documented in this repo's own test suites).
/// `$HERE`/`${HERE}` is the one expansion resolved; anything else dynamic is left
/// unresolved rather than guessed at, which only costs completeness, not soundness — a
/// missed edge means a wrapper isn't traced across that particular source, not a false clean.
fn render_source_arg(arg: &Arg, file_dir: &Path) -> Option<String> {
    match arg {
        Arg::Literal(s) => Some(s.clone()),
        Arg::Dynamic(s) => {
            let here = file_dir.to_string_lossy();
            let rendered = s.replace("${HERE}", &here).replace("$HERE", &here);
            if rendered.contains('$') || rendered.contains('`') {
                None
            } else {
                Some(rendered)
            }
        }
    }
}

fn resolve_source_target(
    literal: &str,
    file_dir: &Path,
    parsed: &[ParsedFile],
    root: &Path,
) -> Option<usize> {
    let candidate = if literal.starts_with('/') {
        PathBuf::from(literal)
    } else {
        file_dir.join(literal)
    };
    let normalized = normalize(&candidate);
    parsed.iter().position(|pf| {
        let full = root.join(&pf.rel_path);
        normalize(&full) == normalized
    })
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn reachable_sets(n: usize, edges: &[(usize, usize)]) -> Vec<HashSet<usize>> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (from, to) in edges {
        adj[*from].push(*to);
    }
    let mut out = Vec::with_capacity(n);
    for start in 0..n {
        let mut seen = HashSet::new();
        seen.insert(start);
        let mut stack = vec![start];
        while let Some(cur) = stack.pop() {
            for &next in &adj[cur] {
                if seen.insert(next) {
                    stack.push(next);
                }
            }
        }
        out.push(seen);
    }
    out
}

fn find_deleted_path_writes(
    node: Node,
    source: &[u8],
    rel_path: &str,
    deleted_paths: &[String],
    out: &mut Vec<Finding>,
) {
    if deleted_paths.is_empty() {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "redirected_statement" {
            if let Some(redirect) = child.child_by_field_name("redirect") {
                if let Some(dest) = redirect.child_by_field_name("destination") {
                    let raw = text_of(dest, source);
                    let dest_text = strip_quotes(&raw);
                    if deleted_paths.iter().any(|p| dest_text.starts_with(p.as_str())) {
                        let line = child.start_position().row + 1;
                        out.push(Finding {
                            class: Class::DeletedPath,
                            file: rel_path.to_string(),
                            line,
                            function: None,
                            callee: None,
                            detail: format!("writes under deleted state path {dest_text}"),
                        });
                    }
                }
            }
        }
        find_deleted_path_writes(child, source, rel_path, deleted_paths, out);
    }
}

/// Commands that read a landstate file's contents rather than merely test for its presence —
/// the shell shapes named in this bead: `ls`/`cat`/`find` given a landstate path, an input
/// redirect (`read ... < "$LANDSTATE/..."`) reading one, or a `for` loop globbing one.
const LANDSTATE_READ_COMMANDS: &[&str] = &["ls", "cat", "find"];

fn mentions_landstate(text: &str) -> bool {
    text.to_ascii_lowercase().contains("landstate")
}

fn find_landstate_reads(node: Node, source: &[u8], rel_path: &str, out: &mut Vec<Finding>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "redirected_statement" => {
                if redirect_reads_landstate(child, source) {
                    push_landstate_path(out, rel_path, child.start_position().row + 1, "read");
                }
            }
            "command" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = text_of(name_node, source);
                    if LANDSTATE_READ_COMMANDS.contains(&name.as_str())
                        && command_args_mention_landstate(child, source)
                    {
                        push_landstate_path(out, rel_path, child.start_position().row + 1, &name);
                    }
                }
            }
            "for_statement" => {
                if for_value_mentions_landstate_glob(child, source) {
                    push_landstate_path(out, rel_path, child.start_position().row + 1, "glob");
                }
            }
            _ => {}
        }
        find_landstate_reads(child, source, rel_path, out);
    }
}

fn push_landstate_path(out: &mut Vec<Finding>, rel_path: &str, line: usize, mechanism: &str) {
    out.push(Finding {
        class: Class::LandstatePath,
        file: rel_path.to_string(),
        line,
        function: None,
        callee: None,
        detail: format!(
            "{mechanism} reads the landstate ledger directly; read it through spira-lc show/list instead"
        ),
    });
}

fn command_args_mention_landstate(node: Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    let args: Vec<Node> = node.children_by_field_name("argument", &mut cursor).collect();
    args.iter().any(|arg| mentions_landstate(&text_of(*arg, source)))
}

fn redirect_reads_landstate(node: Node, source: &[u8]) -> bool {
    let Some(redirect) = node.child_by_field_name("redirect") else {
        return false;
    };
    if redirect.kind() != "file_redirect" {
        return false;
    }
    let mut cursor = redirect.walk();
    if !redirect.children(&mut cursor).any(|c| c.kind() == "<") {
        return false;
    }
    let Some(dest) = redirect.child_by_field_name("destination") else {
        return false;
    };
    mentions_landstate(&text_of(dest, source))
}

fn for_value_mentions_landstate_glob(node: Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    let values: Vec<Node> = node.children_by_field_name("value", &mut cursor).collect();
    values.iter().any(|v| {
        let text = text_of(*v, source);
        mentions_landstate(&text) && text.contains('*')
    })
}
