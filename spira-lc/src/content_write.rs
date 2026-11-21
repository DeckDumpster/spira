//! The content door's writes as one SQL transaction on the beads database, so lc-serve applies
//! them without forking `bd`: labels, notes, comments, metadata, priority, status, assignee,
//! description, create, close, dependency edges and supersede. Each verb makes the row changes
//! bd makes for it: the row itself, `updated_at`, and the `events` row bd files (none for a
//! comment). A write whose preconditions bd would refuse, or that the port does not cover, is
//! left to bd, which refuses it in its own words.

use crate::rows::escape;

mod ids;

const ACTOR: &str = "spira-lc";

const STATUSES: [&str; 7] = ["open", "in_progress", "blocked", "deferred", "closed", "pinned", "hooked"];
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
}

impl Op {
    fn is_column(&self) -> bool {
        matches!(self, Op::Status(_) | Op::Assignee(_) | Op::Description(_))
    }

    /// Whether bd rewrites the issue row (`updated_at`, `row_lock`) for it.
    fn touches_row(&self) -> bool {
        !matches!(
            self,
            Op::LabelAdd(_) | Op::LabelRemove(_) | Op::Comment(_) | Op::Status(_) | Op::Assignee(_) | Op::Description(_) | Op::Create(_) | Op::Close { .. } | Op::DepAdd { .. } | Op::DepRemove { .. } | Op::DepRelate { .. } | Op::Supersede { .. }
        )
    }
}

/// A door write lc-serve applies itself; `argv` and `body` are kept for the bd fork a store
/// with no writer configured still takes.
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
        let (id, ops) = match (a.as_slice(), body) {
            (["label", "add", id, l], None) => (*id, vec![Op::LabelAdd((*l).into())]),
            (["label", "remove", id, l], None) => (*id, vec![Op::LabelRemove((*l).into())]),
            (["note", id, "--stdin"], Some(b)) => (*id, vec![Op::Note(b.trim_end_matches('\n').into())]),
            (["note", id, t], None) if *t != "--stdin" => (*id, vec![Op::Note((*t).into())]),
            (["comments", "add", id, t], None) => (*id, vec![Op::Comment((*t).into())]),
            (["update", id, flags @ ..], body) if !flags.is_empty() => (*id, update_ops(flags, body)?),
            (["create", rest @ ..], body) => ("", vec![Op::Create(Box::new(create_spec(rest, body)?))]),
            (["close", id], None) => (*id, vec![Op::Close { reason: String::new(), force: false }]),
            (["close", id, "--force"], None) => (*id, vec![Op::Close { reason: String::new(), force: true }]),
            (["close", id, "--reason", r], None) if !r.is_empty() => (*id, vec![Op::Close { reason: (*r).into(), force: false }]),
            (["close", id, "--force", "--reason", r], None) | (["close", id, "--reason", r, "--force"], None) if !r.is_empty() => (*id, vec![Op::Close { reason: (*r).into(), force: true }]),
            (["dep", "add", id, on], None) => (*id, vec![Op::DepAdd { on: (*on).into(), kind: "blocks".into() }]),
            (["dep", "add", id, on, "--type", t], None) if matches!(*t, "blocks" | "relates-to" | "parent-child" | "discovered-from") => (*id, vec![Op::DepAdd { on: (*on).into(), kind: (*t).into() }]),
            (["dep", "remove", id, on], None) => (*id, vec![Op::DepRemove { on: (*on).into() }]),
            (["dep", "relate", id, other], None) => (*id, vec![Op::DepRelate { other: (*other).into() }]),
            (["supersede", id, "--with", with], None) => (*id, vec![Op::Supersede { with: (*with).into() }]),
            _ => return None,
        };
        if id.starts_with('-') || (id.is_empty() && !matches!(ops.first(), Some(Op::Create(_)))) {
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
                _ => None,
            })
            .collect()
    }

    /// The transaction: `BEGIN`, the row changes, `COMMIT`.
    #[cfg(test)]
    pub fn script(&self, db: &str) -> Vec<String> {
        self.script_in(db, &Env::default())
    }

    pub fn script_in(&self, db: &str, env: &Env) -> Vec<String> {
        let id = escape(&self.id);
        let event = |kind: &str, comment: &str, guard: &str| event_row(db, &id, kind, "NULL", "NULL", comment, guard);
        let lit = |s: &str| format!("'{}'", escape(s));
        let before = before_json(db, &id);
        let mut s = vec!["BEGIN".to_string()];
        let mut columns_done = false;
        for (n, op) in self.ops.iter().enumerate() {
            let settled = |column: &str| format!("UPDATE {db}.events SET new_value = (SELECT JSON_OBJECT('{column}', {column}) FROM {db}.issues WHERE id = '{id}') WHERE id = @event{n}");
            let filed = |old: &str| format!("INSERT INTO {db}.events (id, issue_id, event_type, actor, old_value, new_value, comment, created_at) VALUES (@event{n}, '{id}', 'updated', '{ACTOR}', {old}, NULL, NULL, UTC_TIMESTAMP())");
            match op {
                Op::LabelAdd(l) => {
                    let e = escape(l);
                    s.push(event("label_added", &lit(&format!("Added label: {l}")), &format!(" WHERE NOT EXISTS (SELECT 1 FROM {db}.labels WHERE issue_id = '{id}' AND label = '{e}')")));
                    s.push(format!("INSERT IGNORE INTO {db}.labels (issue_id, label) VALUES ('{id}', '{e}')"));
                }
                Op::LabelRemove(l) => {
                    let e = escape(l);
                    s.push(event("label_removed", &lit(&format!("Removed label: {l}")), &format!(" WHERE EXISTS (SELECT 1 FROM {db}.labels WHERE issue_id = '{id}' AND label = '{e}')")));
                    s.push(format!("DELETE FROM {db}.labels WHERE issue_id = '{id}' AND label = '{e}'"));
                }
                Op::Note(t) => {
                    s.push(format!("SET @event{n} = UUID()"));
                    s.push(filed(&before));
                    s.push(format!("UPDATE {db}.issues SET notes = CASE WHEN notes IS NULL OR notes = '' THEN {t} ELSE CONCAT(notes, CHAR(10), {t}) END WHERE id = '{id}'", t = lit(t)));
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
                        s.extend(columns_sql(db, &id, &self.ops, n, env));
                    }
                }
                Op::Close { reason, .. } => s.extend(close_sql(db, &id, reason, env)),
                Op::DepAdd { on, kind } => s.extend(dep_add_sql(db, &id, &escape(on), kind, true, env)),
                Op::DepRemove { on } => s.extend(dep_remove_sql(db, &id, &escape(on))),
                Op::DepRelate { other } => {
                    let other = escape(other);
                    s.extend(dep_add_sql(db, &id, &other, "relates-to", true, env));
                    s.extend(dep_add_sql(db, &other, &id, "relates-to", true, env));
                }
                Op::Supersede { with } => {
                    s.extend(dep_add_sql(db, &id, &escape(with), "supersedes", false, env));
                    s.extend(close_sql(db, &id, "", env));
                }
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
fn columns_sql(db: &str, id: &str, ops: &[Op], n: usize, env: &Env) -> Vec<String> {
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
        s.extend(settle_blocked(db, &[id]));
    }
    s
}

fn touch_coordination(db: &str, parent: &str, env: &Env) -> Vec<String> {
    if !env.local_metadata {
        return Vec::new();
    }
    ["dependencies", "wisp_dependencies"].iter().map(|tier| format!("REPLACE INTO {db}.local_metadata (`key`, value) VALUES ('{}', CAST({ROW_LOCK} AS CHAR))", ids::coordination_key(&unescape(parent), tier))).collect()
}

fn close_sql(db: &str, id: &str, reason: &str, env: &Env) -> Vec<String> {
    let r = escape(reason);
    let open = format!(" WHERE EXISTS (SELECT 1 FROM {db}.issues WHERE id = '{id}' AND status <> 'closed')");
    let mut s = touch_coordination(db, id, env);
    s.push(event_row(db, id, "closed", "''", &format!("'{r}'"), "NULL", &open));
    s.push(format!("UPDATE {db}.issues SET status = 'closed', closed_at = UTC_TIMESTAMP(), updated_at = UTC_TIMESTAMP(), close_reason = '{r}', closed_by_session = '', row_lock = {ROW_LOCK} WHERE id = '{id}' AND status <> 'closed'"));
    if env.leases {
        s.push(format!("DELETE FROM {db}.leases WHERE issue_id = '{id}'"));
    }
    s.extend(settle_blocked(db, &[id]));
    s
}

const TARGET: &str = "COALESCE(depends_on_issue_id, depends_on_wisp_id, depends_on_external)";

fn dep_add_sql(db: &str, id: &str, on: &str, kind: &str, emit: bool, env: &Env) -> Vec<String> {
    let k = escape(kind);
    let absent = format!(" WHERE NOT EXISTS (SELECT 1 FROM {db}.dependencies WHERE issue_id = '{id}' AND {TARGET} = '{on}')");
    let mut s = Vec::new();
    if emit {
        s.push(event_row(db, id, "dependency_added", "''", &format!("'Added dependency: {id} {k} {on}'"), "NULL", &absent));
    }
    s.push(dependency_row(db, id, on, kind, &absent));
    if kind == "parent-child" {
        s.extend(touch_coordination(db, on, env));
    }
    if BLOCKING.contains(&kind) {
        s.extend(settle_blocked(db, &[id, on]));
    }
    s
}

fn dependency_row(db: &str, id: &str, on: &str, kind: &str, guard: &str) -> String {
    format!(
        "INSERT INTO {db}.dependencies (id, issue_id, depends_on_issue_id, type, created_at, created_by, metadata, thread_id) SELECT '{}', '{id}', '{on}', '{}', UTC_TIMESTAMP(), '{ACTOR}', '{{}}', '' FROM DUAL{guard}",
        ids::dependency_id(&unescape(id), &unescape(on)),
        escape(kind)
    )
}

fn unescape(s: &str) -> String {
    s.replace("\\'", "'").replace("\\\\", "\\")
}

fn dep_remove_sql(db: &str, id: &str, on: &str) -> Vec<String> {
    let present = format!(" WHERE EXISTS (SELECT 1 FROM {db}.dependencies WHERE issue_id = '{id}' AND {TARGET} = '{on}')");
    let mut s = vec![event_row(db, id, "dependency_removed", "''", &format!("'Removed dependency on {on}'"), "NULL", &present), format!("DELETE FROM {db}.dependencies WHERE issue_id = '{id}' AND {TARGET} = '{on}'")];
    s.extend(settle_blocked(db, &[id, on]));
    s
}

/// bd's `is_blocked` maintenance for the beads a change reaches: its dependents, and their
/// parent-child descendants three levels down. A bead with a `waits-for` edge or a wisp target
/// is left as it is; the port does not read those.
fn settle_blocked(db: &str, seeds: &[&str]) -> Vec<String> {
    let literals = seeds.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>();
    let list = literals.join(", ");
    let named = literals.iter().map(|l| format!("SELECT {l}")).collect::<Vec<_>>().join(" UNION ");
    let level0 = format!("{named} UNION SELECT issue_id FROM {db}.dependencies WHERE depends_on_issue_id IN ({list})");
    let below = |level: &str| format!("{level} UNION SELECT issue_id FROM {db}.dependencies WHERE type = 'parent-child' AND depends_on_issue_id IN ({level})");
    let level2 = below(&below(&level0));
    let untouched = format!("NOT EXISTS (SELECT 1 FROM {db}.dependencies g WHERE g.issue_id = i.id AND (g.type = 'waits-for' OR g.depends_on_wisp_id IS NOT NULL))");
    let held = format!(
        "(EXISTS (SELECT 1 FROM {db}.dependencies d JOIN {db}.issues t ON t.id = d.depends_on_issue_id WHERE d.issue_id = i.id AND d.type IN ('blocks', 'conditional-blocks') AND t.status <> 'closed' AND t.status <> 'pinned') OR EXISTS (SELECT 1 FROM {db}.dependencies d JOIN {db}.issues p ON p.id = d.depends_on_issue_id WHERE d.issue_id = i.id AND d.type = 'parent-child' AND p.is_blocked = 1))"
    );
    let unmark = format!("UPDATE {db}.issues i SET i.is_blocked = 0, i.updated_at = i.updated_at WHERE i.id IN ({level2}) AND i.is_blocked = 1 AND {untouched} AND (i.status = 'closed' OR i.status = 'pinned' OR NOT {held})");
    let mark = format!("UPDATE {db}.issues i SET i.is_blocked = 1, i.updated_at = i.updated_at WHERE i.id IN ({level2}) AND i.is_blocked = 0 AND {untouched} AND i.status <> 'closed' AND i.status <> 'pinned' AND {held}");
    (0..3).flat_map(|_| [unmark.clone(), mark.clone()]).collect()
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
    (titled && from_body == body.is_some() && !c.title.is_empty() && bounded && consistent && parent_edge_free && c.status != "closed" && STATUSES.contains(&c.status.as_str())).then(|| {
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
    Other(String),
}

/// Where a transaction's statements go, in order, on one session.
pub trait Tx {
    fn send(&mut self, sql: &str) -> Result<(), Fail>;
}

fn once(script: &[String], tx: &mut dyn Tx) -> Result<(), Fail> {
    script.iter().try_for_each(|s| tx.send(s))
}

/// Applies `script`; a conflict is retried once. A statement that fails ends the transaction
/// with no `COMMIT` sent.
pub fn apply(script: &[String], tx: &mut dyn Tx) -> Result<(), String> {
    match once(script, tx) {
        Err(Fail::Conflict) => match once(script, tx) {
            Err(Fail::Conflict) => Err("the write conflicted with another writer twice; nothing was applied".into()),
            other => other.map_err(|f| format!("{f:?}")),
        },
        Err(Fail::Other(e)) => Err(e),
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

struct Writer {
    conn: crate::db::Conn,
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

/// The writer connection, or `None` when no writer credential is configured: the door then
/// forks bd for these verbs as it always did.
fn writer() -> Option<Result<&'static Writer, String>> {
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
/// or when bd must answer: a write whose preconditions bd would refuse, or one the port does
/// not cover, goes to bd, which refuses or makes it in its own words.
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
    let db = &w.db;
    let mut named = vec![m.bead()];
    named.extend(m.others());
    let found = rows(w, &format!("SELECT id, status, COALESCE(assignee, '') AS assignee, description, is_blocked FROM {db}.issues WHERE id IN ({})", in_list(&named)))?;
    let row_of = |id: &str| found.iter().find(|r| cell(r, "id") == id);
    let Some(me) = row_of(m.bead()) else {
        return Err(format!("no issue found matching {:?}", m.bead()));
    };
    if m.others().iter().any(|o| row_of(o).is_none()) {
        return Ok(None);
    }
    let Some(m) = preflight(w, m, me)? else {
        return Ok(None);
    };
    if m.ops.is_empty() {
        return Ok(Some(String::new()));
    }
    let env = env_of(w)?;
    w.conn.transaction(|tx| apply(&m.script_in(db, &env), tx)).and_then(|r| r)?;
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
            Op::Status(v) if !STATUSES.contains(&v.as_str()) || v == "closed" => return Ok(None),
            Op::Status(v) if *v == status => continue,
            Op::Assignee(v) if v.chars().count() > 255 => return Ok(None),
            Op::Assignee(v) if *v == holder => continue,
            Op::Assignee(_) if !m.forced && !holder.is_empty() && status == "in_progress" => return Ok(None),
            Op::Description(v) if *v == cell(me, "description") => continue,
            Op::Close { force: false, .. } if status != "closed" => {
                let children = count(w, &format!("SELECT COUNT(DISTINCT d.issue_id) AS n FROM {db}.dependencies d JOIN {db}.issues c ON c.id = d.issue_id WHERE d.depends_on_issue_id = '{id}' AND d.type = 'parent-child' AND c.status <> 'closed'"))?;
                if children > 0 || cell(me, "is_blocked") == "1" {
                    return Ok(None);
                }
            }
            Op::DepAdd { on, kind } => {
                if on == m.bead() || edge(m.bead(), on)?.is_some_and(|t| t != *kind) || (BLOCKING.contains(&kind.as_str()) && cycle(w, m.bead(), on)?) {
                    return Ok(None);
                }
                if kind == "blocks" && (hierarchy(w, m.bead(), on)? || hierarchy(w, on, m.bead())? || is_descendant(m.bead(), on) || is_descendant(on, m.bead())) {
                    return Ok(None);
                }
            }
            Op::DepRelate { other } => {
                if other == m.bead() || edge(m.bead(), other)?.is_some_and(|t| t != "relates-to") || edge(other, m.bead())?.is_some_and(|t| t != "relates-to") {
                    return Ok(None);
                }
            }
            Op::Supersede { with } => {
                let against = with == m.bead() || edge(m.bead(), with)?.is_some_and(|t| t != "supersedes");
                if against {
                    return Ok(None);
                }
            }
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
    let present = rows(w, &format!("SELECT id FROM {db}.issues WHERE id IN ({})", in_list(&named)))?;
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
        let script = create_script(db, &env, c, &id, &labels, child.as_ref());
        match w.conn.transaction(|tx| apply(&script, tx)).and_then(|r| r) {
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

fn create_script(db: &str, env: &Env, c: &Create, id: &str, labels: &[String], child: Option<&(String, u64)>) -> Vec<String> {
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
        s.extend(touch_coordination(db, &escape(p), env));
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
        s.push(dependency_row(db, &q, &escape(target), kind, ""));
        blocking |= BLOCKING.contains(&kind);
    }
    if blocking {
        s.extend(settle_blocked(db, &[&q]));
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
                let stamp = rfc3339(now);
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

/// An RFC 3339 UTC timestamp, from epoch seconds.
fn rfc3339(epoch: u64) -> String {
    let (days, rem) = ((epoch / 86_400) as i64, epoch % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
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

    #[test]
    fn only_the_served_verbs_are_mutations() {
        for argv in [
            &["set-state", "sp-a", "branch=x"][..],
            &["update", "sp-a", "--set-metadata", "a\"b=c"],
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
        ] {
            assert!(Mutation::of(&v(argv), body).is_some(), "{argv:?}");
        }
    }

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

    #[test]
    fn a_priority_is_a_digit_or_p_and_a_digit() {
        assert_eq!([priority_of("2"), priority_of("P0"), priority_of("p4"), priority_of("5"), priority_of("high"), priority_of("")], [Some(2), Some(0), Some(4), None, None, None]);
    }

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
        let t = Mutation::of(&v(&["close", "sp-a"]), None).unwrap().script_in("`beads`", &bare);
        assert!(!t.iter().any(|q| q.contains("leases") || q.contains("local_metadata")), "{t:?}");
    }

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

    #[test]
    fn a_create_inserts_the_row_its_created_event_its_labels_and_its_edges_in_one_transaction() {
        let m = Mutation::of(&v(&["create", "T", "--parent", "sp-p", "-l", "x"]), None).unwrap();
        let c = m.create().unwrap();
        let s = create_script("`beads`", &Env::default(), c, "sp-p.2", &["x".to_string(), "y".to_string()], Some(&("sp-p".to_string(), 2)));
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

    #[test]
    fn a_timestamp_is_written_in_utc_to_the_second() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400 + 3661), "2000-02-29T01:01:01Z");
    }

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

    #[test]
    fn a_label_remove_and_an_update_that_carries_it_make_the_same_changes() {
        let a = script(&["label", "remove", "sp-a", "branch:spira/sp-a"], None);
        let b = script(&["update", "sp-a", "--remove-label", "branch:spira/sp-a"], None);
        assert_eq!(a, b);
        assert!(a[2].starts_with("DELETE FROM `beads`.labels WHERE issue_id = 'sp-a' AND label = 'branch:spira/sp-a'"));
    }

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

    #[test]
    fn a_metadata_number_boolean_or_null_is_stored_as_that_json_value_and_anything_else_as_text() {
        for (v, json) in [("5", true), ("-3", true), ("1.5", true), ("true", true), ("null", true), ("hi", false), ("\"q\"", false), ("[1,2]", false), ("{\"x\":1}", false), (" 5", false), ("", false)] {
            assert_eq!(metadata_value(v).starts_with("CAST("), json, "{v:?}");
        }
    }

    #[test]
    fn a_quote_in_a_note_cannot_end_the_string() {
        let s = script(&["note", "sp-a", "--stdin"], Some("x'; DROP TABLE issues; --"));
        assert!(s[3].contains(r"'x\'; DROP TABLE issues; --'"), "{}", s[3]);
        assert!(!s[3].contains("x'; DROP"));
    }

    #[test]
    fn a_failure_on_the_second_statement_sends_no_commit() {
        let s = script(&["label", "add", "sp-a", "x"], None);
        let mut tx = Rec { sent: vec![], fail_at: Some((2, Fail::Other("boom".into()))), conflicts: 0 };
        assert_eq!(apply(&s, &mut tx), Err("boom".into()));
        assert_eq!(tx.sent.len(), 2);
        assert!(!tx.sent.iter().any(|q| q == "COMMIT"));
    }

    #[test]
    fn writes_owed_before_the_committer_wakes_cost_one_commit() {
        let c = Committer::new();
        c.owe();
        c.owe();
        c.owe();
        c.take();
        assert!(!*c.owed.lock().unwrap(), "one take settles every write owed so far");
    }

    #[test]
    fn a_conflict_retries_once_then_gives_up() {
        let s = script(&["label", "add", "sp-a", "x"], None);
        let mut tx = Rec { sent: vec![], fail_at: None, conflicts: 1 };
        assert_eq!(apply(&s, &mut tx), Ok(()));
        assert_eq!(tx.sent.len(), s.len() * 2);
        let mut tx = Rec { sent: vec![], fail_at: None, conflicts: 2 };
        assert!(apply(&s, &mut tx).is_err());
        assert_eq!(tx.sent.len(), s.len() * 2);
    }
}
