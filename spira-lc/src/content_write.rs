//! The content door's writes as one SQL transaction on the beads database, so lc-serve applies
//! them without forking `bd`: labels, notes, comments, metadata, priority, status, assignee,
//! description, create, close, dependency edges and supersede. Each verb makes the row changes
//! bd makes for it: the row itself, `updated_at`, and the `events` row bd files (none for a
//! comment). A write whose preconditions bd would refuse, or that the port does not cover, is
//! left to bd, which refuses it in its own words.

use crate::rows::escape;

mod ids;
mod verbs;
pub use verbs::delete;

const ACTOR: &str = "spira-lc";

/// The statuses a create or an update sets directly: a close is its own verb, with its own policy.
const SETTABLE: [&str; 6] = ["open", "in_progress", "blocked", "deferred", "pinned", "hooked"];
const TYPES: [&str; 11] = ["bug", "feature", "task", "epic", "chore", "decision", "message", "molecule", "gate", "spike", "story"];
const BLOCKING: [&str; 3] = ["blocks", "conditional-blocks", "parent-child"];

/// What a create prints: the id (`--silent`), the row (`--json`), or the sentence.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Id,
    Json,
    Text,
}

#[derive(Debug, Clone, PartialEq)]
struct Create {
    title: String,
    issue_type: String,
    status: String,
    priority: u8,
    labels: Vec<String>,
    parent: Option<String>,
    deps: Vec<(String, String)>,
    description: String,
    external_ref: String,
    assignee: String,
    id: Option<String>,
    inherit_labels: bool,
    shape: Shape,
}

#[derive(Debug, Clone, PartialEq)]
enum Op {
    LabelAdd(String),
    LabelRemove(String),
    Note(String),
    /// A note holding drafts: `merged` replaces the notes if they are still `read`, else `text` is appended.
    DraftNote { read: String, merged: String, text: String },
    Comment(String),
    Metadata(String, String),
    Priority(u8),
    Status(String),
    Assignee(String),
    Description(String),
    Create(Box<Create>),
    Close { reason: String, force: bool },
    DepAdd { on: String, kind: String },
    DepRemove { on: String },
    DepRelate { other: String },
    Supersede { with: String },
    Duplicate { of: String },
    SetState { dimension: String, value: String },
    GateResolve,
    RecomputeBlocked,
    GateCheck { kind: String },
    KvSet { key: String, value: String },
    KvClear { key: String },
    Remember { key: String, text: String },
    Forget { key: String },
}

impl Op {
    /// A verb on the store itself, not on one bead's row.
    fn is_standalone(&self) -> bool {
        matches!(self, Op::RecomputeBlocked | Op::GateCheck { .. } | Op::KvSet { .. } | Op::KvClear { .. } | Op::Remember { .. } | Op::Forget { .. })
    }

    fn is_column(&self) -> bool {
        matches!(self, Op::Status(_) | Op::Assignee(_) | Op::Description(_))
    }

    /// Whether bd rewrites the issue row (`updated_at`, `row_lock`) for it.
    fn touches_row(&self) -> bool {
        matches!(self, Op::Note(_) | Op::DraftNote { .. } | Op::Metadata(..) | Op::Priority(_))
    }
}

/// A door write lc-serve applies itself; `argv` and `body` are what a store with no writer
/// configured is refused by.
#[derive(Debug, Clone, PartialEq)]
pub struct Mutation {
    pub argv: Vec<String>,
    pub body: Option<String>,
    id: String,
    ops: Vec<Op>,
    forced: bool,
}

/// What the store has, as far as the statements need to know it.
#[derive(Debug, Clone, Copy)]
pub struct Env {
    pub leases: bool,
    pub local_metadata: bool,
}

impl Default for Env {
    fn default() -> Env {
        Env { leases: true, local_metadata: true }
    }
}

impl Mutation {
    /// The mutation `args` (as `guard` left them) and `body` are, when they are exactly one of
    /// the verbs served here.
    pub fn of(args: &[String], body: Option<&str>) -> Option<Mutation> {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let (id, ops) = match a.as_slice() {
            ["label", "add", id, l] if body.is_none() => (*id, vec![Op::LabelAdd((*l).into())]),
            ["label", "remove", id, l] if body.is_none() => (*id, vec![Op::LabelRemove((*l).into())]),
            ["note", id, "--stdin"] => (*id, vec![Op::Note(body?.trim_end_matches('\n').into())]),
            ["note", id, t] if body.is_none() && *t != "--stdin" => (*id, vec![Op::Note((*t).into())]),
            ["comments", "add", id, t] if body.is_none() => (*id, vec![Op::Comment((*t).into())]),
            ["update", id, flags @ ..] if !flags.is_empty() => (*id, update_ops(flags, body)?),
            ["create", rest @ ..] => ("", vec![Op::Create(Box::new(create_spec(rest, body)?))]),
            ["remember", "--key", k] => ("", vec![Op::Remember { key: (*k).into(), text: body?.into() }]),
            _ if body.is_some() => return None,
            ["set-state", id, kv] => {
                let (dimension, value) = kv.split_once('=').filter(|(d, v)| !d.is_empty() && !v.is_empty())?;
                (*id, vec![Op::SetState { dimension: dimension.into(), value: value.into() }])
            }
            ["duplicate", id, "--of", of] => (*id, vec![Op::Duplicate { of: (*of).into() }]),
            ["gate", "resolve", id] => (*id, vec![Op::GateResolve]),
            ["recompute-blocked"] => ("", vec![Op::RecomputeBlocked]),
            ["gate", "check", "--type", kind] => ("", vec![Op::GateCheck { kind: (*kind).into() }]),
            ["gate", "check", flag] if flag.starts_with("--type=") => ("", vec![Op::GateCheck { kind: flag["--type=".len()..].into() }]),
            ["kv", "set", key, value] => ("", vec![Op::KvSet { key: (*key).into(), value: (*value).into() }]),
            ["kv", "clear", key] => ("", vec![Op::KvClear { key: (*key).into() }]),
            ["remember", "--key", key, text] => ("", vec![Op::Remember { key: (*key).into(), text: (*text).into() }]),
            ["forget", key] => ("", vec![Op::Forget { key: (*key).into() }]),
            ["close", id] => (*id, vec![Op::Close { reason: String::new(), force: false }]),
            ["close", id, "--force"] => (*id, vec![Op::Close { reason: String::new(), force: true }]),
            ["close", id, "--reason", r] if !r.is_empty() => (*id, vec![Op::Close { reason: (*r).into(), force: false }]),
            ["close", id, "--force", "--reason", r] | ["close", id, "--reason", r, "--force"] if !r.is_empty() => (*id, vec![Op::Close { reason: (*r).into(), force: true }]),
            ["dep", "add", id, on] => (*id, vec![Op::DepAdd { on: (*on).into(), kind: "blocks".into() }]),
            ["dep", "add", id, on, "--type", t] if matches!(*t, "blocks" | "relates-to" | "parent-child" | "discovered-from") => (*id, vec![Op::DepAdd { on: (*on).into(), kind: (*t).into() }]),
            ["dep", "remove", id, on] => (*id, vec![Op::DepRemove { on: (*on).into() }]),
            ["dep", "relate", id, other] => (*id, vec![Op::DepRelate { other: (*other).into() }]),
            ["supersede", id, "--with", with] => (*id, vec![Op::Supersede { with: (*with).into() }]),
            _ => return None,
        };
        if id.starts_with('-') || (id.is_empty() && !ops.first().is_some_and(|op| matches!(op, Op::Create(_)) || op.is_standalone())) {
            return None;
        }
        Some(Mutation { argv: args.to_vec(), body: body.map(str::to_string), id: id.into(), ops, forced: a.contains(&"--force") })
    }

    pub fn bead(&self) -> &str {
        &self.id
    }

    fn create(&self) -> Option<&Create> {
        match self.ops.first() {
            Some(Op::Create(c)) => Some(c),
            _ => None,
        }
    }

    /// The beads the statements name besides the one the verb is on.
    fn others(&self) -> Vec<&str> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::DepAdd { on, .. } | Op::DepRemove { on } => Some(on.as_str()),
                Op::DepRelate { other } => Some(other.as_str()),
                Op::Supersede { with } => Some(with.as_str()),
                Op::Duplicate { of } => Some(of.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The transaction: `BEGIN`, the row changes, `COMMIT`.
    #[cfg(test)]
    pub fn script(&self, db: &str) -> Vec<String> {
        let reach: Vec<String> = self.seeds().iter().map(|s| s.to_string()).collect();
        self.script_in(db, &Env::default(), &reach)
    }

    /// The beads whose `is_blocked` this mutation can change, before their dependents and
    /// descendants are added: what `settle_blocked` is seeded with.
    pub fn seeds(&self) -> Vec<&str> {
        let mut seeds = Vec::new();
        for op in &self.ops {
            match op {
                Op::Status(_) | Op::Close { .. } | Op::GateResolve | Op::Supersede { .. } | Op::Duplicate { .. } => seeds.push(self.id.as_str()),
                Op::DepAdd { on, kind } if BLOCKING.contains(&kind.as_str()) => seeds.extend([self.id.as_str(), on.as_str()]),
                Op::DepRemove { on } => seeds.extend([self.id.as_str(), on.as_str()]),
                _ => {}
            }
        }
        seeds.sort_unstable();
        seeds.dedup();
        seeds
    }

    pub fn script_in(&self, db: &str, env: &Env, reach: &[String]) -> Vec<String> {
        let id = escape(&self.id);
        let lit = |s: &str| format!("'{}'", escape(s));
        let before = before_json(db, &id);
        let mut s = vec!["BEGIN".to_string()];
        let mut columns_done = false;
        for (n, op) in self.ops.iter().enumerate() {
            let settled = |column: &str| format!("UPDATE {db}.events SET new_value = (SELECT JSON_OBJECT('{column}', {column}) FROM {db}.issues WHERE id = '{id}') WHERE id = @event{n}");
            let filed = |old: &str| format!("INSERT INTO {db}.events (id, issue_id, event_type, actor, old_value, new_value, comment, created_at) VALUES (@event{n}, '{id}', 'updated', '{ACTOR}', {old}, NULL, NULL, UTC_TIMESTAMP())");
            match op {
                Op::LabelAdd(l) => s.extend(label_add_sql(db, &self.id, l)),
                Op::LabelRemove(l) => s.extend(label_remove_sql(db, &self.id, l)),
                Op::Note(t) => {
                    s.push(format!("SET @event{n} = UUID()"));
                    s.push(filed(&before));
                    s.push(format!("UPDATE {db}.issues SET notes = CASE WHEN notes IS NULL OR notes = '' THEN {t} ELSE CONCAT(notes, CHAR(10), {t}) END WHERE id = '{id}'", t = lit(t)));
                    s.push(settled("notes"));
                }
                Op::DraftNote { read, merged, text } => {
                    s.push(format!("SET @event{n} = UUID()"));
                    s.push(filed(&before));
                    s.push(format!("UPDATE {db}.issues SET notes = CASE WHEN COALESCE(notes, '') = {} THEN {} WHEN notes IS NULL OR notes = '' THEN {t} ELSE CONCAT(notes, CHAR(10), {t}) END WHERE id = '{id}'", lit(read), lit(merged), t = lit(text)));
                    s.push(settled("notes"));
                }
                Op::Comment(t) => {
                    s.push(format!("INSERT INTO {db}.comments (id, issue_id, author, text, created_at) VALUES (UUID(), '{id}', '{ACTOR}', {}, UTC_TIMESTAMP())", lit(t)));
                }
                Op::Metadata(k, v) => {
                    s.push(format!("SET @event{n} = UUID()"));
                    s.push(filed(&before));
                    s.push(format!("UPDATE {db}.issues SET metadata = JSON_SET(COALESCE(metadata, JSON_OBJECT()), '$.\"{}\"', {}) WHERE id = '{id}'", escape(k), metadata_value(v)));
                    s.push(settled("metadata"));
                }
                Op::Priority(p) => {
                    s.push(format!("SET @event{n} = UUID()"));
                    s.push(filed(&before));
                    s.push(format!("UPDATE {db}.issues SET priority = {p} WHERE id = '{id}'"));
                    s.push(settled("priority"));
                }
                op if op.is_column() => {
                    if !std::mem::replace(&mut columns_done, true) {
                        s.extend(columns_sql(db, &self.id, &self.ops, n, env, reach));
                    }
                }
                Op::Close { reason, .. } => s.extend(close_sql(db, &self.id, reason, env, reach)),
                Op::DepAdd { on, kind } => s.extend(dep_add_sql(db, &self.id, on, kind, true, env, reach)),
                Op::DepRemove { on } => s.extend(dep_remove_sql(db, &self.id, on, reach)),
                Op::DepRelate { other } => {
                    s.extend(dep_add_sql(db, &self.id, other, "relates-to", true, env, reach));
                    s.extend(dep_add_sql(db, other, &self.id, "relates-to", true, env, reach));
                }
                Op::Supersede { with } => {
                    s.extend(dep_add_sql(db, &self.id, with, "supersedes", false, env, reach));
                    s.extend(close_sql(db, &self.id, "", env, reach));
                }
                Op::Duplicate { of } => {
                    s.extend(dep_add_sql(db, &self.id, of, "duplicates", false, env, reach));
                    s.extend(close_sql(db, &self.id, "", env, reach));
                }
                Op::GateResolve => s.extend(close_sql(db, &self.id, "", env, reach)),
                _ => {}
            }
        }
        if self.ops.iter().any(Op::touches_row) {
            s.push(format!("UPDATE {db}.issues SET updated_at = UTC_TIMESTAMP(), row_lock = {ROW_LOCK} WHERE id = '{id}'"));
        }
        s.push("COMMIT".into());
        s
    }
}

fn label_add_sql(db: &str, bead: &str, label: &str) -> Vec<String> {
    let (id, e) = (&*escape(bead), escape(label));
    let absent = format!(" WHERE NOT EXISTS (SELECT 1 FROM {db}.labels WHERE issue_id = '{id}' AND label = '{e}')");
    vec![event_row(db, id, "label_added", "NULL", "NULL", &format!("'Added label: {e}'"), &absent), format!("INSERT IGNORE INTO {db}.labels (issue_id, label) VALUES ('{id}', '{e}')")]
}

fn label_remove_sql(db: &str, bead: &str, label: &str) -> Vec<String> {
    let (id, e) = (&*escape(bead), escape(label));
    let present = format!(" WHERE EXISTS (SELECT 1 FROM {db}.labels WHERE issue_id = '{id}' AND label = '{e}')");
    vec![event_row(db, id, "label_removed", "NULL", "NULL", &format!("'Removed label: {e}'"), &present), format!("DELETE FROM {db}.labels WHERE issue_id = '{id}' AND label = '{e}'")]
}

const ROW_LOCK: &str = "CAST((RAND() - 0.5) * 1.8e19 AS SIGNED)";

fn event_row(db: &str, id: &str, kind: &str, old: &str, new: &str, comment: &str, guard: &str) -> String {
    format!("INSERT INTO {db}.events (id, issue_id, event_type, actor, old_value, new_value, comment, created_at) SELECT UUID(), '{id}', '{kind}', '{ACTOR}', {old}, {new}, {comment}, UTC_TIMESTAMP() FROM DUAL{guard}")
}

fn before_json(db: &str, id: &str) -> String {
    let stamp = |column: &str| format!("DATE_FORMAT({column}, '%Y-%m-%dT%H:%i:%sZ')");
    format!(
        "(SELECT JSON_MERGE_PATCH('{{}}', JSON_OBJECT('id', id, 'title', title, 'description', NULLIF(description, ''), 'design', NULLIF(design, ''), 'acceptance_criteria', NULLIF(acceptance_criteria, ''), 'notes', NULLIF(notes, ''), 'spec_id', NULLIF(spec_id, ''), 'status', status, 'priority', priority, 'issue_type', issue_type, 'assignee', NULLIF(assignee, ''), 'estimated_minutes', estimated_minutes, 'owner', NULLIF(owner, ''), 'created_at', {}, 'created_by', NULLIF(created_by, ''), 'updated_at', {}, 'closed_at', {}, 'close_reason', NULLIF(close_reason, ''), 'external_ref', NULLIF(external_ref, ''), 'metadata', NULLIF(metadata, JSON_OBJECT()), 'labels', NULLIF((SELECT JSON_ARRAYAGG(label) FROM (SELECT label FROM {db}.labels WHERE issue_id = '{id}' ORDER BY label) AS l), JSON_ARRAY()))) FROM {db}.issues WHERE id = '{id}')",
        stamp("created_at"),
        stamp("updated_at"),
        stamp("closed_at")
    )
}

/// A status, assignee and description edit made by one `update`: one row change, one event.
fn columns_sql(db: &str, bead: &str, ops: &[Op], n: usize, env: &Env, reach: &[String]) -> Vec<String> {
    let id = &*escape(bead);
    let lit = |s: &str| format!("'{}'", escape(s));
    let find = |pick: fn(&Op) -> Option<&String>| ops.iter().find_map(pick);
    let status = find(|o| if let Op::Status(v) = o { Some(v) } else { None });
    let assignee = find(|o| if let Op::Assignee(v) = o { Some(v) } else { None });
    let description = find(|o| if let Op::Description(v) = o { Some(v) } else { None });
    let kind = match status {
        Some(_) => {
            format!("CASE WHEN @was{n} = 'closed' THEN 'reopened' ELSE 'status_changed' END")
        }
        None => "'updated'".to_string(),
    };
    let (mut sets, mut keys) = (Vec::new(), Vec::new());
    if let Some(v) = status {
        let v = lit(v);
        sets.push(format!("status = {v}"));
        sets.push(format!("started_at = CASE WHEN {v} = 'in_progress' AND started_at IS NULL THEN UTC_TIMESTAMP() ELSE started_at END"));
        sets.push(format!("closed_at = CASE WHEN @was{n} = 'closed' THEN NULL ELSE closed_at END"));
        sets.push(format!("close_reason = CASE WHEN @was{n} = 'closed' THEN '' ELSE close_reason END"));
        sets.push(format!("closed_by_session = CASE WHEN @was{n} = 'closed' THEN '' ELSE closed_by_session END"));
        sets.push(format!("pinned = CASE WHEN pinned = 1 AND {v} <> 'pinned' THEN 0 ELSE pinned END"));
        keys.push("'status', status");
    }
    if let Some(v) = assignee {
        sets.push(format!("assignee = {}", lit(v)));
        keys.push("'assignee', assignee");
    }
    if let Some(v) = description {
        sets.push(format!("description = {}", lit(v)));
        keys.push("'description', description");
    }
    sets.push("updated_at = UTC_TIMESTAMP()".to_string());
    sets.push(format!("row_lock = {ROW_LOCK}"));
    let mut s = vec![
        format!("SET @event{n} = UUID()"),
        format!("SET @was{n} = (SELECT status FROM {db}.issues WHERE id = '{id}')"),
        format!("SET @holder{n} = (SELECT COALESCE(assignee, '') FROM {db}.issues WHERE id = '{id}')"),
        format!("INSERT INTO {db}.events (id, issue_id, event_type, actor, old_value, new_value, comment, created_at) VALUES (@event{n}, '{id}', {kind}, '{ACTOR}', {}, NULL, NULL, UTC_TIMESTAMP())", before_json(db, id)),
        format!("UPDATE {db}.issues SET {} WHERE id = '{id}'", sets.join(", ")),
        format!("UPDATE {db}.events SET new_value = (SELECT JSON_OBJECT({}) FROM {db}.issues WHERE id = '{id}') WHERE id = @event{n}", keys.join(", ")),
    ];
    if env.leases && (status.is_some() || assignee.is_some()) {
        let now_status = status.map_or(format!("@was{n}"), |v| lit(v));
        let now_holder = assignee.map_or(format!("@holder{n}"), |v| lit(v));
        s.push(format!("DELETE FROM {db}.leases WHERE issue_id = '{id}' AND NOT ({now_status} = 'in_progress' AND {now_holder} <> '' AND @was{n} = 'in_progress' AND {now_holder} = @holder{n})"));
    }
    if status.is_some() {
        s.extend(settle_blocked(db, reach));
    }
    s
}

fn touch_coordination(db: &str, parent: &str, env: &Env) -> Vec<String> {
    if !env.local_metadata {
        return Vec::new();
    }
    ["dependencies", "wisp_dependencies"].iter().map(|tier| format!("REPLACE INTO {db}.local_metadata (`key`, value) VALUES ('{}', CAST({ROW_LOCK} AS CHAR))", ids::coordination_key(parent, tier))).collect()
}

fn close_sql(db: &str, bead: &str, reason: &str, env: &Env, reach: &[String]) -> Vec<String> {
    let (id, r) = (&*escape(bead), escape(reason));
    let open = format!(" WHERE EXISTS (SELECT 1 FROM {db}.issues WHERE id = '{id}' AND status <> 'closed')");
    let mut s = touch_coordination(db, bead, env);
    s.push(event_row(db, id, "closed", "''", &format!("'{r}'"), "NULL", &open));
    s.push(format!("UPDATE {db}.issues SET status = 'closed', closed_at = UTC_TIMESTAMP(), updated_at = UTC_TIMESTAMP(), close_reason = '{r}', closed_by_session = '', row_lock = {ROW_LOCK} WHERE id = '{id}' AND status <> 'closed'"));
    if env.leases {
        s.push(format!("DELETE FROM {db}.leases WHERE issue_id = '{id}'"));
    }
    s.extend(settle_blocked(db, reach));
    s
}

const TARGET: &str = "COALESCE(depends_on_issue_id, depends_on_wisp_id, depends_on_external)";

fn dep_add_sql(db: &str, from: &str, to: &str, kind: &str, emit: bool, env: &Env, reach: &[String]) -> Vec<String> {
    let (id, on, k) = (&*escape(from), &*escape(to), escape(kind));
    let absent = format!(" WHERE NOT EXISTS (SELECT 1 FROM {db}.dependencies WHERE issue_id = '{id}' AND {TARGET} = '{on}')");
    let mut s = Vec::new();
    if emit {
        s.push(event_row(db, id, "dependency_added", "''", &format!("'Added dependency: {id} {k} {on}'"), "NULL", &absent));
    }
    s.push(dependency_row(db, from, to, kind, &absent));
    if kind == "parent-child" {
        s.extend(touch_coordination(db, to, env));
    }
    if BLOCKING.contains(&kind) {
        s.extend(settle_blocked(db, reach));
    }
    s
}

fn dependency_row(db: &str, from: &str, to: &str, kind: &str, guard: &str) -> String {
    format!(
        "INSERT INTO {db}.dependencies (id, issue_id, depends_on_issue_id, type, created_at, created_by, metadata, thread_id) SELECT '{}', '{}', '{}', '{}', UTC_TIMESTAMP(), '{ACTOR}', '{{}}', '' FROM DUAL{guard}",
        ids::dependency_id(from, to),
        escape(from),
        escape(to),
        escape(kind)
    )
}

fn dep_remove_sql(db: &str, from: &str, to: &str, reach: &[String]) -> Vec<String> {
    let (id, on) = (&*escape(from), &*escape(to));
    let present = format!(" WHERE EXISTS (SELECT 1 FROM {db}.dependencies WHERE issue_id = '{id}' AND {TARGET} = '{on}')");
    let mut s = vec![event_row(db, id, "dependency_removed", "''", &format!("'Removed dependency on {on}'"), "NULL", &present), format!("DELETE FROM {db}.dependencies WHERE issue_id = '{id}' AND {TARGET} = '{on}'")];
    s.extend(settle_blocked(db, reach));
    s
}

/// The ids `seeds` reach: the seeds, their dependents, and those beads' parent-child
/// descendants two levels down. One flat lookup per level, so each is an index seek on
/// `depends_on_issue_id`; a level with nothing new issues no statement.
fn reach_with(db: &str, seeds: &[&str], mut query: impl FnMut(&str) -> Result<Vec<String>, String>) -> Result<Vec<String>, String> {
    let mut reached: Vec<String> = Vec::new();
    for s in seeds {
        if !reached.iter().any(|r| r == s) {
            reached.push((*s).to_string());
        }
    }
    let mut frontier = reached.clone();
    for level in 0..3 {
        if frontier.is_empty() {
            break;
        }
        let list = in_list(&frontier.iter().map(String::as_str).collect::<Vec<_>>());
        let only = if level == 0 { "" } else { "type = 'parent-child' AND " };
        let found = query(&format!("SELECT DISTINCT issue_id AS id FROM {db}.dependencies WHERE {only}depends_on_issue_id IN ({list})"))?;
        frontier = Vec::new();
        for id in found {
            if !reached.contains(&id) {
                reached.push(id.clone());
                frontier.push(id);
            }
        }
    }
    Ok(reached)
}

/// Resolves the beads `seeds` reach, on the writer's session, for `settle_blocked`.
fn reach_of(w: &Writer, seeds: &[&str]) -> Result<Vec<String>, String> {
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    reach_with(&w.db, seeds, |sql| Ok(rows(w, sql)?.iter().map(|r| cell(r, "id")).collect()))
}

/// bd's `is_blocked` maintenance for the beads a change reaches, already resolved to `reach`.
/// A bead with a `waits-for` edge or a wisp target is left as it is; the port does not read those.
fn settle_blocked(db: &str, reach: &[String]) -> Vec<String> {
    if reach.is_empty() {
        return Vec::new();
    }
    let ids: Vec<&str> = reach.iter().map(String::as_str).collect();
    blocked_passes(db, Some(&in_list(&ids)), 3)
}

/// `passes` rounds of bd's mark and unmark over the beads `scope` selects, or over every bead.
fn blocked_passes(db: &str, scope: Option<&str>, passes: usize) -> Vec<String> {
    let within = scope.map(|ids| format!("i.id IN ({ids}) AND ")).unwrap_or_default();
    let untouched = format!("NOT EXISTS (SELECT 1 FROM {db}.dependencies g WHERE g.issue_id = i.id AND (g.type = 'waits-for' OR g.depends_on_wisp_id IS NOT NULL))");
    let held = format!(
        "(EXISTS (SELECT 1 FROM {db}.dependencies d JOIN {db}.issues t ON t.id = d.depends_on_issue_id WHERE d.issue_id = i.id AND d.type IN ('blocks', 'conditional-blocks') AND t.status <> 'closed' AND t.status <> 'pinned') OR EXISTS (SELECT 1 FROM {db}.dependencies d JOIN {db}.issues p ON p.id = d.depends_on_issue_id WHERE d.issue_id = i.id AND d.type = 'parent-child' AND p.is_blocked = 1))"
    );
    let unmark = format!("UPDATE {db}.issues i SET i.is_blocked = 0, i.updated_at = i.updated_at WHERE {within}i.is_blocked = 1 AND {untouched} AND (i.status = 'closed' OR i.status = 'pinned' OR NOT {held})");
    let mark = format!("UPDATE {db}.issues i SET i.is_blocked = 1, i.updated_at = i.updated_at WHERE {within}i.is_blocked = 0 AND {untouched} AND i.status <> 'closed' AND i.status <> 'pinned' AND {held}");
    (0..passes).flat_map(|_| [unmark.clone(), mark.clone()]).collect()
}

/// How bd stores a `--set-metadata` value: a number, `true`, `false` or `null` as that JSON value,
/// anything else (a quoted string, an object, an array) as the text it was given.
fn metadata_value(v: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(v) {
        Ok(j) if v.trim() == v && (j.is_number() || j.is_boolean() || j.is_null()) => {
            format!("CAST('{}' AS JSON)", escape(v))
        }
        _ => format!("'{}'", escape(v)),
    }
}

fn update_ops(flags: &[&str], body: Option<&str>) -> Option<Vec<Op>> {
    let mut ops = Vec::new();
    let mut it = flags.iter();
    while let Some(f) = it.next() {
        if *f == "--force" {
            continue;
        }
        let v = it.next()?;
        ops.push(match *f {
            "--add-label" if !v.is_empty() => Op::LabelAdd((*v).into()),
            "--remove-label" if !v.is_empty() => Op::LabelRemove((*v).into()),
            "--set-metadata" => {
                let (k, val) = v.split_once('=')?;
                if k.is_empty() || k.contains(['"', '\\']) {
                    return None;
                }
                Op::Metadata(k.into(), val.into())
            }
            "--priority" | "-p" => Op::Priority(v.parse().ok().filter(|p| *p <= 4)?),
            "--status" | "-s" => Op::Status((*v).into()),
            "--assignee" | "-a" => Op::Assignee((*v).into()),
            "--body-file" if *v == "-" => Op::Description(body?.into()),
            _ => return None,
        });
    }
    let described = ops.iter().any(|op| matches!(op, Op::Description(_)));
    (described == body.is_some() && !ops.is_empty()).then_some(ops)
}

/// bd's `ParsePriority`: `2` or `P2`, 0 to 4.
fn priority_of(text: &str) -> Option<u8> {
    let t = text.trim();
    let digits = t.strip_prefix(['P', 'p']).unwrap_or(t);
    digits.parse().ok().filter(|p| *p <= 4)
}

/// The `create` the filing flags describe; `None` for any flag, value or combination this
/// port does not make exactly as bd does.
fn create_spec(args: &[&str], body: Option<&str>) -> Option<Create> {
    let mut c = Create {
        title: String::new(),
        issue_type: "task".into(),
        status: "open".into(),
        priority: 2,
        labels: Vec::new(),
        parent: None,
        deps: Vec::new(),
        description: String::new(),
        external_ref: String::new(),
        assignee: String::new(),
        id: None,
        inherit_labels: true,
        shape: Shape::Text,
    };
    let (mut titled, mut described, mut from_body, mut json, mut silent) = (false, false, false, false, false);
    let mut it = args.iter().enumerate();
    while let Some((i, f)) = it.next() {
        match *f {
            t if i == 0 && !t.starts_with('-') => {
                c.title = t.into();
                titled = true;
            }
            "--silent" => silent = true,
            "--json" => json = true,
            "--no-inherit-labels" => c.inherit_labels = false,
            "--body-file" => {
                let (_, v) = it.next()?;
                if *v != "-" || described {
                    return None;
                }
                c.description = body?.into();
                described = true;
                from_body = true;
            }
            flag => {
                let (_, v) = it.next()?;
                let v = *v;
                if v.is_empty() {
                    return None;
                }
                match flag {
                    "--title" if !titled => {
                        c.title = v.into();
                        titled = true;
                    }
                    "--type" | "-t" => c.issue_type = v.into(),
                    "--status" => c.status = v.into(),
                    "--priority" | "-p" => c.priority = priority_of(v)?,
                    "--labels" | "--label" | "-l" => c.labels.extend(v.split(',').map(str::to_string)),
                    "--external-ref" => c.external_ref = v.into(),
                    "--parent" => c.parent = Some(v.into()),
                    "--deps" => {
                        for entry in v.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                            let (kind, target) = match entry.split_once(':') {
                                None => ("blocks", entry),
                                Some((k @ ("discovered-from" | "relates-to" | "parent-child"), t)) => (k, t.trim()),
                                Some(_) => return None,
                            };
                            if target.is_empty() || c.deps.iter().any(|(_, t)| t == target) {
                                return None;
                            }
                            c.deps.push((kind.into(), target.into()));
                        }
                    }
                    "--description" | "-d" if !described && v != "-" => {
                        c.description = v.into();
                        described = true;
                    }
                    "--assignee" => c.assignee = v.into(),
                    "--id" => c.id = Some(v.into()),
                    _ => return None,
                }
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    c.labels = c.labels.iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && seen.insert(l.clone())).collect();
    let bounded = c.title.len() <= 500 && c.labels.iter().all(|l| l.chars().count() <= 255) && c.assignee.chars().count() <= 255;
    let consistent = c.id.is_none() || c.parent.is_none();
    let parent_edge_free = c.parent.as_ref().is_none_or(|p| c.deps.iter().all(|(_, t)| t != p));
    (titled && from_body == body.is_some() && !c.title.is_empty() && bounded && consistent && parent_edge_free && SETTABLE.contains(&c.status.as_str())).then(|| {
        c.shape = if json {
            Shape::Json
        } else if silent {
            Shape::Id
        } else {
            Shape::Text
        };
        c
    })
}

/// Why a transaction did not commit.
#[derive(Debug, PartialEq)]
pub enum Fail {
    /// COMMIT lost a serialization race (40001): nothing was applied.
    Conflict,
    /// A statement was refused for want of a privilege (MySQL 1142); carries Dolt's text.
    Denied(String),
    Other(String),
}

/// Where a transaction's statements go, in order, on one session.
pub trait Tx {
    fn send(&mut self, sql: &str) -> Result<(), Fail>;
}

fn once(what: &str, script: &[String], tx: &mut dyn Tx) -> Result<(), Fail> {
    script.iter().try_for_each(|s| {
        tx.send(s).map_err(|f| match f {
            Fail::Denied(dolt) => Fail::Other(denied(what, s, &dolt)),
            other => other,
        })
    })
}

/// The privilege and `<db>.<table>` a statement needs, read from its leading keywords.
fn tables_written(sql: &str) -> Option<(&'static str, String)> {
    let mut words = sql.split_whitespace();
    let (privilege, table) = match words.next()?.to_ascii_uppercase().as_str() {
        "INSERT" => {
            let mut w = words.skip_while(|w| w.eq_ignore_ascii_case("IGNORE"));
            w.next().filter(|w| w.eq_ignore_ascii_case("INTO"))?;
            ("INSERT", w.next()?)
        }
        "REPLACE" => {
            words.next().filter(|w| w.eq_ignore_ascii_case("INTO"))?;
            ("INSERT and DELETE", words.next()?)
        }
        "UPDATE" => ("UPDATE", words.next()?),
        "DELETE" => {
            words.next().filter(|w| w.eq_ignore_ascii_case("FROM"))?;
            ("DELETE", words.next()?)
        }
        _ => return None,
    };
    let table = table.split('(').next().unwrap_or(table).replace('`', "");
    table.contains('.').then_some((privilege, table))
}

fn denied(what: &str, sql: &str, dolt: &str) -> String {
    let lacks = match tables_written(sql) {
        Some((privilege, table)) => format!("{privilege} on {table}"),
        None => "a privilege this write needs".to_string(),
    };
    format!("{what} was not applied: the content writer lacks {lacks}; its grants predate this write — run `spira-lc admin-provision-writer`\n{dolt}")
}

/// The door's verb words for `argv`: `dep add`, `close`.
fn verb_of(argv: &[String]) -> String {
    let two = matches!(argv.first().map(String::as_str), Some("dep" | "label" | "comments" | "gate" | "kv"));
    argv.iter().take(if two { 2 } else { 1 }).cloned().collect::<Vec<_>>().join(" ")
}

/// Applies `script`; a conflict is retried once. A statement that fails ends the transaction
/// with no `COMMIT` sent.
pub fn apply(what: &str, script: &[String], tx: &mut dyn Tx) -> Result<(), String> {
    match once(what, script, tx) {
        Err(Fail::Conflict) => match once(what, script, tx) {
            Err(Fail::Conflict) => Err("the write conflicted with another writer twice; nothing was applied".into()),
            other => other.map_err(|f| format!("{f:?}")),
        },
        Err(Fail::Other(e) | Fail::Denied(e)) => Err(e),
        Ok(()) => Ok(()),
    }
}

/// The Dolt commit the writes owe, taken off the request: any number of writes before the
/// committer wakes cost one commit.
pub struct Committer {
    owed: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
}

impl Committer {
    pub const fn new() -> Committer {
        Committer { owed: std::sync::Mutex::new(false), wake: std::sync::Condvar::new() }
    }

    pub fn owe(&self) {
        *self.owed.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.wake.notify_one();
    }

    /// Blocks until a commit is owed, then settles every write owed so far.
    pub fn take(&self) {
        let mut g = self.owed.lock().unwrap_or_else(|e| e.into_inner());
        while !*g {
            g = self.wake.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        *g = false;
    }
}

static COMMITTER: Committer = Committer::new();

pub(crate) struct Writer {
    pub(crate) conn: crate::db::Conn,
    db: String,
}

/// Keeps only a value that was built: a failed build leaves nothing, so the next call builds again.
struct Built<T> {
    value: std::sync::OnceLock<T>,
    building: std::sync::Mutex<()>,
}

impl<T> Built<T> {
    const fn new() -> Self {
        Built { value: std::sync::OnceLock::new(), building: std::sync::Mutex::new(()) }
    }

    fn get_or_build(&self, build: impl FnOnce() -> Result<T, String>) -> Result<&T, String> {
        if let Some(v) = self.value.get() {
            return Ok(v);
        }
        let _one_builder = self.building.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = self.value.get() {
            return Ok(v);
        }
        let built = build()?;
        Ok(self.value.get_or_init(|| built))
    }
}

static WRITER: Built<Writer> = Built::new();

fn build_writer(file: &str) -> Result<Writer, String> {
    let s = crate::content_sql::Settings::from_config()?;
    let user = s.user.strip_suffix("_ro").unwrap_or(&s.user).to_string();
    let db = crate::plan::content_db()?.to_string();
    let conn = crate::db::Conn::writer(&s.host, s.port, &user, file, db.trim_matches('`'))?;
    Ok(Writer { conn, db })
}

/// Whether a writer credential is configured.
pub fn configured() -> Result<bool, String> {
    spira_config::process::cfg("SPIRA_BEADS_SQL_WRITER_PASSWORD_FILE").map(|f| !f.trim().is_empty())
}

/// The writer connection, or `None` when no writer credential is configured.
pub(crate) fn writer() -> Option<Result<&'static Writer, String>> {
    let file = match spira_config::process::cfg("SPIRA_BEADS_SQL_WRITER_PASSWORD_FILE") {
        Ok(f) => f,
        Err(e) => return Some(Err(e)),
    };
    if file.trim().is_empty() {
        return None;
    }
    Some(WRITER.get_or_build(|| build_writer(&file)))
}

/// Applies `m` to the beads database in one transaction. `None` when no writer is configured,
/// or when the port does not cover the write.
pub fn run(m: &Mutation) -> Option<Result<String, String>> {
    let w = match writer()? {
        Ok(w) => w,
        Err(e) => return Some(Err(e)),
    };
    match serve(w, m) {
        Ok(Some(out)) => {
            COMMITTER.owe();
            Some(Ok(out))
        }
        Ok(None) => None,
        Err(e) => Some(Err(e)),
    }
}

fn rows(w: &Writer, sql: &str) -> Result<Vec<serde_json::Value>, String> {
    w.conn.query(sql).map_err(|e| format!("{e:?}"))
}

fn cell(row: &serde_json::Value, key: &str) -> String {
    match row.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn count(w: &Writer, sql: &str) -> Result<u64, String> {
    Ok(rows(w, sql)?.first().map(|r| cell(r, "n")).and_then(|n| n.parse().ok()).unwrap_or(0))
}

fn in_list(ids: &[&str]) -> String {
    ids.iter().map(|i| format!("'{}'", escape(i))).collect::<Vec<_>>().join(", ")
}

fn schema_of(w: &Writer) -> &str {
    w.db.trim_matches('`')
}

/// Which of the tables the statements write only when present the store has.
fn env_of(w: &Writer) -> Result<Env, String> {
    static ENV: std::sync::OnceLock<Env> = std::sync::OnceLock::new();
    if let Some(e) = ENV.get() {
        return Ok(*e);
    }
    let present = rows(w, &format!("SELECT TABLE_NAME AS name FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{}' AND TABLE_NAME IN ('leases', 'local_metadata')", escape(schema_of(w))))?;
    let has = |t: &str| present.iter().any(|r| cell(r, "name") == t);
    Ok(*ENV.get_or_init(|| Env { leases: has("leases"), local_metadata: has("local_metadata") }))
}

fn serve(w: &Writer, m: &Mutation) -> Result<Option<String>, String> {
    if let Some(c) = m.create() {
        return create(w, c);
    }
    if m.ops.first().is_some_and(Op::is_standalone) {
        return verbs::standalone(w, m);
    }
    let db = &w.db;
    let mut named = vec![m.bead()];
    named.extend(m.others());
    let found = rows(w, &format!("SELECT id, status, issue_type, status = 'closed' AS closed, status = 'in_progress' AS claimed, COALESCE(assignee, '') AS assignee, description, COALESCE(notes, '') AS notes, is_blocked, ephemeral FROM {db}.issues WHERE id IN ({})", in_list(&named)))?;
    let row_of = |id: &str| found.iter().find(|r| cell(r, "id") == id);
    let Some(me) = row_of(m.bead()) else {
        return Err(format!("no issue found matching {:?}", m.bead()));
    };
    if let Some(Op::DepAdd { on, .. }) = m.ops.first() {
        if row_of(on).is_none() {
            return Err(format!(
                "{on} is not a known item, so {} cannot depend on it: check the id",
                m.bead()
            ));
        }
    }
    if m.others().iter().any(|o| row_of(o).is_none()) {
        return Ok(None);
    }
    let Some(m) = preflight(w, m, me)? else {
        return Ok(None);
    };
    if m.ops.is_empty() {
        return Ok(Some(String::new()));
    }
    if let Some(Op::SetState { dimension, value }) = m.ops.first() {
        return verbs::set_state(w, m.bead(), dimension, value);
    }
    let env = env_of(w)?;
    let reach = reach_of(w, &m.seeds())?;
    w.conn.transaction(|tx| apply(&verb_of(&m.argv), &m.script_in(db, &env, &reach), tx)).and_then(|r| r)?;
    Ok(Some(m.stdout()))
}

/// The mutation with the edits bd would drop as no-ops removed, or `None` when bd would refuse
/// it: the claim fence on an assignee, close policy, a dependency cycle or hierarchy conflict,
/// a second edge between two beads.
fn preflight(w: &Writer, m: &Mutation, me: &serde_json::Value) -> Result<Option<Mutation>, String> {
    let db = &w.db;
    let id = escape(m.bead());
    let (status, holder) = (cell(me, "status"), cell(me, "assignee"));
    let edge = |from: &str, to: &str| -> Result<Option<String>, String> { Ok(rows(w, &format!("SELECT type FROM {db}.dependencies WHERE issue_id = '{}' AND {TARGET} = '{}'", escape(from), escape(to)))?.first().map(|r| cell(r, "type"))) };
    let mut kept = Vec::new();
    for op in &m.ops {
        match op {
            Op::Status(v) if !SETTABLE.contains(&v.as_str()) => return Ok(None),
            Op::Status(v) if *v == status => continue,
            Op::Assignee(v) if v.chars().count() > 255 => return Ok(None),
            Op::Assignee(v) if *v == holder => continue,
            Op::Assignee(_) if !m.forced && !holder.is_empty() && cell(me, "claimed") == "1" => return Ok(None),
            Op::Description(v) if *v == cell(me, "description") => continue,
            Op::Note(t) => {
                let read = cell(me, "notes");
                if let Some(merged) = bead::with_drafts(&read, t) {
                    kept.push(Op::DraftNote { read, merged, text: t.clone() });
                    continue;
                }
            }
            Op::Close { force: false, .. } if cell(me, "closed") != "1" => {
                let children = count(w, &format!("SELECT COUNT(DISTINCT d.issue_id) AS n FROM {db}.dependencies d JOIN {db}.issues c ON c.id = d.issue_id WHERE d.depends_on_issue_id = '{id}' AND d.type = 'parent-child' AND c.status <> 'closed'"))?;
                if children > 0 || cell(me, "is_blocked") == "1" {
                    return Ok(None);
                }
            }
            Op::DepAdd { on, kind } => {
                let me_id = m.bead();
                if on == me_id {
                    return Err(format!("{me_id} cannot depend on itself: a self edge is no dependency; name another item"));
                }
                if let Some(t) = edge(me_id, on)?.filter(|t| t != kind) {
                    let way = if t == "parent-child" && kind == "blocks" {
                        format!("{on} is {me_id}'s parent; to make {me_id} wait for it add the label {}", spira_config::nonwork::SPLIT_LABEL)
                    } else {
                        format!("`dep remove {me_id} {on}` first, then add the {kind} edge")
                    };
                    return Err(format!("{me_id} already has a {t} edge onto {on}, and a pair holds one edge: {way}"));
                }
                if kind == "blocks" {
                    if hierarchy(w, me_id, on)? || is_descendant(me_id, on) {
                        return Err(format!("{on} is an ancestor of {me_id} but not its direct parent: depend on the direct parent, or on an item outside {on}'s tree"));
                    }
                    if hierarchy(w, on, me_id)? || is_descendant(on, me_id) {
                        return Err(format!("{on} is a descendant of {me_id}: a parent waits on its children through them, not by an edge; depend on an item outside {me_id}'s tree"));
                    }
                }
                if BLOCKING.contains(&kind.as_str()) && cycle(w, me_id, on)? {
                    return Err(format!("{me_id} -> {on} would close a dependency cycle ({on} already depends on {me_id}, directly or through its own edges): remove the edge that closes it, or depend on another item"));
                }
            }
            Op::DepRelate { other } => {
                if other == m.bead() || edge(m.bead(), other)?.is_some_and(|t| t != "relates-to") || edge(other, m.bead())?.is_some_and(|t| t != "relates-to") {
                    return Ok(None);
                }
            }
            Op::Supersede { with: other } | Op::Duplicate { of: other } => {
                let kind = if matches!(op, Op::Supersede { .. }) { "supersedes" } else { "duplicates" };
                if other == m.bead() || edge(m.bead(), other)?.is_some_and(|t| t != kind) {
                    return Ok(None);
                }
            }
            Op::GateResolve if cell(me, "issue_type") != "gate" || cell(me, "closed") == "1" => return Ok(None),
            Op::SetState { .. } if cell(me, "ephemeral") == "1" => return Ok(None),
            _ => {}
        }
        kept.push(op.clone());
    }
    Ok(Some(Mutation { ops: kept, ..m.clone() }))
}

fn is_descendant(child: &str, parent: &str) -> bool {
    child.strip_prefix(parent).is_some_and(|rest| rest.starts_with('.'))
}

/// Whether `from` depending on `to` would close a loop through the blocking and parent-child edges.
fn cycle(w: &Writer, from: &str, to: &str) -> Result<bool, String> {
    let db = &w.db;
    Ok(count(
        w,
        &format!("WITH RECURSIVE reachable(node) AS (SELECT '{}' UNION SELECT COALESCE(d.depends_on_issue_id, d.depends_on_wisp_id, d.depends_on_external) FROM reachable r JOIN {db}.dependencies d ON d.issue_id = r.node AND d.type IN ('blocks', 'conditional-blocks', 'parent-child')) SELECT COUNT(*) AS n FROM reachable WHERE node = '{}'", escape(to), escape(from)),
    )? > 0)
}

/// Whether `candidate` is an ancestor of `node` along parent-child edges.
fn hierarchy(w: &Writer, node: &str, candidate: &str) -> Result<bool, String> {
    let db = &w.db;
    Ok(count(
        w,
        &format!("WITH RECURSIVE ancestors(node) AS (SELECT '{}' UNION SELECT COALESCE(d.depends_on_issue_id, d.depends_on_wisp_id, d.depends_on_external) FROM ancestors a JOIN {db}.dependencies d ON d.issue_id = a.node AND d.type = 'parent-child') SELECT COUNT(*) AS n FROM ancestors WHERE node = '{}'", escape(node), escape(candidate)),
    )? > 0)
}

impl Mutation {
    /// What bd prints for the verb: the lines its callers read.
    fn stdout(&self) -> String {
        let id = &self.id;
        match self.ops.first() {
            Some(Op::Close { reason, .. }) => format!("✓ Closed {id}: {reason}\n"),
            Some(Op::DepAdd { on, kind }) => format!("✓ Added dependency: {id} → {on} ({kind})\n"),
            Some(Op::DepRemove { on }) => format!("✓ Removed dependency: {id} → {on}\n"),
            Some(Op::DepRelate { other }) => format!("✓ Linked {id} ↔ {other}\n"),
            Some(Op::Supersede { with }) => {
                format!("✓ Marked {id} as superseded by {with} (closed)\n")
            }
            Some(Op::Duplicate { of }) => format!("✓ Marked {id} as duplicate of {of} (closed)\n"),
            Some(Op::GateResolve) => format!("✓ Gate resolved: {id}\n"),
            _ => String::new(),
        }
    }
}

const ID_ATTEMPTS: usize = 6;

/// A create: the id from the store's own rules, the row, its events, labels, edges and counters,
/// as one transaction. The id is read for and then inserted, so a create that lost the id to
/// another writer reads again.
fn create(w: &Writer, c: &Create) -> Result<Option<String>, String> {
    let db = &w.db;
    let config = rows(w, &format!("SELECT `key` AS k, value AS v FROM {db}.config WHERE `key` IN ('issue_prefix', 'issue_id_mode', 'max_collision_prob', 'min_hash_length', 'max_hash_length', 'types.custom', 'allowed_prefixes')"))?;
    let conf = |k: &str| config.iter().find(|r| cell(r, "k") == k).map(|r| cell(r, "v")).filter(|v| !v.is_empty());
    let Some(prefix) = conf("issue_prefix") else {
        return Ok(None);
    };
    if conf("issue_id_mode").as_deref() == Some("counter") {
        return Ok(None);
    }
    if !TYPES.contains(&c.issue_type.as_str()) {
        let table = rows(w, &format!("SELECT name FROM {db}.custom_types")).unwrap_or_default().iter().map(|r| cell(r, "name")).collect::<Vec<_>>();
        let listed = conf("types.custom").unwrap_or_default();
        if !table.contains(&c.issue_type) && !listed.split(',').any(|t| t.trim() == c.issue_type) {
            return Ok(None);
        }
    }
    let mut named: Vec<&str> = c.deps.iter().map(|(_, t)| t.as_str()).collect();
    named.extend(c.parent.as_deref());
    named.extend(c.id.as_deref());
    let present = if named.is_empty() { Vec::new() } else { rows(w, &format!("SELECT id FROM {db}.issues WHERE id IN ({})", in_list(&named)))? };
    let exists = |id: &str| present.iter().any(|r| cell(r, "id") == id);
    if c.deps.iter().any(|(_, t)| !exists(t)) || c.parent.as_deref().is_some_and(|p| !exists(p)) {
        return Ok(None);
    }
    if let Some(id) = &c.id {
        let allowed = conf("allowed_prefixes").unwrap_or_default();
        let fits = std::iter::once(prefix.as_str()).chain(allowed.split(',').map(str::trim)).any(|p| !p.is_empty() && id.starts_with(&format!("{p}-")));
        if exists(id) || !fits {
            return Ok(None);
        }
    }
    let mut labels = c.labels.clone();
    if let (Some(p), true) = (&c.parent, c.inherit_labels) {
        for r in rows(w, &format!("SELECT label FROM {db}.labels WHERE issue_id = '{}' ORDER BY label", escape(p)))? {
            let l = cell(&r, "label");
            if !labels.contains(&l) {
                labels.push(l);
            }
        }
    }
    let env = env_of(w)?;
    for _ in 0..ID_ATTEMPTS {
        let made = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let (id, child) = match (&c.id, &c.parent) {
            (Some(id), _) => (id.clone(), None),
            (None, Some(p)) => {
                let n = next_child(w, p)?;
                (format!("{p}.{n}"), Some((p.clone(), n)))
            }
            (None, None) => (hash_id(w, &prefix, c, made.as_nanos() as i128, &conf)?, None),
        };
        let blocking = c.parent.is_some() || c.deps.iter().any(|(k, _)| BLOCKING.contains(&k.as_str()));
        let reach = if blocking { reach_of(w, &[&id])? } else { Vec::new() };
        let script = create_script(db, &env, c, &id, &labels, child.as_ref(), &reach);
        match w.conn.transaction(|tx| apply("create", &script, tx)).and_then(|r| r) {
            Ok(()) => return Ok(Some(c.stdout(&id, &labels, made.as_secs()))),
            Err(e) if c.id.is_none() && (e.to_lowercase().contains("duplicate") || e.contains("conflicted with another writer")) => continue,
            Err(e) => return Err(e),
        }
    }
    Err("could not allocate an unused bead id".into())
}

/// bd's id for a new bead: the first of ten nonces, from the length the store's size calls for
/// up to eight, whose candidate no bead holds.
fn hash_id(w: &Writer, prefix: &str, c: &Create, nanos: i128, conf: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let db = &w.db;
    let p = escape(prefix);
    let beads = count(w, &format!("SELECT COUNT(*) AS n FROM {db}.issues WHERE id LIKE CONCAT('{p}', '-%') AND INSTR(SUBSTRING(id, LENGTH('{p}') + 2), '.') = 0"))?;
    let min = conf("min_hash_length").and_then(|v| v.parse().ok()).unwrap_or(3);
    let max = conf("max_hash_length").and_then(|v| v.parse().ok()).unwrap_or(8);
    let probability = conf("max_collision_prob").and_then(|v| v.parse().ok()).unwrap_or(0.25);
    let base = ids::adaptive_length(beads, probability, min, max).min(8);
    for length in base..=8 {
        let candidates: Vec<String> = (0..10).map(|nonce| ids::hash_id(prefix, &c.title, &c.description, ACTOR, nanos, length, nonce)).collect();
        let held = rows(w, &format!("SELECT id FROM {db}.issues WHERE id IN ({})", in_list(&candidates.iter().map(String::as_str).collect::<Vec<_>>())))?;
        if let Some(free) = candidates.iter().find(|id| !held.iter().any(|r| cell(r, "id") == **id)) {
            return Ok(free.clone());
        }
    }
    Err(format!("failed to generate unique ID after trying lengths {}-8 with 10 nonces each", base))
}

/// bd's `GetNextChildID`: one past the higher of the parent's counter and its highest child.
fn next_child(w: &Writer, parent: &str) -> Result<u64, String> {
    let db = &w.db;
    let p = escape(parent);
    let mut last = count(w, &format!("SELECT last_child AS n FROM {db}.child_counters WHERE parent_id = '{p}'"))?;
    for r in rows(w, &format!("SELECT id FROM {db}.issues WHERE id LIKE CONCAT('{p}', '.%') AND id NOT LIKE CONCAT('{p}', '.%.%')"))? {
        if let Some(n) = cell(&r, "id").rsplit('.').next().and_then(|n| n.parse::<u64>().ok()) {
            last = last.max(n);
        }
    }
    Ok(last + 1)
}

fn create_script(db: &str, env: &Env, c: &Create, id: &str, labels: &[String], child: Option<&(String, u64)>, reach: &[String]) -> Vec<String> {
    let q = escape(id);
    let lit = |s: &str| format!("'{}'", escape(s));
    let optional = |s: &str| {
        if s.is_empty() {
            "NULL".to_string()
        } else {
            lit(s)
        }
    };
    let hash = ids::content_hash(&ids::Content { title: &c.title, description: &c.description, status: &c.status, priority: c.priority, issue_type: &c.issue_type, assignee: &c.assignee, owner: "", created_by: ACTOR, external_ref: &c.external_ref });
    let mut s = vec!["BEGIN".to_string()];
    if let Some(p) = &c.parent {
        s.extend(touch_coordination(db, p, env));
    }
    s.push(format!(
        "INSERT INTO {db}.issues (id, content_hash, title, description, design, acceptance_criteria, notes, status, priority, issue_type, assignee, created_at, created_by, owner, updated_at, external_ref, spec_id, work_type, row_lock) VALUES ('{q}', '{hash}', {}, {}, '', '', '', {}, {}, {}, {}, UTC_TIMESTAMP(), '{ACTOR}', '', UTC_TIMESTAMP(), {}, '', '', {ROW_LOCK})",
        lit(&c.title),
        lit(&c.description),
        lit(&c.status),
        c.priority,
        lit(&c.issue_type),
        optional(&c.assignee),
        optional(&c.external_ref)
    ));
    s.push(event_row(db, &q, "created", "''", "''", "NULL", ""));
    for l in labels {
        s.push(format!("INSERT IGNORE INTO {db}.labels (issue_id, label) VALUES ('{q}', {})", lit(l)));
        s.push(event_row(db, &q, "label_added", "NULL", "NULL", &lit(&format!("Added label: {l}")), ""));
    }
    if let Some((parent, n)) = child {
        s.push(format!("INSERT INTO {db}.child_counters (parent_id, last_child) VALUES ({}, {n}) ON DUPLICATE KEY UPDATE last_child = {n}", lit(parent)));
    } else if let Some((parent, n)) = c.id.as_deref().and_then(|i| i.rsplit_once('.')).and_then(|(p, n)| Some((p, n.parse::<u64>().ok().filter(|n| *n > 0)?))) {
        s.push(format!(
            "INSERT INTO {db}.child_counters (parent_id, last_child) SELECT {}, {n} FROM DUAL WHERE EXISTS (SELECT 1 FROM {db}.issues WHERE id = {}) ON DUPLICATE KEY UPDATE last_child = GREATEST(last_child, {n})",
            lit(parent),
            lit(parent)
        ));
    }
    let edges = c.parent.iter().map(|p| ("parent-child", p.as_str())).chain(c.deps.iter().map(|(k, t)| (k.as_str(), t.as_str())));
    let mut blocking = false;
    for (kind, target) in edges {
        s.push(dependency_row(db, id, target, kind, ""));
        blocking |= BLOCKING.contains(&kind);
    }
    if blocking {
        s.extend(settle_blocked(db, reach));
    }
    s.push("COMMIT".into());
    s
}

impl Create {
    /// What `bd create` prints for the flags this create was given.
    fn stdout(&self, id: &str, labels: &[String], now: u64) -> String {
        match self.shape {
            Shape::Id => format!("{id}\n"),
            Shape::Json => {
                let stamp = spira_desired_state::store::format_rfc3339(now);
                let mut row = serde_json::json!({
                    "id": id,
                    "title": self.title,
                    "status": self.status,
                    "priority": self.priority,
                    "issue_type": self.issue_type,
                    "created_at": stamp,
                    "created_by": ACTOR,
                    "updated_at": stamp,
                });
                if let Some(o) = row.as_object_mut() {
                    for (key, value) in [("description", &self.description), ("assignee", &self.assignee), ("external_ref", &self.external_ref)] {
                        if !value.is_empty() {
                            o.insert(key.into(), value.clone().into());
                        }
                    }
                    if !labels.is_empty() {
                        o.insert("labels".into(), labels.into());
                    }
                }
                format!("{}\n", serde_json::to_string_pretty(&row).unwrap_or_default())
            }
            Shape::Text => format!("✓ Created issue: {id} — {}\n  Priority: P{}\n  Status: {}\n", self.title, self.priority, self.status),
        }
    }
}

/// The statement that sets bd's kv `key` to the number `value`, never lowering one already held.
fn kv_upsert_sql(db: &str, key: &str, value: u64) -> String {
    let key = escape(&format!("{}{key}", crate::content_sql::KV_PREFIX));
    format!("INSERT INTO {db}.config (`key`, value) VALUES ('{key}', '{value}') ON DUPLICATE KEY UPDATE value = IF(CAST(value AS UNSIGNED) < {value}, VALUES(value), value)")
}

/// Moves bd's kv `key` forward to `value` in one statement. `None` when no writer is configured.
pub fn upsert_kv(key: &str, value: u64) -> Option<Result<(), String>> {
    let w = match writer()? {
        Ok(w) => w,
        Err(e) => return Some(Err(e)),
    };
    Some(w.conn.execute(&kv_upsert_sql(&w.db, key, value)).map_err(|e| format!("{e:?}")).map(|()| COMMITTER.owe()))
}

/// The committer thread lc-serve runs.
pub fn run_committer() {
    loop {
        COMMITTER.take();
        let Some(Ok(w)) = writer() else { continue };
        match w.conn.execute("CALL DOLT_COMMIT('-Am', 'spira-lc: content writes')") {
            Ok(()) => {}
            Err(e) if format!("{e:?}").contains("nothing to commit") => {}
            Err(e) => {
                eprintln!("spira-lc serve: dolt commit of content writes failed: {e:?}");
                std::thread::sleep(std::time::Duration::from_secs(1));
                COMMITTER.owe();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tonbytt)]
    #[test]
    fn a_failed_writer_build_is_tried_again_and_a_built_writer_is_kept() {
        let cell: Built<u32> = Built::new();
        let builds = std::cell::Cell::new(0);
        let build = |r: Result<u32, String>| {
            builds.set(builds.get() + 1);
            r
        };
        assert_eq!(cell.get_or_build(|| build(Err("reading /x: No such file".into()))), Err("reading /x: No such file".to_string()));
        assert_eq!(cell.get_or_build(|| build(Ok(7))), Ok(&7));
        assert_eq!(cell.get_or_build(|| build(Ok(8))), Ok(&7));
        assert_eq!(builds.get(), 2);
    }

    #[test_id(tz26imh)]
    #[test]
    fn the_kv_upsert_moves_a_number_forward_only_and_names_the_prefixed_key() {
        let sql = kv_upsert_sql("`beads`", "spira.planning_watermark", 7);
        assert_eq!(sql, "INSERT INTO `beads`.config (`key`, value) VALUES ('kv.spira.planning_watermark', '7') ON DUPLICATE KEY UPDATE value = IF(CAST(value AS UNSIGNED) < 7, VALUES(value), value)");
    }

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn script(args: &[&str], body: Option<&str>) -> Vec<String> {
        Mutation::of(&v(args), body).expect("a served verb").script("`beads`")
    }

    struct Rec {
        sent: Vec<String>,
        fail_at: Option<(usize, Fail)>,
        conflicts: usize,
    }

    impl Tx for Rec {
        fn send(&mut self, sql: &str) -> Result<(), Fail> {
            self.sent.push(sql.into());
            if sql == "COMMIT" && self.conflicts > 0 {
                self.conflicts -= 1;
                return Err(Fail::Conflict);
            }
            match &self.fail_at {
                Some((n, _)) if *n == self.sent.len() => Err(self.fail_at.take().unwrap().1),
                _ => Ok(()),
            }
        }
    }

    #[test_id(tzivjd4)]
    #[test]
    fn only_the_served_verbs_are_mutations() {
        for argv in [
            &["update", "sp-a", "--set-metadata", "a\"b=c"][..],
            &["update", "sp-a", "--priority", "9"],
            &["update", "sp-a"],
            &["update", "sp-a", "--title", "x"],
            &["update", "sp-a", "--body-file", "-"],
            &["note", "sp-a", "--stdin"],
            &["create"],
            &["create", "--type", "task"],
            &["create", "T", "--force"],
            &["create", "T", "--priority", "9"],
            &["create", "T", "--status", "closed"],
            &["create", "T", "--deps", "blocks:sp-b"],
            &["create", "T", "--id", "sp-x", "--parent", "sp-y"],
            &["close", "sp-a", "--reason", ""],
            &["close", "sp-a", "--session", "s"],
            &["dep", "add", "sp-a", "sp-b", "--type", "tracks"],
            &["dep", "list", "sp-a"],
            &["supersede", "sp-a"],
        ] {
            assert_eq!(Mutation::of(&v(argv), None), None, "{argv:?}");
        }
        assert_eq!(Mutation::of(&v(&["update", "sp-a", "--priority", "1"]), Some("stray body")), None);
        assert_eq!(Mutation::of(&v(&["create", "T", "--body-file", "-"]), None), None);
        assert!(Mutation::of(&v(&["note", "sp-a", "--stdin"]), Some("b")).is_some());
        assert!(Mutation::of(&v(&["update", "sp-a", "--add-label", "x", "--priority", "1"]), None).is_some());
    }

    #[test_id(tfc0up5)]
    #[test]
    fn every_write_the_door_and_the_lanes_make_is_a_mutation() {
        for (argv, body) in [
            (&["create", "T", "--type", "task", "--labels", "repo:spira", "--silent"][..], None),
            (&["create", "--parent", "sp-e", "--silent", "--title", "T", "--no-inherit-labels"], None),
            (&["create", "T", "-t", "bug", "-p", "P1", "-l", "a,b", "--external-ref", "gh-1", "--deps", "sp-b,discovered-from:sp-c", "--json"], None),
            (&["create", "T", "--body-file", "-"], Some("b")),
            (&["update", "sp-a", "--status", "open", "--assignee", ""], None),
            (&["update", "sp-a", "--assignee", "x", "--force"], None),
            (&["update", "sp-a", "--body-file", "-"], Some("text")),
            (&["close", "sp-a"], None),
            (&["close", "sp-a", "--reason", "r"], None),
            (&["close", "sp-a", "--force", "--reason", "r"], None),
            (&["dep", "add", "sp-a", "sp-b"], None),
            (&["dep", "add", "sp-a", "sp-b", "--type", "discovered-from"], None),
            (&["dep", "remove", "sp-a", "sp-b"], None),
            (&["dep", "relate", "sp-a", "sp-b"], None),
            (&["supersede", "sp-a", "--with", "sp-b"], None),
            (&["duplicate", "sp-a", "--of", "sp-b"], None),
            (&["set-state", "sp-a", "branch=spira/sp-a"], None),
            (&["gate", "resolve", "sp-a"], None),
            (&["recompute-blocked"], None),
            (&["gate", "check", "--type", "timer"], None),
            (&["gate", "check", "--type=timer"], None),
            (&["kv", "set", "k", "v"], None),
            (&["kv", "clear", "k"], None),
            (&["remember", "--key", "k", "the text"], None),
            (&["remember", "--key", "k"], Some("the text")),
            (&["forget", "k"], None),
        ] {
            assert!(Mutation::of(&v(argv), body).is_some(), "{argv:?}");
        }
    }

    /// Every statement the door's writes generate, each with the write that sends it.
    fn door_statements() -> Vec<(String, String)> {
        let mut all = Vec::new();
        for (argv, body) in [
            (&["create", "T", "--type", "task", "--labels", "repo:spira", "--silent"][..], None),
            (&["create", "--parent", "sp-e", "--silent", "--title", "T"], None),
            (&["create", "T", "--deps", "sp-b,discovered-from:sp-c"], None),
            (&["update", "sp-a", "--status", "open", "--assignee", "x"], None),
            (&["update", "sp-a", "--add-label", "x", "--priority", "1", "--set-metadata", "a=b"], None),
            (&["update", "sp-a", "--body-file", "-"], Some("text")),
            (&["note", "sp-a", "text"], None),
            (&["comments", "add", "sp-a", "text"], None),
            (&["label", "add", "sp-a", "l"], None),
            (&["label", "remove", "sp-a", "l"], None),
            (&["close", "sp-a", "--reason", "r"], None),
            (&["dep", "add", "sp-a", "sp-b"], None),
            (&["dep", "add", "sp-a", "sp-b", "--type", "discovered-from"], None),
            (&["dep", "remove", "sp-a", "sp-b"], None),
            (&["dep", "relate", "sp-a", "sp-b"], None),
            (&["supersede", "sp-a", "--with", "sp-b"], None),
            (&["duplicate", "sp-a", "--of", "sp-b"], None),
            (&["gate", "resolve", "sp-a"], None),
        ] {
            let m = Mutation::of(&v(argv), body).unwrap();
            if let Some(c) = m.create() {
                let child = c.parent.clone().map(|p| (p, 1));
                all.extend(create_script("`beads`", &Env::default(), c, "sp-n", &["l".to_string()], child.as_ref(), &["sp-n".to_string()]).into_iter().map(|q| ("create".to_string(), q)));
            }
            all.extend(m.script("`beads`").into_iter().map(|q| (verb_of(&m.argv), q)));
        }
        all.extend(verbs::tests::standalone_statements("`beads`").into_iter().map(|q| ("a standalone verb".to_string(), q)));
        all
    }

    /// `text` cut at each `sep` that sits outside quotes and parentheses.
    fn top_level(text: &str, sep: char) -> Vec<&str> {
        let (mut depth, mut quoted, mut from) = (0i32, false, 0);
        let mut parts = Vec::new();
        for (i, c) in text.char_indices() {
            match c {
                '\'' => quoted = !quoted,
                '(' if !quoted => depth += 1,
                ')' if !quoted => depth -= 1,
                c if c == sep && !quoted && depth == 0 => {
                    parts.push(&text[from..i]);
                    from = i + 1;
                }
                _ => {}
            }
        }
        parts.push(&text[from..]);
        parts
    }

    /// The table a statement writes and the columns it assigns.
    fn assigned(sql: &str) -> Option<(String, Vec<String>)> {
        let (_, table) = tables_written(sql)?;
        let table = table.rsplit('.').next()?.to_string();
        let column = |c: &str| c.trim().trim_start_matches("i.").trim_matches('`').to_string();
        let columns = if sql.starts_with("INSERT") || sql.starts_with("REPLACE") {
            sql.split_once('(')?.1.split_once(')')?.0.split(',').map(column).collect()
        } else if sql.starts_with("UPDATE") {
            let set = sql.split_once(" SET ")?.1;
            let mut assignments = Vec::new();
            for part in top_level(set, ',') {
                let part = part.split(" WHERE ").next()?;
                assignments.push(column(part.split_once('=')?.0));
            }
            assignments
        } else {
            Vec::new()
        };
        Some((table, columns))
    }

    #[test_id(tx2rnv4)]
    #[test]
    fn every_column_a_write_statement_assigns_is_one_the_store_check_asks_for() {
        let have = std::collections::BTreeSet::new();
        let mut listed: std::collections::BTreeSet<String> = crate::admin_writer::missing_columns(&have).into_iter().collect();
        listed.extend(["leases.issue_id", "local_metadata.key", "local_metadata.value"].map(str::to_string));
        assert!(!listed.contains("issues.planted"));
        assert_eq!(assigned("UPDATE `d`.issues SET planted = CONCAT('a, b = c', x), title = 'y' WHERE id = 'z'"), Some(("issues".to_string(), vec!["planted".to_string(), "title".to_string()])));
        let mut seen = 0;
        for (what, sql) in door_statements() {
            let Some((table, columns)) = assigned(&sql) else { continue };
            for c in columns {
                seen += 1;
                assert!(listed.contains(&format!("{table}.{c}")), "{what} writes {table}.{c}, which `admin-store check` does not ask for: {sql}");
            }
        }
        assert!(seen > 100, "the check read {seen} columns");
    }

    #[test_id(tcvjlz5)]
    #[test]
    fn every_table_a_write_statement_changes_is_one_the_writer_is_granted() {
        let all: std::collections::BTreeSet<String> = ["issues", "labels", "comments", "events", "dependencies", "child_counters", "config", "leases", "local_metadata", "wisps"].iter().map(|t| t.to_string()).collect();
        let granted = crate::admin_writer::written(&all).unwrap();
        let mut seen = 0;
        let mut check = |what: &str, sql: &str| {
            let head = sql.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
            let writes = matches!(head.as_str(), "INSERT" | "REPLACE" | "UPDATE" | "DELETE");
            match tables_written(sql) {
                Some((_, t)) => {
                    seen += 1;
                    let table = t.rsplit('.').next().unwrap();
                    assert!(granted.contains(&table), "{what} writes {t}, which admin-provision-writer does not grant: {sql}");
                }
                None => assert!(!writes, "{what}: tables_written cannot read the table of: {sql}"),
            }
        };
        for (what, sql) in door_statements() {
            check(&what, &sql);
        }
        assert!(seen > 20, "the check read {seen} statements");
        assert_eq!(tables_written("INSERT IGNORE INTO `d`.dependencies (a) VALUES (1)"), Some(("INSERT", "d.dependencies".to_string())));
        assert_eq!(tables_written("REPLACE INTO d.local_metadata (k) VALUES (1)"), Some(("INSERT and DELETE", "d.local_metadata".to_string())));
        assert_eq!(tables_written("SELECT 1"), None);
    }

    #[test_id(t9t2j2u)]
    #[test]
    fn a_denied_statement_names_the_verb_the_privilege_the_table_and_the_provisioning_verb() {
        let s = script(&["dep", "add", "sp-a", "sp-b"], None);
        let mut tx = Rec { sent: vec![], fail_at: Some((2, Fail::Denied("Error 1142 (42000): command denied".into()))), conflicts: 0 };
        let e = apply("dep add", &s, &mut tx).unwrap_err();
        for part in ["dep add was not applied", "lacks ", " on beads.", "spira-lc admin-provision-writer", "Error 1142"] {
            assert!(e.contains(part), "{part}: {e}");
        }
        assert!(!tx.sent.iter().any(|q| q.starts_with("COMMIT")), "{:?}", tx.sent);
    }

    #[test_id(tht2n8h)]
    #[test]
    fn a_verb_is_named_by_its_door_words() {
        assert_eq!([verb_of(&v(&["dep", "add", "a", "b"])), verb_of(&v(&["close", "a"])), verb_of(&v(&["kv", "set", "k", "v"]))], ["dep add", "close", "kv set"]);
    }

    #[test_id(ttklzic)]
    #[test]
    fn a_verb_the_port_does_not_make_exactly_stays_bds() {
        for argv in [&["set-state", "sp-a", "branch="][..], &["set-state", "sp-a", "=x"], &["duplicate", "sp-a"], &["gate", "check"], &["kv", "set", "k"], &["remember", "--key"]] {
            assert!(Mutation::of(&v(argv), None).is_none(), "{argv:?}");
        }
        assert!(Mutation::of(&v(&["recompute-blocked"]), Some("stdin")).is_none());
    }

    #[test_id(t09dwwg)]
    #[test]
    fn a_duplicate_is_a_closed_bead_with_a_duplicates_edge_and_no_dependency_event() {
        let s = script(&["duplicate", "sp-a", "--of", "sp-b"], None);
        assert!(s.iter().any(|q| q.contains("INSERT INTO `beads`.dependencies") && q.contains("'duplicates'")), "{s:?}");
        assert!(!s.iter().any(|q| q.contains("dependency_added")), "{s:?}");
        assert!(s.iter().any(|q| q.starts_with("UPDATE `beads`.issues SET status = 'closed'")), "{s:?}");
    }

    #[test_id(tjt3rmo)]
    #[test]
    fn a_recompute_settles_every_bead_and_not_only_a_scope() {
        let s = blocked_passes("`d`", None, 2);
        assert_eq!(s.len(), 4);
        assert!(s.iter().all(|q| !q.contains("i.id IN")), "{s:?}");
        assert!(blocked_passes("`d`", Some("'x', 'y'"), 1).iter().all(|q| q.contains("i.id IN ('x', 'y')")));
    }

    fn marks(s: &[String]) -> Vec<&String> {
        s.iter().filter(|q| q.contains("SET i.is_blocked")).collect()
    }

    fn assert_scoped_by_literals(s: &[String]) {
        let marks = marks(s);
        assert_eq!(marks.len(), 6, "{s:?}");
        for q in marks {
            let scope = q.split("i.id IN (").nth(1).and_then(|r| r.split(')').next()).unwrap_or_else(|| panic!("no scope: {q}"));
            assert!(!scope.to_uppercase().contains("SELECT") && scope.starts_with('\''), "{q}");
        }
    }

    #[test_id(tmcm3a3)]
    #[test]
    fn every_write_that_settles_blocked_scopes_its_updates_by_a_literal_id_list() {
        for args in [&["dep", "add", "sp-a", "sp-b", "--type", "blocks"][..], &["dep", "remove", "sp-a", "sp-b"], &["close", "sp-a"], &["update", "sp-a", "--status", "open"]] {
            assert_scoped_by_literals(&script(args, None));
        }
        let Some(Op::Create(c)) = Mutation::of(&v(&["create", "T", "--deps", "sp-b"]), None).unwrap().ops.into_iter().next() else { panic!() };
        assert_scoped_by_literals(&create_script("`beads`", &Env::default(), &c, "sp-n", &[], None, &["sp-n".to_string()]));
    }

    #[test_id(tr5aur7)]
    #[test]
    fn an_empty_reach_emits_no_blocked_update() {
        assert!(settle_blocked("`d`", &[]).is_empty());
    }

    fn fake_graph(edges: &'static [(&'static str, &'static str, &'static str)]) -> impl FnMut(&str) -> Result<Vec<String>, String> {
        move |sql| {
            let parents_only = sql.contains("type = 'parent-child'");
            let list = sql.split("depends_on_issue_id IN (").nth(1).unwrap().trim_end_matches(')');
            Ok(edges.iter().filter(|(_, on, ty)| list.contains(&format!("'{on}'")) && (!parents_only || *ty == "parent-child")).map(|(from, _, _)| from.to_string()).collect())
        }
    }

    const REACH_EDGES: &[(&str, &str, &str)] = &[("dep", "sp-a", "blocks"), ("child", "dep", "parent-child"), ("grand", "child", "parent-child"), ("great", "grand", "parent-child"), ("other", "elsewhere", "blocks"), ("blocker", "child", "blocks")];

    #[test_id(ty6cgtx)]
    #[test]
    fn the_reach_issues_flat_literal_lookups_only() {
        let mut sent = Vec::new();
        reach_with("`d`", &["sp-a", "sp-b"], |sql| {
            sent.push(sql.to_string());
            fake_graph(REACH_EDGES)(sql)
        })
        .unwrap();
        assert!(sent.len() <= 3, "{sent:?}");
        for q in &sent {
            assert!(q.contains("depends_on_issue_id IN ('"), "{q}");
            assert!(!q.contains("IN (SELECT"), "{q}");
        }
    }

    #[test_id(ths8b5c)]
    #[test]
    fn the_reach_is_seeds_dependents_and_two_levels_of_descendants() {
        let mut got = reach_with("`d`", &["sp-a", "sp-b"], fake_graph(REACH_EDGES)).unwrap();
        got.sort();
        assert_eq!(got, ["child", "dep", "grand", "sp-a", "sp-b"]);
    }

    #[test_id(t4o8tzy)]
    #[test]
    fn a_reach_with_nothing_new_issues_no_further_statement() {
        let mut n = 0;
        let got = reach_with("`d`", &["sp-z"], |sql| {
            n += 1;
            fake_graph(REACH_EDGES)(sql)
        })
        .unwrap();
        assert_eq!((n, got), (1, vec!["sp-z".to_string()]));
    }

    #[test_id(tebprls)]
    #[test]
    fn a_create_reads_its_flags_as_bd_does() {
        let c = |args: &[&str], body| match Mutation::of(&v(&[&["create"], args].concat()), body).unwrap().ops.remove(0) {
            Op::Create(c) => *c,
            other => panic!("{other:?}"),
        };
        let made = c(&["T", "-p", "P1", "-l", " a, b,a,", "--label", "c", "--silent", "--body-file", "-"], Some("the body"));
        assert_eq!((made.title.as_str(), made.priority, made.shape), ("T", 1, Shape::Id));
        assert_eq!(made.labels, ["a", "b", "c"]);
        assert_eq!(made.description, "the body");
        assert_eq!((made.issue_type.as_str(), made.status.as_str()), ("task", "open"));
        assert_eq!(c(&["--title", "T", "--json", "--silent"], None).shape, Shape::Json);
        assert_eq!(c(&["T"], None).shape, Shape::Text);
        let wired = c(&["T", "--deps", "sp-b, relates-to:sp-c"], None);
        assert_eq!(wired.deps, [("blocks".to_string(), "sp-b".to_string()), ("relates-to".to_string(), "sp-c".to_string())]);
    }

    #[test_id(tcfa1uj)]
    #[test]
    fn a_priority_is_a_digit_or_p_and_a_digit() {
        assert_eq!([priority_of("2"), priority_of("P0"), priority_of("p4"), priority_of("5"), priority_of("high"), priority_of("")], [Some(2), Some(0), Some(4), None, None, None]);
    }

    #[test_id(t2jyl8g)]
    #[test]
    fn a_close_files_one_event_and_the_close_columns_only_for_a_bead_that_is_open() {
        let s = script(&["close", "sp-a", "--reason", "it's done"], None);
        assert!(s[1].starts_with("REPLACE INTO `beads`.local_metadata"), "{}", s[1]);
        let event = s.iter().find(|q| q.contains("'closed', 'spira-lc'")).unwrap();
        assert!(event.contains("'', 'it\\'s done', NULL") && event.contains("status <> 'closed'"), "{event}");
        let update = s.iter().find(|q| q.starts_with("UPDATE `beads`.issues SET status = 'closed'")).unwrap();
        assert!(update.contains("closed_at = UTC_TIMESTAMP()") && update.contains("close_reason = 'it\\'s done'") && update.contains("AND status <> 'closed'"), "{update}");
        assert!(s.iter().any(|q| q == "DELETE FROM `beads`.leases WHERE issue_id = 'sp-a'"));
        assert_eq!(s.last().map(String::as_str), Some("COMMIT"));
        let bare = Env { leases: false, local_metadata: false };
        let t = Mutation::of(&v(&["close", "sp-a"]), None).unwrap().script_in("`beads`", &bare, &["sp-a".to_string()]);
        assert!(!t.iter().any(|q| q.contains("leases") || q.contains("local_metadata")), "{t:?}");
    }

    #[test_id(tpgkv20)]
    #[test]
    fn a_dependency_edge_is_keyed_as_bd_keys_it_and_filed_once() {
        let s = script(&["dep", "add", "sp-a", "sp-b", "--type", "discovered-from"], None);
        let row = s.iter().find(|q| q.starts_with("INSERT INTO `beads`.dependencies")).unwrap();
        assert!(row.contains(&format!("SELECT '{}', 'sp-a', 'sp-b', 'discovered-from'", ids::dependency_id("sp-a", "sp-b"))), "{row}");
        assert!(row.contains("WHERE NOT EXISTS"), "an edge that is there is not added again: {row}");
        let event = s.iter().find(|q| q.contains("'dependency_added'")).unwrap();
        assert!(event.contains("'', 'Added dependency: sp-a discovered-from sp-b', NULL"), "{event}");
        assert!(!s.iter().any(|q| q.contains("is_blocked")), "a discovered-from edge blocks nothing");
        let blocks = script(&["dep", "add", "sp-a", "sp-b"], None);
        assert!(blocks.iter().any(|q| q.contains("'blocks'") && q.starts_with("INSERT INTO `beads`.dependencies")));
        assert!(blocks.iter().any(|q| q.contains("SET i.is_blocked = 1")));
        let parent = script(&["dep", "add", "sp-a", "sp-b", "--type", "parent-child"], None);
        assert!(parent.iter().any(|q| q.starts_with("REPLACE INTO `beads`.local_metadata")));
    }

    #[test_id(tzzxqy9)]
    #[test]
    fn a_relate_adds_both_edges_and_a_supersede_adds_one_without_an_event_and_closes() {
        let r = script(&["dep", "relate", "sp-a", "sp-b"], None);
        assert_eq!(r.iter().filter(|q| q.starts_with("INSERT INTO `beads`.dependencies")).count(), 2);
        assert_eq!(r.iter().filter(|q| q.contains("'dependency_added'")).count(), 2);
        let s = script(&["supersede", "sp-a", "--with", "sp-b"], None);
        assert_eq!(s.iter().filter(|q| q.starts_with("INSERT INTO `beads`.dependencies")).count(), 1);
        assert!(s.iter().any(|q| q.contains("'supersedes'")));
        assert!(!s.iter().any(|q| q.contains("'dependency_added'")), "bd files no dependency event for a supersede");
        assert!(s.iter().any(|q| q.starts_with("UPDATE `beads`.issues SET status = 'closed'")));
    }

    #[test_id(txl1md1)]
    #[test]
    fn a_status_and_an_assignee_edit_make_one_event_and_a_reopen_clears_what_a_close_set() {
        let s = script(&["update", "sp-a", "--status", "open", "--assignee", ""], None);
        assert_eq!(s.iter().filter(|q| q.starts_with("INSERT INTO `beads`.events")).count(), 1);
        let event = s.iter().find(|q| q.starts_with("INSERT INTO `beads`.events")).unwrap();
        assert!(event.contains("CASE WHEN @was0 = 'closed' THEN 'reopened' ELSE 'status_changed' END"), "{event}");
        let update = s.iter().find(|q| q.starts_with("UPDATE `beads`.issues SET status")).unwrap();
        assert!(update.contains("status = 'open'") && update.contains("assignee = ''") && update.contains("closed_at = CASE WHEN @was0 = 'closed' THEN NULL"), "{update}");
        let settled = s.iter().find(|q| q.starts_with("UPDATE `beads`.events SET new_value")).unwrap();
        assert!(settled.contains("JSON_OBJECT('status', status, 'assignee', assignee)"), "{settled}");
        assert!(s.iter().any(|q| q.starts_with("DELETE FROM `beads`.leases WHERE issue_id = 'sp-a' AND NOT ('open' = 'in_progress'")));
        let d = script(&["update", "sp-a", "--body-file", "-"], Some("new text\n"));
        assert!(d.iter().any(|q| q.contains("description = 'new text\n'")), "{d:?}");
        assert!(!d.iter().any(|q| q.contains("leases")), "a description edit holds no lease");
    }

    #[test_id(t7c7o45)]
    #[test]
    fn a_create_inserts_the_row_its_created_event_its_labels_and_its_edges_in_one_transaction() {
        let m = Mutation::of(&v(&["create", "T", "--parent", "sp-p", "-l", "x"]), None).unwrap();
        let c = m.create().unwrap();
        let s = create_script("`beads`", &Env::default(), c, "sp-p.2", &["x".to_string(), "y".to_string()], Some(&("sp-p".to_string(), 2)), &["sp-p.2".to_string()]);
        assert_eq!(s.first().map(String::as_str), Some("BEGIN"));
        assert_eq!(s.last().map(String::as_str), Some("COMMIT"));
        let insert = s.iter().find(|q| q.starts_with("INSERT INTO `beads`.issues")).unwrap();
        assert!(insert.contains("VALUES ('sp-p.2', '") && insert.contains("'T', '', '', '', '', 'open', 2, 'task', NULL, UTC_TIMESTAMP(), 'spira-lc'"), "{insert}");
        assert!(s.iter().any(|q| q.contains("'created', 'spira-lc', '', '', NULL")));
        assert_eq!(s.iter().filter(|q| q.starts_with("INSERT IGNORE INTO `beads`.labels")).count(), 2);
        assert!(s.iter().any(|q| q.contains("INSERT INTO `beads`.child_counters (parent_id, last_child) VALUES ('sp-p', 2) ON DUPLICATE KEY UPDATE last_child = 2")));
        let edge = s.iter().find(|q| q.starts_with("INSERT INTO `beads`.dependencies")).unwrap();
        assert!(edge.contains(&format!("SELECT '{}', 'sp-p.2', 'sp-p', 'parent-child'", ids::dependency_id("sp-p.2", "sp-p"))), "{edge}");
        assert!(s.iter().any(|q| q.contains("SET i.is_blocked = 1")), "a child of a blocked parent is blocked");
        assert!(s[1].starts_with("REPLACE INTO `beads`.local_metadata"));
    }

    #[test_id(t5hogro)]
    #[test]
    fn a_create_prints_what_bd_prints_for_the_same_flags() {
        let c = |args: &[&str]| Mutation::of(&v(&[&["create"], args].concat()), None).unwrap().create().unwrap().clone();
        let labels = ["a".to_string()];
        assert_eq!(c(&["T", "--silent"]).stdout("sp-x1", &[], 0), "sp-x1\n");
        let json: serde_json::Value = serde_json::from_str(&c(&["T", "--json", "-p", "1"]).stdout("sp-x1", &labels, 86_400 * 365)).unwrap();
        assert_eq!((json["id"].as_str(), json["priority"].as_u64(), json["created_at"].as_str()), (Some("sp-x1"), Some(1), Some("1971-01-01T00:00:00Z")));
        assert_eq!(json["labels"][0], "a");
        assert_eq!(bead::parse_created_id(&c(&["T", "--json"]).stdout("sp-x1", &[], 0)).as_deref(), Some("sp-x1"));
        assert!(c(&["T"]).stdout("sp-x1", &[], 0).starts_with("✓ Created issue: sp-x1"));
    }

    #[test_id(tfgd8bw)]
    #[test]
    fn a_label_add_is_one_transaction_that_records_the_event_before_the_row_and_leaves_the_issue_row_alone() {
        let s = script(&["label", "add", "sp-a", "overseer"], None);
        assert_eq!(
            s,
            vec![
                "BEGIN",
                "INSERT INTO `beads`.events (id, issue_id, event_type, actor, old_value, new_value, comment, created_at) SELECT UUID(), 'sp-a', 'label_added', 'spira-lc', NULL, NULL, 'Added label: overseer', UTC_TIMESTAMP() FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM `beads`.labels WHERE issue_id = 'sp-a' AND label = 'overseer')",
                "INSERT IGNORE INTO `beads`.labels (issue_id, label) VALUES ('sp-a', 'overseer')",
                "COMMIT",
            ]
        );
    }

    #[test_id(t402tw9)]
    #[test]
    fn a_label_remove_and_an_update_that_carries_it_make_the_same_changes() {
        let a = script(&["label", "remove", "sp-a", "branch:spira/sp-a"], None);
        let b = script(&["update", "sp-a", "--remove-label", "branch:spira/sp-a"], None);
        assert_eq!(a, b);
        assert!(a[2].starts_with("DELETE FROM `beads`.labels WHERE issue_id = 'sp-a' AND label = 'branch:spira/sp-a'"));
    }

    
    #[test_id(tx55xyp)]
    #[test]
    fn a_draft_note_merges_only_over_the_notes_it_read_and_appends_otherwise() {
        let m = Mutation { argv: vec![], body: None, id: "sp-a".into(), ops: vec![Op::DraftNote { read: "r'1".into(), merged: "m".into(), text: "t".into() }], forced: false };
        let q = m.script_in("`beads`", &Env { leases: false, local_metadata: false }, &[]).join("\n");
        assert!(q.contains("CASE WHEN COALESCE(notes, '') = 'r\\'1' THEN 'm' WHEN notes IS NULL OR notes = '' THEN 't' ELSE CONCAT(notes, CHAR(10), 't') END"), "{q}");
    }

    #[test_id(toqnqfz)]
    #[test]
    fn a_note_appends_a_comment_inserts_metadata_merges_and_priority_sets() {
        let n = script(&["note", "sp-a", "--stdin"], Some("body\n"));
        assert!(n[3].contains("CONCAT(notes, CHAR(10), 'body')"), "{}", n[3]);
        let c = script(&["comments", "add", "sp-a", "hi"], None);
        assert!(c[1].starts_with("INSERT INTO `beads`.comments (id, issue_id, author, text, created_at) VALUES (UUID(), 'sp-a', 'spira-lc', 'hi',"));
        assert_eq!(c.len(), 3, "a comment files no event and leaves the issue row alone, as bd does: {c:?}");
        let m = script(&["update", "sp-a", "--set-metadata", "k=v"], None);
        assert!(m[3].contains("JSON_SET(COALESCE(metadata, JSON_OBJECT()), '$.\"k\"', 'v')"), "{}", m[3]);
        let p = script(&["update", "sp-a", "-p", "2"], None);
        assert_eq!(p[3], "UPDATE `beads`.issues SET priority = 2 WHERE id = 'sp-a'");
    }

    #[test_id(t0o2faq)]
    #[test]
    fn an_update_event_carries_the_issue_as_it_was_and_the_value_it_became() {
        for s in [script(&["note", "sp-a", "t"], None), script(&["update", "sp-a", "--set-metadata", "k=v"], None), script(&["update", "sp-a", "-p", "1"], None)] {
            assert_eq!(s[1], "SET @event0 = UUID()");
            assert!(s[2].starts_with("INSERT INTO `beads`.events (id, issue_id, event_type, actor, old_value, new_value, comment, created_at) VALUES (@event0, 'sp-a', 'updated', 'spira-lc', (SELECT JSON_MERGE_PATCH("), "{}", s[2]);
            assert!(s[2].contains("'labels', NULLIF((SELECT JSON_ARRAYAGG(label)"), "{}", s[2]);
            assert!(s[4].starts_with("UPDATE `beads`.events SET new_value = (SELECT JSON_OBJECT(") && s[4].ends_with("WHERE id = @event0"), "{}", s[4]);
            assert!(s[5].contains("row_lock = CAST(") && s[5].contains("updated_at = UTC_TIMESTAMP()"), "{}", s[5]);
        }
    }

    #[test_id(t4stqo9)]
    #[test]
    fn a_metadata_number_boolean_or_null_is_stored_as_that_json_value_and_anything_else_as_text() {
        for (v, json) in [("5", true), ("-3", true), ("1.5", true), ("true", true), ("null", true), ("hi", false), ("\"q\"", false), ("[1,2]", false), ("{\"x\":1}", false), (" 5", false), ("", false)] {
            assert_eq!(metadata_value(v).starts_with("CAST("), json, "{v:?}");
        }
    }

    #[test_id(ttzca40)]
    #[test]
    fn a_quote_in_a_note_cannot_end_the_string() {
        let s = script(&["note", "sp-a", "--stdin"], Some("x'; DROP TABLE issues; --"));
        assert!(s[3].contains(r"'x\'; DROP TABLE issues; --'"), "{}", s[3]);
        assert!(!s[3].contains("x'; DROP"));
    }

    #[test_id(tv5rirn)]
    #[test]
    fn a_failure_on_the_second_statement_sends_no_commit() {
        let s = script(&["label", "add", "sp-a", "x"], None);
        let mut tx = Rec { sent: vec![], fail_at: Some((2, Fail::Other("boom".into()))), conflicts: 0 };
        assert_eq!(apply("x", &s, &mut tx), Err("boom".into()));
        assert_eq!(tx.sent.len(), 2);
        assert!(!tx.sent.iter().any(|q| q == "COMMIT"));
    }

    #[test_id(t4upfsn)]
    #[test]
    fn writes_owed_before_the_committer_wakes_cost_one_commit() {
        let c = Committer::new();
        c.owe();
        c.owe();
        c.owe();
        c.take();
        assert!(!*c.owed.lock().unwrap(), "one take settles every write owed so far");
    }

    #[test_id(t3tedfw)]
    #[test]
    fn a_conflict_retries_once_then_gives_up() {
        let s = script(&["label", "add", "sp-a", "x"], None);
        let mut tx = Rec { sent: vec![], fail_at: None, conflicts: 1 };
        assert_eq!(apply("x", &s, &mut tx), Ok(()));
        assert_eq!(tx.sent.len(), s.len() * 2);
        let mut tx = Rec { sent: vec![], fail_at: None, conflicts: 2 };
        assert!(apply("x", &s, &mut tx).is_err());
        assert_eq!(tx.sent.len(), s.len() * 2);
    }
}
