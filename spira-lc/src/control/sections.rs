//! `section`, `intent` and `lock` on the control store: components publish their sections,
//! the operator's apply publishes spec targets, and the reconciler's steps are intents at
//! their own keys. Every write passes the section schema's admission before its txn. Every
//! answer is JSON: the value on exit 0, `{}`-shaped not-found on 1, `{"cannot_tell": …}` on 2
//! and `{"refused": {"field", "rule"}}` on 3.

use serde_json::{json, Value};
use spira_desired_state::intent::{
    admit_put, parse_intent, parse_key, Ack, AckResult, Intent, Judgement, JudgementResult,
    INTENTS_PREFIX, INTENT_TTL_SECS, RECONCILER, RECONCILER_LOCK,
};
use spira_desired_state::section::{
    self, admit, admit_publish, caught_up, freshness, parse_ref, parse_section, registered,
    section_key, Refusal, Section, SectionMeta, Target, COMPOSE, PASS_KEY, SECTIONS_PREFIX,
    SECTION_TTL_SECS,
};
use spira_desired_state::store::format_rfc3339;
use spira_otel::metrics::{MetricsSink, Point};
use state_store::{Backend, Compare, Error, Fence, Item, Object, Request, Rev, Selector, Txn};

use super::keys;
use super::store::Store;
use crate::{flag, CANNOT_TELL, REFUSED};

pub mod lifecycle;

/// The verbs this module answers.
pub const VERBS: &[&str] = &["section", "intent", "lock", lifecycle::VERB];

pub const WRITES: &str = "spira_section_writes";
pub const REFUSALS: &str = "spira_section_refusals";

/// What one call carries besides its argv: the wall clock it is judged at, the request id its
/// writes commit under, and where its write and refusal counts go.
pub struct Call<'a> {
    pub now_ms: u64,
    pub request: &'a str,
    pub metrics: &'a MetricsSink,
}

enum Answer {
    Ok(Value),
    Missing(Value),
    Refused(Refusal),
    CannotTell(String),
}

impl From<Refusal> for Answer {
    fn from(r: Refusal) -> Self {
        Answer::Refused(r)
    }
}

fn cannot(e: impl std::fmt::Display) -> Answer {
    Answer::CannotTell(format!("cannot tell: {e}"))
}

fn refuse(field: &str, code: &'static str, rule: impl Into<String>) -> Answer {
    Answer::Refused(Refusal::new(field, code, rule))
}

fn render(a: Answer) -> (i32, String) {
    match a {
        Answer::Ok(v) => (0, v.to_string()),
        Answer::Missing(v) => (1, v.to_string()),
        Answer::Refused(r) => (
            REFUSED,
            json!({ "refused": { "field": r.field, "rule": r.rule } }).to_string(),
        ),
        Answer::CannotTell(e) => (CANNOT_TELL, json!({ "cannot_tell": e }).to_string()),
    }
}

/// `{"cannot_tell": reason}`, for the routing seam's own failures.
pub fn cannot_tell(reason: &str) -> (i32, String) {
    render(Answer::CannotTell(format!("cannot tell: {reason}")))
}

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let sub = args.get(1).map(String::as_str);
    let rest = args.get(2..).unwrap_or(&[]);
    let verb = format!("{} {}", args[0], sub.unwrap_or(""));
    let (answer, kind) = match (args[0].as_str(), sub) {
        ("section", Some("get")) => (section_get(store, rest, call), None),
        ("section", Some("list")) => (section_list(store, rest, call), None),
        ("section", Some("snapshot")) => (section_snapshot(store, call), None),
        ("section", Some("apply-spec")) => written(apply_spec(store, rest, call)),
        ("section", Some("publish-status")) => written(publish_status(store, rest, call)),
        ("intent", Some("put")) => written(intent_put(store, rest, call)),
        ("intent", Some("ack")) => written(intent_ack(store, rest, call)),
        ("intent", Some("judge")) => written(intent_judge(store, rest, call)),
        ("intent", Some("apply")) => written(intent_apply(store, rest, call)),
        ("lock", Some("acquire")) => written(lock_acquire(store, rest, call)),
        ("lock", Some("renew")) => written(lock_renew(store, rest, call)),
        ("lock", Some("release")) => written(lock_release(store, rest, call)),
        _ => (
            Answer::CannotTell(format!(
                "cannot tell: {verb:?} is not a verb: section get|list|snapshot|apply-spec|publish-status, intent put|ack|judge|apply, lock acquire|renew|release"
            )),
            None,
        ),
    };
    if let Some(kind) = kind {
        record(call.metrics, verb.trim(), &kind, &answer);
    }
    render(answer)
}

/// A mutating verb's answer and the kind its metrics are labelled by.
fn written(a: (Answer, String)) -> (Answer, Option<String>) {
    (a.0, Some(a.1))
}

fn record(sink: &MetricsSink, verb: &str, kind: &str, a: &Answer) {
    let (name, reason) = match a {
        Answer::Ok(_) => (WRITES, "ok"),
        Answer::Refused(r) => (REFUSALS, r.code),
        Answer::Missing(_) => (REFUSALS, "not-found"),
        Answer::CannotTell(_) => (REFUSALS, "cannot-tell"),
    };
    sink.push(
        &[Point::Count { name, value: 1 }],
        &[("verb", verb), ("kind", kind), ("reason", reason)],
    );
}

fn body(args: &[String]) -> Result<Value, Answer> {
    let text = flag(args, "--json").ok_or_else(|| cannot("the body is required on stdin"))?;
    serde_json::from_str(&text).map_err(|e| refuse("body", "schema", format!("is not JSON: {e}")))
}

fn required(args: &[String], name: &str) -> Result<String, Answer> {
    flag(args, name).ok_or_else(|| cannot(format!("{name} is required")))
}

fn positional(args: &[String], what: &str) -> Result<String, Answer> {
    args.first()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| cannot(format!("missing {what}")))
}

fn rfc3339(now_ms: u64) -> String {
    format_rfc3339(now_ms / 1000)
}

fn token_text(t: Rev) -> String {
    format!("{}.{}", t.main, t.sub)
}

fn parse_token(s: &str) -> Result<Rev, Answer> {
    let bad = || refuse("--token", "token", format!("{s:?} is not a fencing token (<main>.<sub>)"));
    let (m, sub) = s.split_once('.').ok_or_else(bad)?;
    Ok(Rev::new(m.parse().map_err(|_| bad())?, sub.parse().map_err(|_| bad())?))
}

/// The section at `item`, its `resourceVersion` filled in from the store.
fn decode_section(item: &Item) -> Result<Section, Answer> {
    let mut s = parse_section(&item.value.data)
        .map_err(|r| cannot(format!("{} does not decode: {r}", item.key)))?;
    s.metadata.resource_version = Some(item.mod_rev.main.to_string());
    Ok(s)
}

fn stored(s: &Section) -> Object {
    let mut s = s.clone();
    s.metadata.resource_version = None;
    Object::new(serde_json::to_value(&s).unwrap_or(Value::Null)).with_ttl(SECTION_TTL_SECS)
}

fn shown(s: &Section, now_ms: u64) -> Value {
    json!({
        "section": s,
        "freshness": freshness(s, now_ms),
        "caught_up": caught_up(s),
    })
}

fn read_section<B: Backend>(store: &Store<B>, key: &str) -> Result<Option<(Section, Item)>, Answer> {
    let Some(item) = store.contract().get(key).map_err(cannot)? else {
        return Ok(None);
    };
    Ok(Some((decode_section(&item)?, item)))
}

/// `section get <Kind>/<name>`: the section, its freshness and whether it has caught up.
fn section_get<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> Answer {
    let r = match positional(args, "<Kind>/<name>") {
        Ok(r) => r,
        Err(a) => return a,
    };
    let Some((kind, name)) = parse_ref(&r) else {
        return cannot(format!("{r:?} is not <Kind>/<name>"));
    };
    if registered(kind).is_err() {
        return Answer::Missing(json!({ "not_found": r }));
    }
    match read_section(store, &section_key(kind, name)) {
        Ok(Some((s, _))) => Answer::Ok(shown(&s, call.now_ms)),
        Ok(None) => Answer::Missing(json!({ "not_found": r })),
        Err(a) => a,
    }
}

/// Every listed section of a registered kind, and the keys of any that no longer decode: a
/// kind not registered reads as absent, and one bad record does not blind the rest.
fn sections_of(items: &[Item], now_ms: u64) -> (Vec<Value>, Vec<String>) {
    let mut out = Vec::new();
    let mut undecodable = Vec::new();
    for i in items.iter().filter(|i| i.key.starts_with(SECTIONS_PREFIX)) {
        match decode_section(i) {
            Ok(s) if registered(&s.kind).is_ok() => out.push(shown(&s, now_ms)),
            Ok(_) => {}
            Err(_) => undecodable.push(i.key.clone()),
        }
    }
    (out, undecodable)
}

/// `section list [--kind K]`.
fn section_list<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> Answer {
    let prefix = match flag(args, "--kind") {
        Some(k) => format!("{SECTIONS_PREFIX}{k}/"),
        None => SECTIONS_PREFIX.to_string(),
    };
    let (items, list_rev) = match store.contract().list(&[&prefix], &Selector::Everything) {
        Ok(x) => x,
        Err(e) => return cannot(e),
    };
    let (sections, undecodable) = sections_of(&items, call.now_ms);
    Answer::Ok(json!({ "sections": sections, "undecodable": undecodable, "list_rev": list_rev }))
}

/// `section snapshot`: every section, every intent and the pass record, from one list over the
/// three prefixes, so the reconciler's pass reads one revision.
fn section_snapshot<B: Backend>(store: &Store<B>, call: &Call) -> Answer {
    let prefixes = [SECTIONS_PREFIX, INTENTS_PREFIX, PASS_KEY];
    let (items, list_rev) = match store.contract().list(&prefixes, &Selector::Everything) {
        Ok(x) => x,
        Err(e) => return cannot(e),
    };
    let (sections, undecodable) = sections_of(&items, call.now_ms);
    let intents: Vec<Value> = items
        .iter()
        .filter(|i| i.key.starts_with(INTENTS_PREFIX))
        .map(|i| json!({ "key": i.key, "intent": i.value.data, "mod_rev": i.mod_rev.main }))
        .collect();
    let pass = items
        .iter()
        .find(|i| i.key == PASS_KEY)
        .map_or(Value::Null, |i| i.value.data.clone());
    Answer::Ok(json!({ "sections": sections, "undecodable": undecodable, "intents": intents, "pass": pass, "list_rev": list_rev }))
}

/// A spec document: the shape of a composite resource of a section kind.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecDoc {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    metadata: SpecMeta,
    #[serde(default)]
    spec: std::collections::BTreeMap<String, Target>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecMeta {
    name: String,
}

fn parse_spec_doc(v: &Value) -> Result<SpecDoc, Refusal> {
    serde_path_to_error::deserialize(v).map_err(|e| {
        let path = e.path().to_string();
        let field = if path.is_empty() || path == "." { "spec document".into() } else { path };
        Refusal::new(field, "schema", e.inner().to_string())
    })
}

/// How far past this store's clock an owner's `observedAt` may be.
const CLOCK_SKEW_SECS: u64 = 60;

/// How often a write that lost its compare to a concurrent one is re-planned.
const ATTEMPTS: usize = 5;

/// `section apply-spec --actor compose` (body on stdin): the operator's targets. A changed
/// spec bumps `generation`; an unchanged one writes nothing.
fn apply_spec<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (Answer, String) {
    let doc = match body(args).and_then(|v| parse_spec_doc(&v).map_err(Answer::from)) {
        Ok(d) => d,
        Err(a) => return (a, "unknown".into()),
    };
    let kind = doc.kind.clone();
    (apply_spec_doc(store, args, call, doc), kind)
}

fn apply_spec_doc<B: Backend>(store: &Store<B>, args: &[String], call: &Call, doc: SpecDoc) -> Answer {
    let actor = match required(args, "--actor") {
        Ok(a) => a,
        Err(a) => return a,
    };
    if actor != COMPOSE {
        return refuse("actor", "ownership", format!("spec is written only by the operator's apply ({COMPOSE}), not {actor:?}"));
    }
    if doc.api_version != section::API_VERSION {
        return refuse("apiVersion", "api-version", format!("{:?} is not {:?}", doc.api_version, section::API_VERSION));
    }
    let kind = match registered(&doc.kind) {
        Ok(k) => k,
        Err(r) => return r.into(),
    };
    let key = section_key(kind.name, &doc.metadata.name);
    for attempt in 0..ATTEMPTS {
        let existing = match read_section(store, &key) {
            Ok(e) => e,
            Err(a) => return a,
        };
        let (mut s, prev) = match existing {
            Some((s, item)) => (s, Some(item.mod_rev)),
            None => (
                Section {
                    api_version: section::API_VERSION.into(),
                    kind: kind.name.into(),
                    metadata: SectionMeta {
                        name: doc.metadata.name.clone(),
                        owner: kind.owner.into(),
                        generation: Some(0),
                        ..SectionMeta::default()
                    },
                    spec: Default::default(),
                    status: Default::default(),
                    flow: Default::default(),
                    distance: None,
                    actions: Vec::new(),
                },
                None,
            ),
        };
        let generation = s.metadata.generation.unwrap_or(0);
        if prev.is_some() && s.spec == doc.spec {
            return Answer::Ok(json!({ "applied": false, "generation": generation, "resourceVersion": s.metadata.resource_version }));
        }
        s.spec = doc.spec.clone();
        s.metadata.generation = Some(generation + 1);
        s.metadata.resource_version = None;
        if let Err(r) = admit(&s) {
            return r.into();
        }
        let t = Txn::new().when(Compare::ModRev(key.clone(), prev)).put(&key, stored(&s));
        let req = Request::new(&format!("{}/{attempt}", call.request), COMPOSE, "lc.section.apply-spec");
        match store.contract().txn(&t, &req) {
            Ok(r) if r.succeeded => {
                return Answer::Ok(json!({ "applied": true, "generation": generation + 1, "resourceVersion": r.rev.to_string() }))
            }
            Ok(_) => continue,
            Err(e) => return cannot(e),
        }
    }
    cannot(format!("{key} kept moving under {ATTEMPTS} attempts; retry"))
}

/// `section publish-status --actor A --expect-rev R` (body on stdin): the owner's status, flow,
/// distance and actions, compared on the revision it read (0: the section does not exist).
fn publish_status<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (Answer, String) {
    let s = match body(args).and_then(|v| parse_section(&v).map_err(Answer::from)) {
        Ok(s) => s,
        Err(a) => return (a, "unknown".into()),
    };
    let kind = s.kind.clone();
    (publish(store, args, call, s), kind)
}

fn publish<B: Backend>(store: &Store<B>, args: &[String], call: &Call, mut s: Section) -> Answer {
    let actor = match required(args, "--actor") {
        Ok(a) => a,
        Err(a) => return a,
    };
    let expect: u64 = match required(args, "--expect-rev").map(|r| r.parse()) {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => return cannot("--expect-rev must be a non-negative integer"),
        Err(a) => return a,
    };
    let kind = match registered(&s.kind) {
        Ok(k) => k,
        Err(r) => return r.into(),
    };
    if actor != kind.owner {
        return refuse("actor", "ownership", format!("{} status is written only by {:?}, not {actor:?}", kind.name, kind.owner));
    }
    // The schema judges the body before the store's revision does, so a malformed write is
    // refused for what is wrong with it, whatever revision it names.
    if let Err(r) = admit_publish(&s) {
        return r.into();
    }
    let key = section_key(kind.name, &s.metadata.name);
    let existing = match read_section(store, &key) {
        Ok(e) => e,
        Err(a) => return a,
    };
    let at = existing.as_ref().map_or(0, |(_, i)| i.mod_rev.main);
    if at != expect {
        return refuse("--expect-rev", "revision", format!("{key} is at {at}, not {expect}: re-read and republish"));
    }
    let (spec, generation) = existing
        .as_ref()
        .map_or((Default::default(), 0), |(e, _)| (e.spec.clone(), e.metadata.generation.unwrap_or(0)));
    if !s.spec.is_empty() && s.spec != spec {
        return refuse("spec", "ownership", format!("spec is written only by the operator's apply ({COMPOSE})"));
    }
    if s.metadata.generation.is_some_and(|g| g != generation) {
        return refuse("metadata.generation", "generation", format!("is the store's, {generation}"));
    }
    if s.status.observed_generation > generation {
        return refuse(
            "status.observedGeneration",
            "generation",
            format!("{} is past the section's generation {generation}", s.status.observed_generation),
        );
    }
    s.spec = spec;
    s.metadata.generation = Some(generation);
    if let Err(r) = admit_publish(&s) {
        return r.into();
    }
    // A future stamp would read fresh until the clock caught up with it.
    let observed = s.metadata.observed_at.as_deref().and_then(section::parse_rfc3339);
    if observed.is_some_and(|o| o > call.now_ms / 1000 + CLOCK_SKEW_SECS) {
        return refuse("metadata.observedAt", "time", format!("is more than {CLOCK_SKEW_SECS}s past this store's clock"));
    }
    let prev = existing.map(|(_, i)| i.mod_rev);
    let t = Txn::new().when(Compare::ModRev(key.clone(), prev)).put(&key, stored(&s));
    let req = Request::new(call.request, &actor, "lc.section.publish-status");
    match store.contract().txn(&t, &req) {
        Ok(r) if r.succeeded => Answer::Ok(json!({ "published": true, "generation": generation, "resourceVersion": r.rev.to_string() })),
        Ok(_) => refuse("--expect-rev", "revision", format!("{key} moved before the write: re-read and republish")),
        Err(e) => cannot(e),
    }
}

fn reconciler_fence(token: Rev) -> Fence {
    Fence {
        resource: RECONCILER_LOCK.into(),
        token,
    }
}

/// Whether the reconciler's lock is live and held under `token`; the txn fences on it again.
fn holds_lock<B: Backend>(store: &Store<B>, f: &Fence) -> Result<bool, Answer> {
    let lock = store.contract().get(&f.key()).map_err(cannot)?;
    Ok(lock.is_some_and(|l| l.lock_expires_at_ms.is_some() && l.create_rev == f.token))
}

fn lock_lost() -> Answer {
    refuse("--token", "token", format!("is not the live token of locks/{RECONCILER_LOCK}"))
}

fn kind_of(key: &str) -> String {
    parse_key(key).map_or("unknown", |(k, _, _)| k).to_string()
}

/// `intent put <key> --token T --actor reconciler` (body on stdin).
fn intent_put<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (Answer, String) {
    let key = match positional(args, "<intent key>") {
        Ok(k) => k,
        Err(a) => return (a, "unknown".into()),
    };
    let kind = kind_of(&key);
    (put(store, args, call, &key), kind)
}

fn put<B: Backend>(store: &Store<B>, args: &[String], call: &Call, key: &str) -> Answer {
    let (actor, token) = match (required(args, "--actor"), required(args, "--token")) {
        (Ok(a), Ok(t)) => (a, t),
        (Err(a), _) | (_, Err(a)) => return a,
    };
    if actor != RECONCILER {
        return refuse("actor", "ownership", format!("only {RECONCILER} writes an intent, not {actor:?}"));
    }
    let token = match parse_token(&token) {
        Ok(t) => t,
        Err(a) => return a,
    };
    let i = match body(args).and_then(|v| parse_intent(&v).map_err(Answer::from)) {
        Ok(i) => i,
        Err(a) => return a,
    };
    if let Err(r) = admit_put(key, &i, &token_text(token)) {
        return r.into();
    }
    let fence = reconciler_fence(token);
    match holds_lock(store, &fence) {
        Ok(true) => {}
        Ok(false) => return lock_lost(),
        Err(a) => return a,
    }
    match store.contract().get(key) {
        Ok(Some(_)) => return refuse("key", "exists", format!("{key} is already written; an intent is written once")),
        Ok(None) => {}
        Err(e) => return cannot(e),
    }
    let value =Object::new(serde_json::to_value(&i).unwrap_or(Value::Null)).with_ttl(INTENT_TTL_SECS);
    let t = Txn::new()
        .fenced(&fence)
        .when(Compare::ModRev(key.into(), None))
        .put(key, value);
    let mut req = Request::new(call.request, &actor, "lc.intent.put");
    req.evidence = json!({ "key": key });
    match store.contract().txn(&t, &req) {
        Ok(r) if r.succeeded => Answer::Ok(json!({ "key": key, "rev": r.rev })),
        Ok(_) => match holds_lock(store, &fence) {
            Ok(true) => refuse("key", "exists", format!("{key} is already written; an intent is written once")),
            Ok(false) => lock_lost(),
            Err(a) => a,
        },
        Err(e) => cannot(e),
    }
}

fn read_intent<B: Backend>(store: &Store<B>, key: &str) -> Result<(Intent, Item), Answer> {
    let item = store
        .contract()
        .get(key)
        .map_err(cannot)?
        .ok_or_else(|| Answer::Missing(json!({ "not_found": key })))?;
    let i = parse_intent(&item.value.data).map_err(|r| cannot(format!("{key} does not decode: {r}")))?;
    Ok((i, item))
}

/// The intent at `key`, whose target section's owner must be `actor`.
fn owned_intent<B: Backend>(store: &Store<B>, args: &[String], key: &str) -> Result<(Intent, Item, String), Answer> {
    let actor = required(args, "--actor")?;
    let Some((kind, _, _)) = parse_key(key) else {
        return Err(refuse("key", "key", format!("{key:?} is not intents/<Kind>/<name>/<id>")));
    };
    let kind = registered(kind)?;
    if actor != kind.owner {
        return Err(refuse("actor", "ownership", format!("only {:?}, the target section's owner, acks a {} intent", kind.owner, kind.name)));
    }
    let (i, item) = read_intent(store, key)?;
    if i.ack.is_some() {
        return Err(refuse("ack", "once", format!("{key} is already acknowledged")));
    }
    Ok((i, item, actor))
}

fn intent_object(prev: &Item, i: &Intent) -> Object {
    Object {
        metadata: prev.value.metadata.clone(),
        data: serde_json::to_value(i).unwrap_or(Value::Null),
    }
}

fn ack_result(args: &[String]) -> Result<AckResult, Answer> {
    match required(args, "--result")?.as_str() {
        "applied" => Ok(AckResult::Applied),
        "already-applied" => Ok(AckResult::AlreadyApplied),
        "refused" => Ok(AckResult::Refused {
            reason: required(args, "--reason")?,
            stale: None,
        }),
        other => Err(refuse("--result", "result", format!("{other:?} is not applied|already-applied|refused"))),
    }
}

/// `intent ack <key> --actor <owner> --result applied|already-applied|refused [--reason R]`:
/// once, and nothing but `ack`.
fn intent_ack<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (Answer, String) {
    let key = match positional(args, "<intent key>") {
        Ok(k) => k,
        Err(a) => return (a, "unknown".into()),
    };
    let kind = kind_of(&key);
    let answer = (|| {
        let (mut i, item, actor) = owned_intent(store, args, &key)?;
        let result = ack_result(args)?;
        let head = store.contract().head().map_err(cannot)?;
        i.ack = Some(Ack {
            at: rfc3339(call.now_ms),
            result,
            observed_rev: head,
        });
        let t = Txn::new()
            .when(Compare::ModRev(key.clone(), Some(item.mod_rev)))
            .put(&key, intent_object(&item, &i));
        let req = Request::new(call.request, &actor, "lc.intent.ack");
        match store.contract().txn(&t, &req).map_err(cannot)? {
            r if r.succeeded => Ok(Answer::Ok(json!({ "key": key, "ack": i.ack, "rev": r.rev }))),
            _ => Err(refuse("ack", "once", format!("{key} moved before the ack: re-read it"))),
        }
    })();
    (answer.unwrap_or_else(|a| a), kind)
}

/// `intent judge <key> --token T --actor reconciler --result fulfilled|failed --observed X`:
/// once, and only after the owner's ack.
fn intent_judge<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (Answer, String) {
    let key = match positional(args, "<intent key>") {
        Ok(k) => k,
        Err(a) => return (a, "unknown".into()),
    };
    let kind = kind_of(&key);
    let answer = (|| {
        let actor = required(args, "--actor")?;
        if actor != RECONCILER {
            return Err(refuse("actor", "ownership", format!("only {RECONCILER} judges an intent, not {actor:?}")));
        }
        let fence = reconciler_fence(parse_token(&required(args, "--token")?)?);
        let result = match required(args, "--result")?.as_str() {
            "fulfilled" => JudgementResult::Fulfilled,
            "failed" => JudgementResult::Failed,
            other => return Err(refuse("--result", "result", format!("{other:?} is not fulfilled|failed"))),
        };
        let observed: f64 = required(args, "--observed")?
            .parse()
            .map_err(|_| refuse("--observed", "schema", "is not a number"))?;
        let (mut i, item) = read_intent(store, &key)?;
        if i.ack.is_none() {
            return Err(refuse("ack", "unacknowledged", "only an acknowledged intent is judged"));
        }
        if i.judgement.is_some() {
            return Err(refuse("judgement", "once", format!("{key} is already judged")));
        }
        if !holds_lock(store, &fence)? {
            return Err(lock_lost());
        }
        i.judgement = Some(Judgement {
            at: rfc3339(call.now_ms),
            result,
            observed,
        });
        let t = Txn::new()
            .fenced(&fence)
            .when(Compare::ModRev(key.clone(), Some(item.mod_rev)))
            .put(&key, intent_object(&item, &i));
        let req = Request::new(call.request, &actor, "lc.intent.judge");
        match store.contract().txn(&t, &req).map_err(cannot)? {
            r if r.succeeded => Ok(Answer::Ok(json!({ "key": key, "judgement": i.judgement, "rev": r.rev }))),
            _ if !holds_lock(store, &fence)? => Err(lock_lost()),
            _ => Err(refuse("judgement", "once", format!("{key} moved before the judgement: re-read it"))),
        }
    })();
    (answer.unwrap_or_else(|a| a), kind)
}

/// A knob this verb may move: a key under `knobs/`, which no other verb writes.
fn knob_key(args: &[String]) -> Result<String, Answer> {
    let k = required(args, "--knob")?;
    match k.strip_prefix(keys::KNOBS) {
        Some(rest) if !rest.is_empty() && !rest.starts_with('/') => Ok(k),
        _ => Err(refuse("--knob", "knob", format!("{k:?} is not a key under {}", keys::KNOBS))),
    }
}

/// `intent apply <key> --knob <store-key> --actor <owner>`: the from→to compare-and-swap and
/// its ack in one txn. A knob already at `to` is the re-run after a crash between the two,
/// and acks already-applied without moving it again.
fn intent_apply<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (Answer, String) {
    let key = match positional(args, "<intent key>") {
        Ok(k) => k,
        Err(a) => return (a, "unknown".into()),
    };
    let kind = kind_of(&key);
    let answer = (|| {
        let knob = knob_key(args)?;
        let (mut i, item, actor) = owned_intent(store, args, &key)?;
        let own = parse_key(&key).map(|(k, n, _)| format!("{}{k}/{n}/", keys::KNOBS));
        if knob != i.target || !own.is_some_and(|p| knob.starts_with(&p)) {
            return Err(refuse(
                "--knob",
                "knob",
                format!("{knob:?} is not this intent's target {:?} under its own section's knobs", i.target),
            ));
        }
        let cur = store.contract().get(&knob).map_err(cannot)?;
        let value = cur.as_ref().map(|c| c.value.data.clone());
        let result = match &value {
            Some(v) if *v == i.from => AckResult::Applied,
            Some(v) if *v == i.to => AckResult::AlreadyApplied,
            other => AckResult::Refused {
                reason: "stale".into(),
                stale: Some(other.clone().unwrap_or(Value::Null)),
            },
        };
        let head = store.contract().head().map_err(cannot)?;
        let moved = result == AckResult::Applied;
        i.ack = Some(Ack {
            at: rfc3339(call.now_ms),
            result,
            observed_rev: head,
        });
        let mut t = Txn::new()
            .when(Compare::ModRev(key.clone(), Some(item.mod_rev)))
            .when(Compare::ModRev(knob.clone(), cur.as_ref().map(|c| c.mod_rev)))
            .put(&key, intent_object(&item, &i));
        if let (true, Some(c)) = (moved, &cur) {
            t = t.put(
                &knob,
                Object {
                    metadata: c.value.metadata.clone(),
                    data: i.to.clone(),
                },
            );
        }
        let req = Request::new(call.request, &actor, "lc.intent.apply");
        match store.contract().txn(&t, &req).map_err(cannot)? {
            r if r.succeeded => Ok(Answer::Ok(json!({ "key": key, "ack": i.ack, "knob": knob, "rev": r.rev }))),
            _ => Err(refuse("ack", "once", format!("{key} or {knob} moved before the apply: re-read and re-run"))),
        }
    })();
    (answer.unwrap_or_else(|a| a), kind)
}

/// A lock resource these verbs may take: not a claim, which only the bead machine takes.
fn resource(args: &[String]) -> Result<String, Answer> {
    let r = positional(args, "<resource>")?;
    if r.chars().any(char::is_whitespace) || r.starts_with(&keys::claim_resource("")) {
        return Err(refuse("resource", "resource", format!("{r:?} is not a resource these verbs lock")));
    }
    Ok(r)
}

fn ttl_ms(args: &[String]) -> Result<Option<u64>, Answer> {
    let Some(s) = flag(args, "--ttl") else {
        return Ok(None);
    };
    match s.parse::<u64>() {
        Ok(n) if n > 0 => Ok(Some(n.saturating_mul(1000))),
        _ => Err(refuse("--ttl", "ttl", format!("{s:?} is not a positive number of seconds"))),
    }
}

fn lock_error(e: Error) -> Answer {
    match e {
        Error::Locked { resource, token } => refuse("resource", "locked", format!("{resource} is held under token {}", token_text(token))),
        Error::LockLost { resource } => refuse("--token", "lock-lost", format!("{resource}'s lock has expired or is held under another token")),
        Error::LockTtlBelowMinimum { min_ms, got_ms } => refuse("--ttl", "ttl", format!("{got_ms} ms is below the {min_ms} ms minimum")),
        other => cannot(other),
    }
}

/// `lock acquire <resource> --ttl S --holder H`: the fencing token is the lock's `create_rev`.
fn lock_acquire<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (Answer, String) {
    let answer = (|| {
        let r = resource(args)?;
        let holder = required(args, "--holder")?;
        let ttl = ttl_ms(args)?.ok_or_else(|| cannot("--ttl is required"))?;
        let req = Request::new(call.request, &holder, "lc.lock.acquire");
        let fence = store.contract().lock(&r, ttl, &req).map_err(lock_error)?;
        Ok(Answer::Ok(json!({ "resource": r, "token": token_text(fence.token), "holder": holder, "ttl_secs": ttl / 1000 })))
    })();
    (answer.unwrap_or_else(|a| a), "lock".into())
}

fn holder_fence(args: &[String]) -> Result<(Fence, String), Answer> {
    let r = resource(args)?;
    let token = parse_token(&required(args, "--token")?)?;
    let holder = flag(args, "--holder").unwrap_or_else(|| "lock-holder".into());
    Ok((Fence { resource: r, token }, holder))
}

/// `lock renew <resource> --token T [--ttl S]`.
fn lock_renew<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (Answer, String) {
    let answer = (|| {
        let (fence, holder) = holder_fence(args)?;
        let req = Request::new(call.request, &holder, "lc.lock.renew");
        match ttl_ms(args)? {
            Some(ttl) => store.contract().renew_for(&fence, ttl, &req),
            None => store.contract().renew(&fence, &req),
        }
        .map_err(lock_error)?;
        Ok(Answer::Ok(json!({ "resource": fence.resource, "token": token_text(fence.token), "renewed": true })))
    })();
    (answer.unwrap_or_else(|a| a), "lock".into())
}

/// `lock release <resource> --token T`.
fn lock_release<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (Answer, String) {
    let answer = (|| {
        let (fence, holder) = holder_fence(args)?;
        let req = Request::new(call.request, &holder, "lc.lock.release");
        store.contract().release(&fence, &req).map_err(lock_error)?;
        Ok(Answer::Ok(json!({ "resource": fence.resource, "released": true })))
    })();
    (answer.unwrap_or_else(|a| a), "lock".into())
}

#[cfg(test)]
mod tests;
