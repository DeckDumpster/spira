use crate::finding::{Class, Finding};
use crate::rules::{
    Rules, BD_GLOBAL_VALUE_FLAGS, CREDENTIAL_TOKENS, FORBIDDEN_BARE_VERBS,
    FORBIDDEN_READY_FLAGS, FORBIDDEN_UPDATE_FLAGS, ORACLE_SUBCOMMANDS, READ_VERBS,
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

    // Array-held verbs (sp-hyo5e): `bdq "${READY_ARGS[@]}"` names its verb in the array's
    // assignment, not at the call. Where that is unambiguous tree-wide, judge the call as if
    // the array were spelled out.
    let arrays = literal_arrays(&parsed);
    for call in &mut all_calls {
        expand_array_arg(call, &arrays);
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
    // Calls grouped by scope once: filtering every call per function definition was
    // quadratic, ~20 s over the harness tree at the gate's opt-level 0 (sp-ts2qr).
    let mut calls_by_scope: HashMap<&ScopeId, Vec<&CallSite>> = HashMap::new();
    for c in &all_calls {
        calls_by_scope.entry(&c.scope).or_default().push(c);
    }
    for (name, defs) in &func_defs {
        for file_idx in defs {
            let scope = ScopeId {
                file_idx: *file_idx,
                function: Some(name.clone()),
            };
            let body_calls: &[&CallSite] = calls_by_scope.get(&scope).map(|v| v.as_slice()).unwrap_or(&[]);
            // Exactly one bd/bdq call, and it forwards "$@". Other calls in the body are
            // filters around it (`bdjson() { bdq "$@" --json | json_only; }`): they cannot
            // change the verb, so the verb is still the call site's to name (sp-hyo5e).
            let bd_calls: Vec<&&CallSite> =
                body_calls.iter().filter(|c| c.callee == "bd" || c.callee == "bdq").collect();
            if let [call] = bd_calls.as_slice() {
                if forwards_all_args(call) {
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

        // The verb follows bd's leading global flags (`bd -C "$DB" close …`), not argv[1].
        let verb_at = verb_index(&call.args);
        let Some(first) = call.args.get(verb_at) else {
            continue;
        };
        // A forwarder's own `bdq "$@"`: its verb is judged at each of its call sites (above),
        // so the body itself is not an unresolvable verb.
        let forwarder_body = !is_forwarded
            && call.scope.function.as_ref().is_some_and(|f| forward_target.contains_key(f))
            && forwards_all_args(call);
        match first {
            _ if forwarder_body => {}
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
                } else if let Some(flags) = match verb.as_str() {
                    "update" => Some(FORBIDDEN_UPDATE_FLAGS),
                    "ready" => Some(FORBIDDEN_READY_FLAGS),
                    _ => None,
                } {
                    let flag = call.args[verb_at + 1..]
                        .iter()
                        .filter_map(|a| a.literal())
                        .find(|a| flags.contains(a));
                    flag.map(|f| format!("{verb} {f}"))
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
    // functions, never bd/bdq itself, and every call site is a finding.
    // The same reach through the binary: `landing-pass mark|state|landed|cited-commit|
    // close-on-land` (sp-ts2qr), by any path to it.
    for call in &all_calls {
        let oracle = oracle_subcommand(call);
        if oracle.is_none() && !matches!(call.callee.as_str(), "land_mark" | "landed" | "landed_sha") {
            continue;
        }
        let rel = &parsed[call.scope.file_idx].rel_path;
        let (callee, detail) = match oracle {
            Some(verb) => (
                format!("landing-pass {verb}"),
                format!(
                    "landing-pass {verb} is the landstate ledger / landed oracle; read it through spira-lc show/list instead"
                ),
            ),
            None => (
                call.callee.clone(),
                format!(
                    "{} is a direct call into the landstate ledger; read it through spira-lc show/list instead",
                    call.callee
                ),
            ),
        };
        findings.push(Finding {
            class: Class::LandstateCall,
            file: rel.clone(),
            line: call.line,
            function: call.scope.function.clone(),
            callee: Some(callee),
            detail,
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
        find_landstate_reads(pf.tree.root_node(), pf.text.as_bytes(), &pf.rel_path, &mut findings);
    }

    Ok(ShellScan { findings })
}

/// The oracle subcommand `call` invokes, when its command is landing-pass (a bare name or any
/// path ending in it, `$VAR/bin/landing-pass` included) and its first argument is literally
/// one of [`ORACLE_SUBCOMMANDS`]. A prefix `env [VAR=…]… `, `command ` or `exec ` is looked
/// through, as aeon's `env SPIRA_RUN=… landing-pass mark …` was written.
fn oracle_subcommand(call: &CallSite) -> Option<&'static str> {
    let words: Vec<String> = std::iter::once(strip_quotes(&call.callee))
        .chain(call.args.iter().map(|a| match a {
            Arg::Literal(s) | Arg::Dynamic(s) => s.clone(),
        }))
        .collect();
    let mut i = 0;
    while i < words.len() && matches!(words[i].as_str(), "env" | "command" | "exec") {
        i += 1;
        while i < words.len() && (words[i].starts_with('-') || words[i].contains('=')) {
            i += 1;
        }
    }
    if words.get(i)?.rsplit('/').next() != Some("landing-pass") {
        return None;
    }
    // The verb must be a literal: a dynamic one is not an oracle call this pass can name.
    // words[k] is args[k - 1], so the word after the command (words[i + 1]) is args[i].
    let verb = call.args.get(i)?.literal()?;
    ORACLE_SUBCOMMANDS.iter().copied().find(|v| *v == verb)
}

/// The call hands its caller's whole argv to bd in the verb's position (`bdq "$@" …`,
/// `bd -C "$DB" "$@"`), so the verb is whatever the caller passes. Only flags and expansions
/// may precede it: `bdq show "$@"` fixes its own verb and is judged where it is written.
fn forwards_all_args(call: &CallSite) -> bool {
    let Some(at) = call.args.iter().position(|a| matches!(a, Arg::Dynamic(t) if t == "$@" || t == "$*")) else {
        return false;
    };
    call.args[..at].iter().all(|a| a.literal().is_none_or(|l| l.starts_with('-')))
}

/// Every array named in the scanned tree whose contents are known statically: each plain
/// assignment `NAME=(w …)` (global or `local`/`declare`) spells the same literal first word,
/// and every other way it is written is an append `NAME+=(…)`. Anything else — an empty or
/// dynamic first word, two assignments that disagree, an element write `NAME[i]=…`, or a fill
/// by `mapfile`/`readarray`/`read -a` — leaves the name out, so its expansion stays dynamic.
/// Names are tree-wide, not per scope: a common name assigned two ways is simply unresolved,
/// which costs completeness, never soundness. The value is every element of every assignment
/// and append, the base's first, so a forbidden `update` flag held in an append is still seen.
fn literal_arrays(parsed: &[ParsedFile]) -> HashMap<String, Vec<Arg>> {
    #[derive(Default)]
    struct Seen {
        bases: Vec<Vec<Arg>>,
        appends: Vec<Arg>,
        poisoned: bool,
    }
    fn walk(node: Node, source: &[u8], seen: &mut HashMap<String, Seen>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "variable_assignment" => {
                    if let (Some(name), Some(value)) = (child.child_by_field_name("name"), child.child_by_field_name("value")) {
                        if name.kind() == "subscript" {
                            if let Some(n) = name.child_by_field_name("name") {
                                seen.entry(text_of(n, source)).or_default().poisoned = true;
                            }
                        } else if value.kind() == "array" {
                            let mut c = value.walk();
                            let elems: Vec<Arg> = value.named_children(&mut c).map(|e| classify_arg(e, source)).collect();
                            let mut c2 = child.walk();
                            let append = child.children(&mut c2).any(|t| t.kind() == "+=");
                            let entry = seen.entry(text_of(name, source)).or_default();
                            if append {
                                entry.appends.extend(elems);
                            } else {
                                entry.bases.push(elems);
                            }
                        } else {
                            seen.entry(text_of(name, source)).or_default().poisoned = true;
                        }
                    }
                }
                "command" => {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        if matches!(text_of(name_node, source).as_str(), "mapfile" | "readarray" | "read") {
                            let mut c = child.walk();
                            for a in child.children_by_field_name("argument", &mut c) {
                                seen.entry(strip_quotes(&text_of(a, source))).or_default().poisoned = true;
                            }
                        }
                    }
                }
                _ => {}
            }
            walk(child, source, seen);
        }
    }
    let mut seen: HashMap<String, Seen> = HashMap::new();
    for pf in parsed {
        walk(pf.tree.root_node(), pf.text.as_bytes(), &mut seen);
    }
    seen.into_iter()
        .filter_map(|(name, s)| {
            if s.poisoned || s.bases.is_empty() {
                return None;
            }
            let first = s.bases[0].first()?.literal()?.to_string();
            if !s.bases.iter().all(|b| b.first().and_then(Arg::literal) == Some(first.as_str())) {
                return None;
            }
            let mut all: Vec<Arg> = s.bases.into_iter().flatten().collect();
            all.extend(s.appends);
            Some((name, all))
        })
        .collect()
}

/// `"${NAME[@]}"` / `${NAME[@]}` in a call's verb position (its first argument, or the first
/// after bd's leading global flags), NAME a [`literal_arrays`] entry: the array's words stand
/// in its place. An array of global flags is expanded and the verb looked for again after it.
fn expand_array_arg(call: &mut CallSite, arrays: &HashMap<String, Vec<Arg>>) {
    for _ in 0..8 {
        let at = verb_index(&call.args);
        let Some(Arg::Dynamic(word)) = call.args.get(at) else {
            return;
        };
        let Some(name) = word.strip_prefix("${").and_then(|r| r.strip_suffix("[@]}")) else {
            return;
        };
        let Some(words) = arrays.get(name) else {
            return;
        };
        call.args.splice(at..at + 1, words.iter().cloned());
    }
}

/// Where a bd/bdq call's verb sits: past its leading global flags (`-C <dir>`, `--db <path>`,
/// `--actor <name>`, `--json`, `--db=…`, …; see [`BD_GLOBAL_VALUE_FLAGS`]). Before sp-voip5
/// the verb was argv[1], so `bd -C "$DB" close …` and `bdq --db … update --status …` went
/// unseen. An unknown flag is taken as boolean: at worst its value is read as the verb, and a
/// non-verb word is never a finding. Past the end when the call has no verb (`bd --version`).
fn verb_index(args: &[Arg]) -> usize {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let (Arg::Literal(w) | Arg::Dynamic(w)) = arg;
        if !w.starts_with('-') {
            break;
        }
        i += if BD_GLOBAL_VALUE_FLAGS.contains(&w.as_str()) { 2 } else { 1 };
    }
    i
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
/// same file, and promote it to `in_conditional` if so. The file's conditional variables are
/// collected in one walk — one walk per captured call was most of a gate's run (sp-ts2qr).
fn resolve_captured_conditionals(root: Node, source: &[u8], calls: &mut [CallSite]) {
    if !calls.iter().any(|c| c.captured_into.is_some()) {
        return;
    }
    let mut cond_vars: HashSet<String> = HashSet::new();
    vars_used_in_conditionals(root, source, false, &mut cond_vars);
    for call in calls.iter_mut() {
        if call.captured_into.as_ref().is_some_and(|v| cond_vars.contains(v)) {
            call.in_conditional = true;
        }
    }
}

fn vars_used_in_conditionals(node: Node, source: &[u8], in_cond: bool, out: &mut HashSet<String>) {
    let now_cond = in_cond || node.kind() == "test_command" || node.kind() == "case_statement";
    if now_cond && matches!(node.kind(), "variable_name" | "special_variable_name") {
        out.insert(node.utf8_text(source).unwrap_or_default().to_string());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        vars_used_in_conditionals(child, source, now_cond, out);
    }
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
