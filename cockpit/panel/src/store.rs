//! One fetch, many views.
//!
//! The panel used to call `load()` for every view on every tab switch: two identical
//! `bd list --all` queries (decisions and insights read the same rows) plus a mail query.
//! Measured — `bd` 1134 ms, `gt mail` 816 ms — that is over three seconds of subprocess
//! before spawn overhead, on a keypress. the operator: *"almost 5 seconds to switch tabs."*
//!
//! So the process boundary is crossed on a TIMER, never on a keystroke. A background thread
//! owns refreshing; the UI thread only ever reads the last snapshot and filters it in
//! memory. Switching tabs costs nothing, and a slow or hung `bd` can never freeze the pane —
//! it just means the snapshot is a few seconds old, which the header says out loud.
//!
//! There used to be a second fetch here — `gt mail inbox`, 816 ms, run concurrently with the
//! beads query so a refresh cost max() rather than the sum. It is gone with the town it read:
//! nothing has written to that mailbox since the 2026-09-05 cutover, so it was 1,440 subprocess
//! calls a day into a decommissioned harness to render a tab that could only be empty or
//! historical. NOTIFICATIONS now filters rows this store already had.

use crate::model::{Item, View};
use serde_json::Value;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Directories prepended to every child's PATH, from `SPIRA_PATH` in `spira.conf`. `bd` and
/// `gt` live in a different place on every box, and this panel is started by tmux, whose
/// global PATH has neither.
fn extra_path() -> Vec<String> {
    std::env::var("SPIRA_PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .collect()
}

/// THE beads database this panel raises, reads, answers and closes in.
///
/// (the operator, verbatim: "let's just use the spira one for everything from here on out. i
/// don't want this migration/deprecation to bake a bunch of complexity and modality into our
/// tools.")
///
/// There was a walk here — the town, one entry per rig, then Spira — with a precedence order,
/// a per-row `_db` tag and a dedupe by id. All four existed because two live databases held
/// the same beads, and all four are gone with that. They were not free while they lasted: the
/// shell tools were switched to Spira before the panel was, which split one conversation
/// across two databases within the hour. Their 18:54 comment landed in the town because the
/// pane wrote there, while `unanswered.sh` read Spira and reported nothing waiting. A
/// half-migrated tool is worse than either end of the migration, because each half is
/// individually correct and together they lose messages.
pub fn db() -> String {
    // No hardcoded fallback. conf.sh exports COCKPIT_DB, defaulting it to SPIRA_DB, and a
    // panel that guessed a database instead would read someone else's conversation.
    std::env::var("COCKPIT_DB")
        .or_else(|_| std::env::var("SPIRA_DB"))
        .unwrap_or_default()
}

/// The label that means "waiting on the operator" — the same one the escalation gate defers
/// on, every persona's predicate excludes, and the shell tools read. It is a configured key
/// rather than a literal in five files, because five literals is how the panel and the
/// predicates come to disagree about which beads are waiting on anyone, and the panel is the
/// half nobody notices is wrong: it simply shows fewer.
pub fn ask_label() -> String {
    std::env::var("SPIRA_ASK_LABEL").unwrap_or_else(|_| "needs-operator".to_string()) // literal-ok: Rust fallback for direct invocation without conf.sh
}

/// Raw rows plus when they were fetched, and what went wrong if anything did.
#[derive(Clone, Default)]
pub struct Snapshot {
    /// Bumped on every completed refresh. The UI derives its lists from the snapshot and
    /// caches the result against this number, so a keypress re-filters nothing.
    pub gen: u64,
    pub beads: Option<Vec<Value>>,
    /// issue id -> comment rows, for the beads that have any.
    pub threads: std::collections::HashMap<String, Vec<Value>>,
    pub beads_err: Option<String>,
    pub at: Option<Instant>,
    pub refreshing: bool,
}

impl Snapshot {
    /// Seconds since this snapshot was taken, or None if nothing has landed yet.
    pub fn age(&self) -> Option<u64> {
        self.at.map(|t| t.elapsed().as_secs())
    }
}

pub type Shared = Arc<Mutex<Snapshot>>;

/// What an optimistic hide is waiting to see.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// A decision was closed.
    Closed,
    /// A record was archived (`true`) or restored (`false`).
    Archived(bool),
}

/// Whether the snapshot has caught up with an optimistic hide.
///
/// `pending` — the ids hidden after a keypress — has to be pruned against something, or it
/// grows and keeps filtering: after eight keypresses the panel reported NOTIFICATIONS 0
/// against a real 17 unread. A count that reads zero when the answer is seventeen is the
/// exact reassuring lie this panel exists to prevent.
///
/// PRESENCE IS NOT THAT SOMETHING. Pruning on "the id is no longer returned" was right for a
/// close only by accident — a closed decision leaves DECISIONS' filter — and wrong for an
/// archive, which LABELS a row rather than removing it. So every dismissed id stayed pending
/// for the life of the session and went on hiding its row, and pressing `A` and then `h`
/// showed an empty history over the burst just archived: the same reassuring lie, arriving
/// through the mechanism built to prevent it. The hide now ends when the WRITE IT WAS
/// COVERING becomes visible, which is what it always meant.
pub fn settled(s: &Snapshot, id: &str, e: Expect) -> bool {
    let Some(rows) = &s.beads else { return false };
    let Some(r) = rows.iter().find(|r| r["id"].as_str() == Some(id)) else {
        // Gone from the store outright. Nothing is left to hide, whatever was expected.
        return true;
    };
    match e {
        Expect::Closed => r["status"].as_str() == Some("closed"),
        Expect::Archived(want) => labels(r).contains(&crate::model::ARCHIVED) == want,
    }
}

/// Resolve a tool to an absolute path, because PATH cannot be trusted here.
///
/// WHY. The pane came up reading `bd: No such file or directory`. `bd` and `gt`
/// live in ~/.local/bin, and tmux's GLOBAL environment carries a PATH without it — so the
/// panel worked when launched from an interactive shell and broke the moment layout.sh
/// respawned it. Same class as hunk needing node on a login-shell PATH, and as the
/// gt-dashboard unit having to set PATH explicitly because systemd does not inherit one.
///
/// A tool that only works when started by hand is not installed, it is coincidental.
pub fn bin(cmd: &str) -> String {
    if cmd.contains('/') {
        return cmd.to_string();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    // SPIRA_PATH comes FIRST, deliberately, and the operator decides what is in it: it may
    // hold a shim directory whose tools are not duplicates of the real binaries. Resolving
    // straight to ~/.local/bin looks tidier and would step around whatever the operator put
    // in front of them.
    let mut dirs = extra_path();
    dirs.extend([
        format!("{home}/.local/bin"),
        "/usr/local/bin".into(),
        "/usr/bin".into(),
    ]);
    for dir in dirs {
        let p = format!("{dir}/{cmd}");
        if std::path::Path::new(&p).is_file() {
            return p;
        }
    }
    cmd.to_string() // let PATH try, and let the error say so
}

/// The PATH our children get.
///
/// Resolving a tool absolutely is not enough on its own: a shim execs the real binary, and a
/// tool may shell out to another BY NAME. So a child inherits our PATH and fails one level
/// down, which is how NOTIFICATIONS once rendered `?` while DECISIONS worked. Hand them a
/// PATH that has the tools.
pub fn child_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let inherited = std::env::var("PATH").unwrap_or_default();
    let mut dirs = extra_path();
    dirs.extend([
        format!("{home}/.local/bin"),
        "/usr/local/bin".into(),
        "/usr/bin".into(),
        "/bin".into(),
    ]);
    if !inherited.is_empty() {
        dirs.push(inherited);
    }
    dirs.join(":")
}

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let mut c = Command::new(bin(cmd));
    c.args(args).env("PATH", child_path());
    // NO `current_dir`. It existed so `gt` could resolve its town, and `gt` is gone from this
    // panel; `bd` takes its database from `-C`, so a working directory here could only ever
    // decide something by accident.
    match c.output() {
        Err(e) => Err(format!("{cmd}: {e}")),
        Ok(o) if !o.status.success() && o.stdout.is_empty() => Err(format!(
            "{cmd}: {}",
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .next()
                .unwrap_or("failed")
                .to_string()
        )),
        Ok(o) => Ok(String::from_utf8_lossy(&o.stdout).to_string()),
    }
}

/// `bd --json` can print human warnings on stdout before the payload.
fn rows(text: &str, key: &str) -> Result<Vec<Value>, String> {
    let start = text.find(['[', '{']).ok_or("no JSON in output")?;
    let v: Value = serde_json::from_str(&text[start..]).map_err(|e| format!("bad JSON: {e}"))?;
    Ok(if v.is_array() {
        v.as_array().cloned().unwrap_or_default()
    } else {
        v[key].as_array().cloned().unwrap_or_default()
    })
}

/// `--all` is required: insights AND events are created CLOSED, and `bd list` hides closed
/// issues. Every view the panel has is a filter over this one result — decisions, FYI and
/// now events too, which were already in this payload and dropped on the floor because
/// neither surviving filter matched them.
///
/// One call against one database. An error here is a real error and reaches the pane as one:
/// there is no other database whose rows could stand in for these, so silently returning an
/// empty list would render "nothing is waiting" over an unread queue.
fn fetch_beads() -> Result<Vec<Value>, String> {
    let out = run("bd", &["-C", &db(), "list", "--all", "--limit", "0", "--json"])?;
    rows(&out, "issues")
}

/// Comment threads for every bead reporting `comment_count > 0`.
///
/// A thread is fetched, not counted, because the pane has to SHOW the conversation: a count
/// tells the operator something was said and then makes them go elsewhere to read it, which is the
/// thing that pushed replies into the chat transcript in the first place.
fn fetch_threads(beads: &[Value]) -> std::collections::HashMap<String, Vec<Value>> {
    let ask = ask_label();
    let wanted: Vec<String> = beads
        .iter()
        .filter(|r| r["comment_count"].as_u64().unwrap_or(0) > 0)
        // ONLY the beads a view can actually show. The first version filtered on
        // comment_count alone, and `fetch_beads` returns every bead there is — ~1700 rows,
        // of which hundreds carry comments. It spawned 307 concurrent `bd`
        // processes every refresh and drove the load average to 274 on four cores, which is
        // precisely the failure law-fence-loops-on-shared-hardware was written about, from
        // the same hand, the same day. A panel that fetches what it cannot display is not
        // caching, it is a denial of service with a nice UI.
        .filter(|r| {
            let l = labels(r);
            let waiting = l.contains(&ask.as_str()) || l.contains(&"overseer");
            if !waiting {
                return false;
            }
            // A DISMISSED INSIGHT KEEPS ITS THREAD, because `h` brings it back and a
            // retrieved item that silently lost its conversation is the reassuring lie this
            // panel exists to avoid. The cost is bounded by construction: only insights, only
            // ones that already report comments — 4 dismissed insights on this box, 1 of them
            // with a comment. Everything else archived stays excluded, which is what keeps
            // this from becoming the 307-concurrent-`bd` fetch it was before.
            if l.contains(&"insight") {
                return true;
            }
            // A CLEARED ALERT KEEPS ITS THREAD for the same reason: `h` brings it back, and a
            // retrieved record that silently lost the note saying what was done about it is
            // the reassuring lie one layer along. Bounded the same way — only alerts, only
            // ones already reporting a comment.
            if l.contains(&crate::model::alert::LABEL) {
                return true;
            }
            if l.contains(&"archived") {
                return false;
            }
            r["status"].as_str() != Some("closed")
        })
        .filter_map(|r| Some(r["id"].as_str()?.to_string()))
        .collect();

    let handles: Vec<_> = wanted
        .into_iter()
        .map(|id| {
            thread::spawn(move || {
                let out = run("bd", &["-C", &db(), "comments", &id, "--json"]).ok()?;
                Some((id, rows(&out, "comments").ok()?))
            })
        })
        .collect();

    handles
        .into_iter()
        .filter_map(|h| h.join().ok().flatten())
        .collect()
}

/// A FROZEN SNAPSHOT, for verifying the pane without a human and without the beads.
///
/// `PANEL_FIXTURE=<file.json>` makes the store read that file instead of shelling out, and
/// `--dump` writes the live snapshot in that format. Both halves are needed together.
///
/// WHY. The bead asked for the pane to be verified by
/// capturing `--once` before and after and diffing. The first attempt did exactly that and
/// the diff was worthless: between the two captures a bead got answered, DECISIONS went
/// from 3 to 1, and every row moved for a reason that had nothing to do with the render.
/// A render regression test against a live database is not a test, it is a coin toss —
/// and the pane is now the surface every escalation is read in, so it needs one.
pub fn fixture() -> Option<String> {
    std::env::var("PANEL_FIXTURE").ok().filter(|s| !s.is_empty())
}

/// The snapshot in the shape `PANEL_FIXTURE` reads back.
pub fn dump(s: &Snapshot) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "beads": s.beads.clone().unwrap_or_default(),
        "threads": s.threads.clone(),
    }))
    .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// Load a fixture into the snapshot. Synchronous and total: a fixture that will not parse is
/// an error on the pane rather than a silent fall back to the live database, which would
/// make a test quietly measure the wrong thing.
fn load_fixture(shared: &Shared, path: &str) {
    let mut s = shared.lock().unwrap();
    s.refreshing = false;
    match std::fs::read_to_string(path)
        .map_err(|e| format!("{path}: {e}"))
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| format!("{path}: {e}")))
    {
        Err(e) => s.beads_err = Some(e),
        Ok(v) => {
            s.beads = Some(v["beads"].as_array().cloned().unwrap_or_default());
            s.threads = v["threads"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_array().cloned().unwrap_or_default()))
                        .collect()
                })
                .unwrap_or_default();
            s.beads_err = None;
        }
    }
    // A FIXED age, not `Instant::now()`. The header prints how stale the snapshot is, so a
    // live clock would put "live" / "<1m" / "3m" into a frame that is supposed to be
    // byte-identical across runs — the one thing a render diff cannot tolerate.
    s.at = Some(Instant::now() - Duration::from_secs(90));
    s.gen = s.gen.wrapping_add(1);
}

/// Refresh in the background. Never blocks the caller, and never runs two at once — a
/// second refresh while one is in flight would double the subprocess load for nothing.
pub fn refresh(shared: &Shared) {
    if let Some(path) = fixture() {
        load_fixture(shared, &path);
        return;
    }
    {
        let mut s = shared.lock().unwrap();
        if s.refreshing {
            return;
        }
        s.refreshing = true;
    }
    let shared = Arc::clone(shared);
    thread::spawn(move || {
        // ONE call. There was a `thread::spawn` here to overlap this with `gt mail inbox`;
        // with the mail fetch gone the concurrency was overlapping a query with nothing.
        let b = fetch_beads();
        // Only for beads that actually have comments — `comment_count` is already in the
        // list payload, so this is usually two or three extra calls, not one per bead.
        let threads = match &b {
            Ok(v) => fetch_threads(v),
            Err(_) => Default::default(),
        };
        let mut s = shared.lock().unwrap();
        match b {
            Ok(v) => {
                s.beads = Some(v);
                s.beads_err = None;
                s.threads = threads;
            }
            Err(e) => s.beads_err = Some(e),
        }
        s.at = Some(Instant::now());
        s.gen = s.gen.wrapping_add(1);
        s.refreshing = false;
    });
}

pub fn spawn_refresher(shared: Shared, every: Duration) {
    thread::spawn(move || loop {
        refresh(&shared);
        thread::sleep(every);
    });
}

fn labels(r: &Value) -> Vec<&str> {
    r["labels"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default()
}

/// The statute a promoted insight was enacted as, read off its `enacted:` label.
///
/// One label, one law: an insight promoted twice is a mistake worth seeing rather than a
/// list to render, so the FIRST is taken and the row shows it.
fn enacted(labels: &[&str]) -> Option<String> {
    labels
        .iter()
        .find_map(|l| l.strip_prefix(crate::model::ENACTED))
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// The recommended default, lifted from the description. An ask without one makes the operator
/// decide from scratch, so when it exists it belongs on screen.
fn lead(desc: &str) -> String {
    desc.lines()
        .find(|l| l.contains("Default"))
        .map(|l| {
            l.split_once(':')
                .map(|(_, v)| v)
                .unwrap_or(l)
                .replace("**", "")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// The ask body with the Default recommendation line removed.
///
/// `detail_lines` renders `lead` above the body as "default: …". Leaving the same line in
/// the body renders it twice — once labelled, once as raw markdown — claiming rows that
/// produce nothing the reader has not already seen. Blank lines left adjacent by the removal
/// are stripped so a description that IS ONLY the Default line leaves an empty body rather
/// than a body of blank rows.
fn ask_body(desc: &str) -> String {
    let joined: Vec<&str> = desc.lines().filter(|l| !l.contains("Default")).collect();
    let s = joined.join("\n");
    s.trim_matches('\n').to_string()
}

/// An insight's body, with the escalation template's call to action taken off it.
///
/// (the operator, verbatim: *"these insights seem more like bug reports which means they're
/// implicitly asking me for feedback -- really they should be FYI only."*) They are describing
/// the text, not an impression of it. `ask.sh compose` wrote ONE body for all three kinds, so
/// every insight on this box opens with `**What is blocked:**` -- nothing is blocked, it is a
/// record -- and ends by instructing them to file a verdict:
///
/// ```text
/// _Filed by the brain session. Answer inline in the cockpit pane, or:_
/// `.claude/cockpit/ask.sh answered <id> "<verdict>"`
/// ```
///
/// That trailer is a bug report's clothing sewn on by the generator, and it is why the view
/// reads as one. `ask.sh` no longer writes it onto an insight; this strips it from the ones
/// already in the database, which cannot be rewritten -- Spira's aeons may not write to the
/// Gas Town beads databases, and editing a stored record to fix its presentation would be the
/// wrong repair anyway.
///
/// Only two things go: the trailer, and the false section LABEL. The prose itself is
/// untouched -- the reader names the section in chrome (`-- why it matters --`) rather than
/// substituting a different claim about what the author wrote.
fn fyi_body(desc: &str) -> String {
    let mut lines: Vec<&str> = desc.lines().collect();
    // The trailer, and everything after it: it is always last, and it is the only part of the
    // body that tells them to do something.
    // BOTH trailers. `ask.sh` now signs an insight "_Recorded by the brain session. Nothing is
    // owed_" — true, and worth having in the stored record, on the phone and in `bd show`. In
    // the pane it is two rows of boilerplate repeated on every item, saying what the footer
    // already says with a live key hint; the pane earns its rows.
    if let Some(i) = lines.iter().position(|l| {
        let t = l.trim_start();
        t.starts_with("_Filed by the brain session") || t.starts_with("_Recorded by the brain session")
    }) {
        lines.truncate(i);
    }
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    // The label on the leading paragraph. "What is blocked" is the escalation template's and
    // is false here; "Why it matters" is the insight's own and is what the chrome now says.
    if let Some(first) = out.iter_mut().find(|l| !l.trim().is_empty()) {
        for label in ["**What is blocked:**", "**Why it matters:**"] {
            if let Some(rest) = first.trim_start().strip_prefix(label) {
                *first = rest.trim_start().to_string();
                break;
            }
        }
    }
    while out.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.remove(0);
    }
    out.join("\n")
}

/// A comment thread in reading order: oldest first, enforced rather than assumed.
///
/// The order is load-bearing twice over. The pane now renders the thread ABOVE the ask, so
/// it is the first thing read; and the list's turn marker asks `thread.last()` whose ball it
/// is — `↩` for me, `●` for the operator — which inverts outright if the newest comment is at the
/// front. `bd comments` returns oldest-first today (checked against a real thread,
/// three comments, ascending), but a display whose meaning flips with someone else's
/// `ORDER BY` should not rest on that.
///
/// Only sorted when every comment carries a full stamp: an empty one sorts before all the
/// others and would silently hoist an unstamped comment to the top of the conversation.
/// ISO-8601 UTC compares lexicographically, and the sort is stable, so equal stamps keep
/// the order the database returned them in.
fn oldest_first(mut t: Vec<(String, String, String)>) -> Vec<(String, String, String)> {
    if t.iter().all(|(_, when, _)| when.len() >= 16) {
        t.sort_by(|a, b| a.1.cmp(&b.1));
    }
    t
}

/// The ALERTS view: conditions that are true NOW, oldest first.
///
/// OPEN MEANS FIRING; CLOSED MEANS THE CONDITION CLEARED. Nothing in this function closes
/// anything — the writer retracts its own statement, which is what makes the tab
/// self-clearing, and the whole reason an alert must not be a decision.
///
/// OLDEST FIRST, EXPLICITLY, and not the `out.reverse()` the other views use. First-seen
/// order is the reading order for conditions: the one that has been true longest is the one
/// that is not fixing itself. `bd list` orders by recency and nothing promises it will keep
/// doing so, so the sort is stated here rather than inherited.
///
/// THE EMPTY CASE IS A POSITIVE CONTROL (law-absence-needs-a-positive-control). "No alerts"
/// and "the reader is pointed at the wrong database" are the same pixels otherwise, and the
/// wrong one reads as all-clear — which displaces exactly the suspicion that would have
/// prompted a look. So an empty view is only reported after the read is proved: the snapshot
/// must hold beads. Zero rows from a `bd list --all --limit 0` is not an empty queue, it is a
/// reader that resolved nothing, and it is refused rather than rendered green.
fn alerts(s: &Snapshot, dismissed: bool, now: i64) -> Result<Vec<Item>, String> {
    let all = match (&s.beads, &s.beads_err) {
        (_, Some(e)) => return Err(e.clone()),
        (None, _) => return Err("loading…".into()),
        (Some(all), _) => all,
    };
    // The positive control. Not `all.is_empty()` as a convenience check — it IS the check:
    // the only evidence available here that the query resolved a database at all.
    if all.is_empty() {
        return Err("no beads read at all — cannot say whether anything is firing".into());
    }
    // THE LABELS ARE THE WHOLE PREDICATE, and `issue_type == "event"` is deliberately not
    // part of it even though the writer's contract says every alert bead is one. Two reasons,
    // and the second is the load-bearing one. Every other view here selects on labels alone,
    // so a type check would be the one place a bead's shape mattered. And a second condition
    // can only ever REMOVE rows: a correctly-labelled alert filed with the wrong type would
    // then be a firing condition that the tab silently does not show, which is the failure
    // direction this whole design refuses — the same reasoning that makes an unparseable
    // `silent-until:` silence nothing. Being too permissive costs a spurious row somebody can
    // see and fix; being too strict costs the outage.
    let mut out: Vec<Item> = all
        .iter()
        .filter(|r| {
            let l = labels(r);
            l.contains(&crate::model::alert::LABEL) && l.contains(&"overseer")
        })
        .map(|r| {
            let l = labels(r);
            let flaps: u32 = l
                .iter()
                .find_map(|x| x.strip_prefix(crate::model::alert::FLAPS)?.trim().parse().ok())
                .unwrap_or(1);
            let it = Item {
                id: r["id"].as_str().unwrap_or("?").to_string(),
                title: r["title"].as_str().unwrap_or("").to_string(),
                // NO `default:` LINE, EVER. An alert carries no recommended default, because
                // there is nothing to choose until he has looked; `lead` matches any line
                // containing the word, so leaving it blank HERE is what makes that a
                // guarantee rather than a tendency. Same reasoning as an insight's.
                lead: String::new(),
                body: r["description"].as_str().unwrap_or("").to_string(),
                // The flap count rides the badge so it reaches the rule and the reader's
                // header; the list row prints it too. "alert" alone when it has only ever
                // fired once — a "×1" is noise that makes a real "×7" harder to spot.
                badge: if flaps > 1 {
                    format!("alert ×{flaps}")
                } else {
                    "alert".into()
                },
                // FIRST SEEN, not last. A flapping condition reopens its own bead rather than
                // cutting a new one, so `created_at` is when the condition was first true and
                // the age on the row is how long it has been going unfixed.
                when: r["created_at"].as_str().unwrap_or("").to_string(),
                enacted: None,
                thread: oldest_first(
                    s.threads
                        .get(r["id"].as_str().unwrap_or(""))
                        .map(|cs| {
                            cs.iter()
                                .map(|c| {
                                    (
                                        c["author"].as_str().unwrap_or("?").to_string(),
                                        c["created_at"]
                                            .as_str()
                                            .or_else(|| c["timestamp"].as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        c["text"].as_str().unwrap_or("").to_string(),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
                labels: l.iter().map(|x| x.to_string()).collect(),
            };
            // Carried alongside rather than added to `Item`: status answers one question in
            // one branch, and a field the other four views ignore is a field that will drift.
            (r["status"].as_str() != Some("closed"), it)
        })
        // Firing and audible, versus everything the tab is deliberately not showing. A
        // silence is compared against `now` rather than merely being present, so it expires
        // by being read and nothing has to run to lift it.
        .filter(|(firing, it)| {
            let quiet = crate::model::silenced(it, now);
            if dismissed {
                !firing || quiet
            } else {
                *firing && !quiet
            }
        })
        .map(|(_, it)| it)
        .collect();
    out.sort_by(|a, b| a.when.cmp(&b.when));
    // The history reads newest-first: what cleared most recently is what a reader coming to
    // it wants, where the live tab's question is what has been true longest.
    if dismissed {
        out.reverse();
    }
    Ok(out)
}

/// Does the OPERATOR have the last word on this bead's thread?
///
/// The FYI view lets a dismissed insight back onto the tab when a conversation is still
/// owed an answer, and this is the question it should have been asking. It asked
/// `comment_count > 0` instead — whether ANYONE had ever spoken — so an insight the
/// operator dismissed came straight back the moment it carried any comment at all,
/// including my own replies to it. Dismissal was therefore permanently ineffective on
/// exactly the insights that had been discussed, which are the ones most likely to be
/// finished. Measured 2026-09-07 after the operator dismissed five and watched all five
/// return: hq-m2x9, hq-wuog, hq-mb25, hq-9b8z and sp-nm7 were each archived, each carried
/// two to six comments, and on every one of them the last word was mine. Nothing was owed
/// on any of them.
///
/// Ordered before it is read, by the same rule `oldest_first` uses and for the same reason:
/// `bd comments` returns oldest-first today and a predicate whose meaning inverts with
/// someone else's `ORDER BY` should not rest on that. Only sorted when every comment carries
/// a full stamp, so one unstamped row cannot hoist itself to the end of the conversation.
///
/// FALSE WHEN THERE IS NO THREAD, which is the safe direction here: an insight with nothing
/// said on it is a notice, and dismissing a notice must end it.
fn operator_spoke_last(s: &Snapshot, id: &str) -> bool {
    let Some(cs) = s.threads.get(id) else {
        return false;
    };
    let mut t: Vec<(&str, &str)> = cs
        .iter()
        .map(|c| {
            (
                c["author"].as_str().unwrap_or(""),
                c["created_at"]
                    .as_str()
                    .or_else(|| c["timestamp"].as_str())
                    .unwrap_or(""),
            )
        })
        .collect();
    if t.iter().all(|(_, when)| when.len() >= 16) {
        t.sort_by(|a, b| a.1.cmp(b.1));
    }
    let op = crate::model::operator_actor();
    t.last().map(|(a, _)| op == *a).unwrap_or(false)
}

/// Filter the cached rows down to one view. Pure, in memory, instant.
///
/// `dismissed` only means anything in record views (FYI, NOTIFICATIONS), where it swaps the
/// live rows for the ones already read. It is a parameter rather than a second function
/// because the list, the tab count and the destructive key must all agree about which set is
/// on screen: the tab said 0 while the list showed twelve is exactly the reassuring-lie shape
/// `settled` exists to prevent, one layer up.
pub fn view_items(
    s: &Snapshot,
    view: View,
    dismissed: bool,
    now: i64,
) -> Result<Vec<Item>, String> {
    if view == View::Alerts {
        return alerts(s, dismissed, now);
    }
    match (&s.beads, &s.beads_err) {
        (_, Some(e)) => Err(e.clone()),
        (None, _) => Err("loading…".into()),
        (Some(all), _) => {
            let ask = ask_label();
            let want_insight = view == View::Insights;
            let want_event = view == View::Notifications;
            let mut out: Vec<Item> = all
                .iter()
                .filter(|r| {
                    let l = labels(r);
                    let is_insight = l.contains(&"insight");
                    // THE EVENT ARM IS MATCHED FIRST, AND EVENTS ARE EXCLUDED BY TYPE BELOW.
                    // Both halves are load-bearing and neither is sufficient. DECISIONS matches
                    // on `needs-ryan || overseer`, so an event carrying either — and an emitter
                    // reaching for `ask.sh`'s vocabulary will carry `overseer` — would land in
                    // the queue of things waiting on the operator. Same shape as the bug where
                    // requiring `overseer` hid every rig escalation: a filter reading labels
                    // alone cannot tell two kinds of thing apart, so the kind has to be read
                    // from the type.
                    let is_event = r["issue_type"].as_str() == Some("event");
                    if want_event {
                        if !is_event {
                            return false;
                        }
                        // An ALERT is a special-purpose event that belongs to the ALERTS tab,
                        // not here. The ALERTS tab selects on the `alert` label, not on
                        // `issue_type`, so a mislabelled alert still shows there; this side
                        // excludes the correctly-labelled ones from NOTIFICATIONS so they
                        // cannot appear on both tabs at once.
                        if l.contains(&crate::model::alert::LABEL) {
                            return false;
                        }
                        // Events are created CLOSED — they are records, not work — so status
                        // says nothing about whether the operator has read one. `archived` does,
                        // and it is the same label FYI dismisses with, so `h` retrieves here
                        // for free and the two views cannot disagree about what "read" means.
                        return l.contains(&crate::model::ARCHIVED) == dismissed;
                    }
                    if is_event {
                        return false;
                    }
                    if want_insight {
                        // Insights are a town-side record, created closed. `archived` is
                        // the dismissal: an insight the operator has read leaves the pane, and
                        // `h` is what brings the read ones back. Same label `ask.sh list
                        // insights` filters on, so the two paths cannot disagree.
                        if !(l.contains(&"overseer") && is_insight) {
                            return false;
                        }
                        let arch = l.contains(&crate::model::ARCHIVED);
                        if dismissed {
                            return arch;
                        }
                        // ARCHIVING ENDS A NOTICE, NOT A CONVERSATION. Dismissal is how a
                        // read FYI leaves the pane, which is right for a notice nobody
                        // replied to. But an insight the operator has commented on is a
                        // thread they are owed an answer in, and archiving it hid my
                        // replies from the only surface they read them in — they asked
                        // where the acknowledgement was, and it was behind this filter.
                        //
                        // OWED, NOT MERELY DISCUSSED. This read `comment_count > 0`, so
                        // any insight that had ever been spoken on came back however the
                        // conversation ended — including when the last thing in it was my
                        // own reply. Dismissal did nothing to precisely the finished ones,
                        // and the operator dismissed five and watched all five return.
                        // The question is whose turn it is, and `operator_spoke_last`
                        // asks it.
                        return !arch
                            || operator_spoke_last(s, r["id"].as_str().unwrap_or(""));
                    }
                    if is_insight {
                        return false;
                    }
                    // AN ALERT IS NEVER A DECISION, and this exclusion is load-bearing
                    // rather than defensive: an alert bead carries `overseer`, and the
                    // predicate below admits the escalation label OR `overseer`, so every
                    // firing alert would otherwise appear HERE — in the one list whose
                    // whole value is that nothing leaves it unless the operator moved it.
                    // See the module header of `model` for why the discriminator is
                    // lifecycle.
                    if l.contains(&crate::model::alert::LABEL) {
                        return false;
                    }
                    // DECISIONS is "what is waiting on the operator", and the label that
                    // means exactly that is the configured escalation label -- the same
                    // one the escalation gate defers on and every predicate excludes. An
                    // escalation raised by a worker carries only that label, never
                    // `overseer`, so requiring `overseer` hid every one of them.
                    let waiting = l.contains(&ask.as_str()) || l.contains(&"overseer");
                    if !waiting {
                        return false;
                    }
                    // An EPIC is a container for a branch of work, not a question, so it
                    // can never be a decision however it is labelled. Two once reached the
                    // operator carrying the escalation label from a deferred-sweep triage,
                    // with nothing in them but two lines of branch metadata, and the answer
                    // to "what is the decision?" was: none. Structural exclusion, not a
                    // guess about content.
                    if r["issue_type"].as_str() == Some("epic") {
                        return false;
                    }
                    // And NOT `status == open`: an escalated bead is legitimately parked
                    // in `deferred` -- that is the one status the policy allows it to sit
                    // in, and filtering on `open` once hid every bead that was in it.
                    // Anything not closed is still awaiting an answer.
                    r["status"].as_str() != Some("closed")
                })
                .map(|r| {
                    let l = labels(r);
                    let is_event = r["issue_type"].as_str() == Some("event");
                    // AN EVENT'S BADGE IS ITS OWN COLUMN, not a guess from its labels.
                    // `event_kind` is what the emitter said this was — `spira.landed`,
                    // `wisp.compaction` — and it is the only field that distinguishes a
                    // landing from a reclaim from a CI verdict. Deriving it from labels
                    // would badge every event identically and lose the one thing the
                    // view exists to show.
                    let badge = if is_event {
                        r["event_kind"].as_str().unwrap_or("event")
                    } else if l.contains(&"insight") {
                        "insight"
                    } else if l.contains(&"ask-suit") {
                        // A lawsuit has its own badge — distinct from a plain ask — because
                        // its verdicts (uphold/retire/amend) are different from a decision's.
                        "lawsuit"
                    } else if l.contains(&"ask-law") {
                        "law proposal"
                    } else if l.contains(&"ask-decision") {
                        "decision"
                    } else if l.contains(&"ask-task") {
                        "task"
                    } else {
                        "question"
                    };
                    // An event with no prose still has to say something, and its payload
                    // is the record. Better a raw JSON line than a blank detail pane.
                    let desc = match r["description"].as_str() {
                        Some(d) if !d.trim().is_empty() => d,
                        _ if is_event => r["payload"].as_str().unwrap_or(""),
                        _ => r["description"].as_str().unwrap_or(""),
                    };
                    // AN INSIGHT HAS NO DEFAULT AND NO CALL TO ACTION. A `default:` line
                    // is a recommendation awaiting agreement, which is precisely the
                    // "implicitly asking me for feedback" shape; `lead` matches any line
                    // containing the word, so suppressing it here is what guarantees one
                    // can never appear on an FYI rather than merely usually not appearing.
                    // NO `lead` ON EVENTS EITHER. An event has already happened, so there
                    // is nothing to agree to.
                    let is_record = badge == "insight" || is_event;
                    let item_lead = if is_record { String::new() } else { lead(desc) };
                    let item_body = if badge == "insight" {
                        fyi_body(desc)
                    } else if !item_lead.is_empty() {
                        ask_body(desc)
                    } else {
                        desc.to_string()
                    };
                    Item {
                        id: r["id"].as_str().unwrap_or("?").to_string(),
                        title: r["title"].as_str().unwrap_or("").to_string(),
                        lead: item_lead,
                        body: item_body,
                        badge: badge.to_string(),
                        when: r["created_at"].as_str().unwrap_or("").to_string(),
                        enacted: enacted(&l),
                        thread: oldest_first(
                            s.threads
                                .get(r["id"].as_str().unwrap_or(""))
                                .map(|cs| {
                                    cs.iter()
                                        .map(|c| {
                                            (
                                                c["author"].as_str().unwrap_or("?").to_string(),
                                                c["created_at"]
                                                    .as_str()
                                                    .or_else(|| c["timestamp"].as_str())
                                                    .unwrap_or("")
                                                    .to_string(),
                                                c["text"].as_str().unwrap_or("").to_string(),
                                            )
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        ),
                        labels: l.iter().map(|x| x.to_string()).collect(),
                    }
                })
                .collect();
            // DECISIONS renders newest-first: bd list returns beads in that order, so
            // reversing would put the ten-hour-old row on top. For all other views the
            // oldest item is the most relevant (longest-unanswered insight, oldest
            // notification not yet archived), so the reverse stays.
            if view != View::Decisions {
                out.reverse();
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact body `ask.sh` wrote for every insight before this change, verbatim from
    /// the long insight in `tests/fixture.json`. An insight that opens "what is blocked" and closes by
    /// asking for a verdict IS a bug report, whatever the label says, which is what the operator was
    /// reading. The prose in between is the author's and is left alone.
    #[test]
    fn an_insight_loses_the_escalation_templates_call_to_action() {
        let got = fyi_body(concat!(
            "**What is blocked:** The sentinel rebases branches onto master mechanically.\n",
            "\n",
            "The PR head a required check tests is a merge of the two.\n",
            "\n",
            "_Filed by the brain session. Answer inline in the cockpit pane, or:_\n",
            "`.claude/cockpit/ask.sh answered <id> \"<verdict>\"`",
        ));
        assert_eq!(
            got,
            "The sentinel rebases branches onto master mechanically.\n\nThe PR head a required check tests is a merge of the two."
        );
    }

    /// The trailer `ask.sh` writes now is true and belongs in the stored record — and it is
    /// still boilerplate the pane will not spend two of nineteen rows repeating.
    #[test]
    fn the_recorded_trailer_goes_too() {
        let got = fyi_body(concat!(
            "**Why it matters:** a box over the disk threshold with a dead alert channel reads green.\n",
            "\n",
            "_Recorded by the brain session. Nothing is owed — this is a record, not a_\n",
            "_request. Press `d` in the cockpit pane to dismiss it; `h` brings it back._",
        ));
        assert_eq!(
            got,
            "a box over the disk threshold with a dead alert channel reads green."
        );
    }

    /// Evidence is not boilerplate: it is the finding. It sits above the trailer and stays.
    #[test]
    fn the_evidence_block_survives() {
        let got = fyi_body(concat!(
            "**Why it matters:** 37 pipelines are exposed.\n",
            "\n",
            "**Evidence**\n```\ngit log | grep -q foo\n```\n",
            "\n",
            "_Recorded by the brain session. Nothing is owed._",
        ));
        assert!(got.contains("**Evidence**"), "{got:?}");
        assert!(got.contains("grep -q foo"), "{got:?}");
        assert!(!got.contains("Recorded by"), "{got:?}");
    }

    /// A body with neither label nor trailer is returned untouched — most insights written
    /// from now on by anything other than `ask.sh` will look like this.
    #[test]
    fn a_plain_body_is_left_alone() {
        let b = "one line.\n\nand another.";
        assert_eq!(fyi_body(b), b);
    }

    /// When the description IS only the Default recommendation, `ask_body` returns an empty
    /// string — the lead already renders it as "default: …", and a body that only repeats
    /// that sentence costs a row and says nothing new.
    #[test]
    fn the_lead_line_is_removed_from_the_ask_body() {
        assert_eq!(ask_body("**Default:** do X."), "");
        // A description with prose before and after keeps everything else.
        let desc = "Why this matters.\n\n**Default:** do X.\n\nMore context.";
        let body = ask_body(desc);
        assert!(body.contains("Why this matters."), "{body:?}");
        assert!(body.contains("More context."), "{body:?}");
        assert!(!body.contains("Default"), "{body:?}");
        assert!(!body.contains("do X"), "{body:?}");
    }

    // ── NOTIFICATIONS: events as outcomes ────────────────────────────────────────────
    //
    // Every one of these asserts a property that keeps NOTIFICATIONS separate from DECISIONS.
    // The event arm is matched first and events are excluded by type below; both halves are
    // needed and neither is sufficient alone.

    /// One row, in the shape `bd list --all --json` returns.
    fn event_row(id: &str, extra_labels: &[&str]) -> Value {
        let mut labels = vec!["overseer"];
        labels.extend_from_slice(extra_labels);
        serde_json::json!({
            "id": id,
            "title": format!("t {id}"),
            "description": "what happened",
            "status": "closed",
            "issue_type": "event",
            "labels": labels,
            "event_kind": "spira.landed",
            "payload": r#"{"bead":"sp-q7k"}"#,
            "created_at": "2026-09-06T10:00:00Z",
            "comment_count": 0,
        })
    }

    fn notif_ids(s: &Snapshot, dismissed: bool) -> Vec<String> {
        view_items(s, View::Notifications, dismissed, NOW)
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect()
    }

    fn decision_ids(s: &Snapshot) -> Vec<String> {
        view_items(s, View::Decisions, false, NOW)
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect()
    }

    /// THE FILTER-ORDER TRAP, stated as a test. DECISIONS matches `needs-ryan || overseer`
    /// and excludes insights BY LABEL, so an event carrying either label would land in the
    /// queue of things waiting on the operator. Two halves are needed: the event arm matched
    /// first, AND the decisions arm excluding on `issue_type`. Same shape as the bug where
    /// requiring `overseer` hid every rig escalation.
    #[test]
    fn an_event_is_a_notification_and_never_a_decision() {
        // AN *OPEN* EVENT, deliberately. DECISIONS also drops anything closed, and every
        // event `sp-emit` writes is closed — so a closed fixture here would pass on the
        // status check alone and prove nothing about the type check.
        let mut ev = event_row("sp-ev", &["needs-operator", "spira", "plan"]); // literal-ok: test fixture data
        ev["status"] = Value::from("open");
        // A closed decision is not shown either, so the control has to be genuinely open.
        let mut open_ask = serde_json::json!({
            "id": "sp-ask", "title": "t", "description": "body",
            "status": "open", "issue_type": "task",
            "labels": ["needs-operator", "overseer"], // literal-ok: test fixture data
            "created_at": "2026-09-06T10:00:00Z", "comment_count": 0,
        });
        open_ask["status"] = Value::from("open");
        let s = snap(vec![
            ev,
            open_ask,
            // ...and the ordinary closed event still reaches NOTIFICATIONS.
            event_row("sp-ev2", &[]),
        ]);
        assert_eq!(notif_ids(&s, false), ["sp-ev2", "sp-ev"]);
        assert_eq!(decision_ids(&s), ["sp-ask"]);
    }

    /// THE POSITIVE CONTROL. A filter that returned nothing at all would satisfy "no event in
    /// DECISIONS" while being completely broken. Same labels, same status: only the type
    /// differs, so the exclusion is the type doing the work and nothing else
    /// (law-absence-needs-a-positive-control).
    #[test]
    fn the_same_row_as_a_task_is_a_decision() {
        let mut r = serde_json::json!({
            "id": "sp-x", "title": "t", "description": "body",
            "status": "open", "issue_type": "task",
            "labels": ["needs-operator", "overseer"], // literal-ok: test fixture data
            "created_at": "2026-09-06T10:00:00Z", "comment_count": 0,
        });
        r["status"] = Value::from("open");
        let s = snap(vec![r]);
        assert_eq!(decision_ids(&s), ["sp-x"]);
        assert!(notif_ids(&s, false).is_empty());
    }

    /// An event is created CLOSED, so `archived` is the only thing "read" can mean.
    /// `h` toggles between live events and archived ones; both use the same label as FYI.
    #[test]
    fn a_read_event_leaves_the_pane_and_h_brings_it_back() {
        let s = snap(vec![
            event_row("sp-new", &[]),
            event_row("sp-read", &[crate::model::ARCHIVED]),
        ]);
        assert_eq!(notif_ids(&s, false), ["sp-new"]);
        assert_eq!(notif_ids(&s, true), ["sp-read"]);
    }

    /// The badge is what distinguishes a landing from a reclaim from a CI verdict, and only
    /// `event_kind` carries that. Derived from labels it would badge every event alike.
    /// An event also never has a `lead` — it has already happened; there is nothing to agree to.
    #[test]
    fn an_events_badge_is_its_event_kind_and_it_has_no_lead() {
        let s = snap(vec![event_row("sp-ev", &[])]);
        let it = &view_items(&s, View::Notifications, false, NOW).unwrap()[0];
        assert_eq!(it.badge, "spira.landed");
        assert_eq!(it.lead, "");
    }

    /// THE OPTIMISTIC HIDE ENDS WHEN THE WRITE IS VISIBLE. Archiving labels a row; it does not
    /// remove it. Pruning on presence therefore never released a dismissed event, so `A` then
    /// `h` rendered an empty history over the burst just archived.
    #[test]
    fn an_archived_row_stops_being_hidden_once_the_label_lands() {
        let before = snap(vec![event_row("sp-ev", &[])]);
        assert!(!settled(&before, "sp-ev", Expect::Archived(true)));
        let after = snap(vec![event_row("sp-ev", &[crate::model::ARCHIVED])]);
        assert!(settled(&after, "sp-ev", Expect::Archived(true)));
        assert!(!settled(&after, "sp-ev", Expect::Archived(false)));
        assert!(settled(&before, "sp-ev", Expect::Archived(false)));
    }

    /// A close settles on the status; a row gone from the store entirely is also settled.
    #[test]
    fn a_close_settles_on_the_status_or_on_the_row_going_away() {
        let mut open = serde_json::json!({
            "id": "sp-ask", "title": "t", "description": "",
            "status": "open", "issue_type": "task",
            "labels": ["needs-operator"], "created_at": "2026-09-06T10:00:00Z", // literal-ok: test fixture data
        });
        open["status"] = Value::from("open");
        let s = snap(vec![open]);
        assert!(!settled(&s, "sp-ask", Expect::Closed));
        let closed = snap(vec![serde_json::json!({
            "id": "sp-ask", "title": "t", "status": "closed",
            "issue_type": "task", "labels": [],
        })]);
        assert!(settled(&closed, "sp-ask", Expect::Closed));
        // A row gone from the store has nothing left to hide.
        assert!(settled(&s, "sp-gone", Expect::Closed));
    }

    /// Nothing has synced yet: hide everything rather than releasing a row prematurely.
    #[test]
    fn an_unsynced_store_settles_nothing() {
        let s = Snapshot::default();
        assert!(!settled(&s, "sp-ev", Expect::Archived(true)));
        assert!(!settled(&s, "sp-ask", Expect::Closed));
    }

    /// An event whose emitter wrote only a payload still has to say something.
    #[test]
    fn an_event_with_no_prose_falls_back_to_its_payload() {
        let mut r = event_row("sp-ev", &[]);
        r["description"] = Value::from("");
        let s = snap(vec![r]);
        assert_eq!(
            view_items(&s, View::Notifications, false, NOW).unwrap()[0].body,
            r#"{"bead":"sp-q7k"}"#
        );
    }

    // ── ALERTS: a condition that self-clears ─────────────────────────────────────────
    //
    // Every one of these asserts a property that distinguishes an alert from a decision.
    // Nothing about the SHAPE of the code announces that letting an alert into DECISIONS, or
    // letting an empty read render as all-clear, is wrong — so the checks say it instead.

    fn snap(rows: Vec<Value>) -> Snapshot {
        Snapshot { beads: Some(rows), ..Default::default() }
    }

    fn bead(id: &str, status: &str, created: &str, labels: &[&str]) -> Value {
        serde_json::json!({
            "id": id,
            "title": format!("{id} fired"),
            "description": "the condition",
            "issue_type": "event",
            "status": status,
            "created_at": created,
            "labels": labels,
        })
    }

    /// An insight, with the comment count the FYI filter used to trust.
    fn fyi(id: &str, comments: u64, archived: bool) -> Value {
        let mut labels = vec!["insight", "overseer"];
        if archived {
            labels.push("archived");
        }
        serde_json::json!({
            "id": id,
            "title": format!("{id} noticed"),
            "description": "what was learned",
            "issue_type": "task",
            "status": "closed",
            "created_at": "2026-09-05T10:00:00Z",
            "comment_count": comments,
            "labels": labels,
        })
    }

    /// The operator's author name is CONFIGURED (`SPIRA_OPERATOR_ACTOR`), not the literal
    /// "ryan" the live database happens to hold — so the tests ask for it the same way the
    /// filter does. Writing the name in by hand made the owed-a-reply case fail against a
    /// correct filter, which is the test asserting about one box rather than about the rule.
    fn op() -> String {
        crate::model::operator_actor()
    }

    fn threaded(rows: Vec<Value>, id: &str, authors: &[(&str, &str)]) -> Snapshot {
        let mut s = snap(rows);
        s.threads.insert(
            id.to_string(),
            authors
                .iter()
                .map(|(who, when)| serde_json::json!({"author": who, "created_at": when, "text": "…"}))
                .collect(),
        );
        s
    }

    /// THE ONE THE OPERATOR HIT. Five archived insights came back because they carried
    /// comments — and the last word on every one of them was mine, so nothing was owed.
    #[test]
    fn a_dismissed_insight_i_answered_last_stays_dismissed() {
        let s = threaded(
            vec![fyi("sp-nm7", 2, true)],
            "sp-nm7",
            &[(&op(), "2026-09-06T10:00:00Z"), ("claude", "2026-09-06T11:00:00Z")],
        );
        assert!(ids(view_items(&s, View::Insights, false, NOW)).is_empty());
        // And it is still reachable, which is the whole reason dismissal is reversible.
        assert_eq!(ids(view_items(&s, View::Insights, true, NOW)), ["sp-nm7"]);
    }

    /// The half worth keeping: a thread where the OPERATOR spoke last is still owed an answer,
    /// and archiving it must not hide the conversation from the only surface they read.
    #[test]
    fn a_dismissed_insight_the_operator_spoke_last_on_comes_back() {
        let s = threaded(
            vec![fyi("sp-owed", 3, true)],
            "sp-owed",
            &[
                (&op(), "2026-09-06T10:00:00Z"),
                ("claude", "2026-09-06T11:00:00Z"),
                (&op(), "2026-09-06T12:00:00Z"),
            ],
        );
        assert_eq!(ids(view_items(&s, View::Insights, false, NOW)), ["sp-owed"]);
    }

    /// Order is not trusted: the same thread shuffled must give the same verdict, because
    /// `bd comments` ordering is somebody else's `ORDER BY`.
    #[test]
    fn whose_turn_it_is_does_not_depend_on_the_rows_arriving_in_order() {
        let s = threaded(
            vec![fyi("sp-shuf", 2, true)],
            "sp-shuf",
            &[("claude", "2026-09-06T11:00:00Z"), (&op(), "2026-09-06T10:00:00Z")],
        );
        assert!(ids(view_items(&s, View::Insights, false, NOW)).is_empty());
    }

    /// A dismissed notice nobody ever replied to simply leaves. The positive control for the
    /// two above: without it, a filter that hid everything archived would pass them both.
    #[test]
    fn a_dismissed_insight_with_no_thread_leaves_the_tab() {
        let s = snap(vec![fyi("sp-quiet", 0, true)]);
        assert!(ids(view_items(&s, View::Insights, false, NOW)).is_empty());
        assert_eq!(ids(view_items(&s, View::Insights, true, NOW)), ["sp-quiet"]);
    }

    /// An insight nobody has dismissed is on the tab whatever its thread says.
    #[test]
    fn an_undismissed_insight_is_on_the_tab_regardless_of_whose_turn_it_is() {
        let s = threaded(
            vec![fyi("sp-live", 2, false)],
            "sp-live",
            &[(&op(), "2026-09-06T10:00:00Z"), ("claude", "2026-09-06T11:00:00Z")],
        );
        assert_eq!(ids(view_items(&s, View::Insights, false, NOW)), ["sp-live"]);
    }

    fn firing(id: &str, created: &str) -> Value {
        bead(id, "open", created, &["alert", "overseer"])
    }

    /// 2026-09-05T16:20:00Z, the same frozen instant the render tests use.
    const NOW: i64 = 1_788_625_200;

    fn ids(r: Result<Vec<Item>, String>) -> Vec<String> {
        r.unwrap().into_iter().map(|i| i.id).collect()
    }

    /// The writer's contract says an alert bead is `issue_type: event`, and the tab does NOT
    /// select on that — see the comment on the filter. Pinned here because the natural
    /// instinct on reading the contract is to add the check, and adding it would turn a
    /// mislabelled type into a firing condition nobody is shown. The failure direction has to
    /// stay "you still see it".
    #[test]
    fn a_wrongly_typed_alert_is_still_shown() {
        let mut odd = firing("sp-typed", "2026-09-05T10:00:00Z");
        odd["issue_type"] = serde_json::json!("task");
        let got = ids(alerts(&snap(vec![odd]), false, NOW));
        assert_eq!(vec!["sp-typed"], got, "a labelled alert must not vanish on its type");
    }

    /// An alert bead carries `overseer`, and DECISIONS admits the escalation label OR `overseer` — so
    /// without an explicit exclusion every firing alert lands in the one list whose whole
    /// value is that nothing leaves it unless the operator moved it. This is the check that the
    /// exclusion exists, not merely that the labels happen not to overlap today.
    #[test]
    fn a_firing_alert_is_not_a_decision() {
        // sp-d1 is a genuine task-type decision bead, not an event.  The local `bead()`
        // helper hardcodes `issue_type: "event"` for the alerts test context, so this one
        // is inlined as a plain task the way every other decisions-section bead is.
        let d1 = serde_json::json!({
            "id": "sp-d1", "title": "a question", "description": "what to do?",
            "status": "open", "issue_type": "task",
            "labels": ["needs-operator", "overseer"], // literal-ok: test fixture data
            "created_at": "2026-09-05T10:00:00Z", "comment_count": 0,
        });
        let s = snap(vec![
            firing("sp-a1", "2026-09-05T10:00:00Z"),
            d1,
        ]);
        assert_eq!(ids(view_items(&s, View::Decisions, false, NOW)), ["sp-d1"]);
        assert_eq!(ids(view_items(&s, View::Alerts, false, NOW)), ["sp-a1"]);
        // And it is not an FYI either — that view wants `insight`.
        assert!(ids(view_items(&s, View::Insights, false, NOW)).is_empty());
        // And not a notification — the event arm is checked by a_wrongly_typed_alert_is_still_shown.
        assert!(ids(view_items(&s, View::Notifications, false, NOW)).is_empty());
    }

    /// THE POSITIVE CONTROL (law-absence-needs-a-positive-control). "No alerts" and "the
    /// reader resolved no database" are the same pixels, and the wrong one reads as
    /// all-clear. An empty read is refused; a read that returned rows and matched none is
    /// a genuine all-clear and is allowed through.
    #[test]
    fn an_empty_read_is_refused_and_a_proved_read_is_not() {
        assert!(view_items(&snap(vec![]), View::Alerts, false, NOW).is_err());
        let other = snap(vec![bead(
            "sp-d1",
            "open",
            "2026-09-05T10:00:00Z",
            &["needs-operator", "overseer"], // literal-ok: test fixture data
        )]);
        assert_eq!(view_items(&other, View::Alerts, false, NOW).unwrap().len(), 0);
        // And a reader that is genuinely broken is an error, never an empty list.
        let broken = Snapshot { beads_err: Some("bd: no such database".into()), ..Default::default() };
        assert!(view_items(&broken, View::Alerts, false, NOW).is_err());
    }

    /// Oldest first, and stated rather than inherited from `bd list`'s own ordering: the
    /// condition that has been true longest is the one that is not fixing itself.
    #[test]
    fn alerts_are_sorted_by_first_seen_oldest_first() {
        let s = snap(vec![
            firing("sp-new", "2026-09-05T16:00:00Z"),
            firing("sp-old", "2026-09-01T09:00:00Z"),
            firing("sp-mid", "2026-09-04T09:00:00Z"),
        ]);
        assert_eq!(
            ids(view_items(&s, View::Alerts, false, NOW)),
            ["sp-old", "sp-mid", "sp-new"]
        );
    }

    /// THE TAB SELF-CLEARS. A closed alert is a retracted statement and leaves with nobody
    /// touching it — and stays reachable behind the history toggle, the way a dismissed FYI
    /// does. This is the property that makes an alert not a decision.
    #[test]
    fn a_cleared_alert_leaves_the_tab_on_its_own_and_stays_reachable() {
        let s = snap(vec![
            firing("sp-live", "2026-09-05T10:00:00Z"),
            bead("sp-gone", "closed", "2026-09-04T10:00:00Z", &["alert", "overseer"]),
        ]);
        assert_eq!(ids(view_items(&s, View::Alerts, false, NOW)), ["sp-live"]);
        assert_eq!(ids(view_items(&s, View::Alerts, true, NOW)), ["sp-gone"]);
    }

    /// A SILENCE EXPIRES BY BEING READ. The deadline lives in the label, so nothing has to
    /// run to lift it — a silence that needs a process to end it is a silence that outlives
    /// the process, and the condition then sits unreported with no record of why.
    #[test]
    fn a_silence_expires_on_its_own_deadline() {
        let quiet = |until: &str| {
            snap(vec![bead(
                "sp-q",
                "open",
                "2026-09-05T10:00:00Z",
                &["alert", "overseer", until],
            )])
        };
        let ahead = quiet("silent-until:2026-09-05T17:00:00Z");
        assert!(ids(view_items(&ahead, View::Alerts, false, NOW)).is_empty());
        assert_eq!(ids(view_items(&ahead, View::Alerts, true, NOW)), ["sp-q"]);

        let passed = quiet("silent-until:2026-09-05T15:00:00Z");
        assert_eq!(ids(view_items(&passed, View::Alerts, false, NOW)), ["sp-q"]);
        assert!(ids(view_items(&passed, View::Alerts, true, NOW)).is_empty());

        // A deadline that will not parse is NO silence, never an eternal one: the failure
        // direction has to be "you still see it".
        let junk = quiet("silent-until:whenever");
        assert_eq!(ids(view_items(&junk, View::Alerts, false, NOW)), ["sp-q"]);
    }

    /// ACKNOWLEDGING IS NOT CLEARING. An acked alert is still firing, so it stays on the tab;
    /// hiding it because someone looked at it is the reassuring lie this pane exists to
    /// prevent, and it would make the tab disagree with the world.
    #[test]
    fn an_acknowledged_alert_is_still_on_the_tab() {
        let s = snap(vec![bead(
            "sp-seen",
            "open",
            "2026-09-05T10:00:00Z",
            &["alert", "overseer", "acked"],
        )]);
        let got = view_items(&s, View::Alerts, false, NOW).unwrap();
        assert_eq!(got.len(), 1);
        assert!(crate::model::acked(&got[0]));
    }

    /// The flap count reaches the badge, and only when there is one to show: a "×1" on every
    /// row is noise that makes a real "×7" harder to spot. An alert never carries a default.
    #[test]
    fn the_flap_count_rides_the_badge_and_there_is_never_a_default() {
        let s = snap(vec![
            bead("sp-f", "open", "2026-09-05T09:00:00Z", &["alert", "overseer", "flaps:7"]),
            firing("sp-once", "2026-09-05T10:00:00Z"),
        ]);
        let got = view_items(&s, View::Alerts, false, NOW).unwrap();
        assert_eq!(got[0].badge, "alert ×7");
        assert_eq!(crate::model::flaps(&got[0]), 7);
        assert_eq!(got[1].badge, "alert");
        assert_eq!(crate::model::flaps(&got[1]), 1, "no label means it has fired once");
        for it in &got {
            assert!(it.lead.is_empty(), "an alert has nothing to choose yet: {:?}", it.lead);
        }
    }

    fn c(author: &str, when: &str) -> (String, String, String) {
        (author.into(), when.into(), format!("{author}@{when}"))
    }

    /// The pane leads with the thread and the list's turn marker reads its last entry, so a
    /// thread arriving newest-first would both read backwards and put the wrong arrow on the
    /// row. Enforced here rather than trusted to `bd comments`.
    #[test]
    fn a_thread_is_ordered_oldest_first() {
        let got = oldest_first(vec![
            c("claude", "2026-09-05T16:00:00Z"),
            c("operator", "2026-09-05T15:00:00Z"),
            c("operator", "2026-09-04T09:00:00Z"),
        ]);
        let whens: Vec<&str> = got.iter().map(|(_, w, _)| w.as_str()).collect();
        assert_eq!(
            whens,
            ["2026-09-04T09:00:00Z", "2026-09-05T15:00:00Z", "2026-09-05T16:00:00Z"]
        );
    }

    /// An unstamped comment sorts before every stamped one, which would hoist it to the top
    /// of the conversation. Better to keep the order the database gave us than to invent one.
    #[test]
    fn an_unstamped_comment_leaves_the_order_alone() {
        let got = oldest_first(vec![
            c("claude", "2026-09-05T16:00:00Z"),
            c("operator", ""),
            c("operator", "2026-09-05T15:00:00Z"),
        ]);
        let whens: Vec<&str> = got.iter().map(|(_, w, _)| w.as_str()).collect();
        assert_eq!(whens, ["2026-09-05T16:00:00Z", "", "2026-09-05T15:00:00Z"]);
    }

    /// Equal stamps keep the order they arrived in — two comments in the same second are
    /// common enough, and a reshuffle there is a conversation reordered for no reason.
    #[test]
    fn equal_stamps_keep_their_arrival_order() {
        let got = oldest_first(vec![
            c("operator", "2026-09-05T15:00:00Z"),
            c("claude", "2026-09-05T15:00:00Z"),
        ]);
        let who: Vec<&str> = got.iter().map(|(a, _, _)| a.as_str()).collect();
        assert_eq!(who, ["operator", "claude"]);
    }

    // ── VIEW ORDERING: decisions newest-first, alerts oldest-first ───────────────────
    //
    // bd list returns newest-first. The general view builder used to reverse that unconditionally,
    // making every view oldest-first. Decisions should be newest-first — a ten-hour-old row on top
    // is the one most likely to have been overtaken by everything under it (sp-dyh80). Alerts stay
    // oldest-first because the condition that has been true longest is the one not fixing itself.
    // Both directions are pinned here so a future refactor cannot silently swap them.

    fn ask_bead(id: &str, created: &str) -> Value {
        serde_json::json!({
            "id": id,
            "title": format!("{id} question"),
            "description": "what to do?",
            "status": "open",
            "issue_type": "task",
            "labels": ["needs-operator", "overseer"], // literal-ok: test fixture data
            "created_at": created,
            "comment_count": 0,
        })
    }

    /// DECISIONS renders newest-first. bd list returns beads newest-first; before sp-dyh80
    /// the unconditional reverse put the oldest bead on top, which was the one most likely to
    /// have been overtaken by everything below it.
    #[test]
    fn decisions_are_newest_first() {
        // bd list returns newest-first, so we give the snapshot rows in that order and
        // assert the view preserves it rather than reversing it.
        let s = snap(vec![
            ask_bead("sp-newest", "2026-09-09T16:00:00Z"),
            ask_bead("sp-middle", "2026-09-09T10:00:00Z"),
            ask_bead("sp-oldest", "2026-09-01T09:00:00Z"),
        ]);
        assert_eq!(
            ids(view_items(&s, View::Decisions, false, NOW)),
            ["sp-newest", "sp-middle", "sp-oldest"],
        );
    }

    /// ALERTS renders oldest-first. The condition that has been true longest is the one that is
    /// not fixing itself. This direction is deliberately opposite to DECISIONS and must stay so.
    #[test]
    fn alerts_ordering_is_opposite_to_decisions() {
        let s = snap(vec![
            firing("sp-new", "2026-09-09T16:00:00Z"),
            firing("sp-old", "2026-09-01T09:00:00Z"),
        ]);
        // Alerts: oldest on top.
        let alert_ids = ids(view_items(&s, View::Alerts, false, NOW));
        assert_eq!(alert_ids, ["sp-old", "sp-new"], "alerts must be oldest-first");
        // Decisions: newest on top (positive control — same store, different view).
        let d = snap(vec![
            ask_bead("sp-new-d", "2026-09-09T16:00:00Z"),
            ask_bead("sp-old-d", "2026-09-01T09:00:00Z"),
        ]);
        let decision_ids = ids(view_items(&d, View::Decisions, false, NOW));
        assert_eq!(decision_ids, ["sp-new-d", "sp-old-d"], "decisions must be newest-first");
    }

    // =========================================================================================
    // REFRESH (gap #3, docs/test-plan/cockpit-observability.md): the bd subprocess call
    // underneath every view had no test at all. crate::test_support::StubBd puts a recording
    // `bd` on SPIRA_PATH via the same lookup `bin()` already does.
    // =========================================================================================

    fn wait_for<F: Fn() -> bool>(cond: F) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("refresh did not complete within 2s");
    }

    #[test]
    fn refresh_populates_beads_from_the_real_bd_shape() {
        let stub = crate::test_support::StubBd::new()
            .env("SPIRA_DB", "/fake/db")
            .env(
                "BD_LIST_OUT",
                r#"{"issues":[{"id":"sp-r1","status":"open","labels":[]}]}"#,
            );
        let shared: Shared = Arc::new(Mutex::new(Snapshot::default()));
        refresh(&shared);
        wait_for(|| shared.lock().unwrap().at.is_some());
        let s = shared.lock().unwrap();
        assert_eq!(s.beads_err, None, "{:?}", s.beads_err);
        let beads = s.beads.as_ref().expect("beads populated");
        assert_eq!(beads.len(), 1);
        assert_eq!(beads[0]["id"], "sp-r1");
        drop(s);
        let log = stub.argv_log();
        assert!(log.contains("list --all"), "{log}");
    }

    /// A failing `bd list` must surface as `beads_err`, verbatim, never a silent empty list —
    /// an empty list reads as "nothing is waiting" over a queue that was never actually read.
    #[test]
    fn refresh_relays_a_bd_failure_verbatim() {
        let _stub = crate::test_support::StubBd::new()
            .env("SPIRA_DB", "/fake/db")
            .env("BD_LIST_RC", "1")
            .env("BD_LIST_ERR", "schema version mismatch: database is at v61");
        let shared: Shared = Arc::new(Mutex::new(Snapshot::default()));
        refresh(&shared);
        wait_for(|| shared.lock().unwrap().at.is_some());
        let s = shared.lock().unwrap();
        assert!(s.beads.is_none(), "a failed fetch must not populate beads");
        let e = s.beads_err.as_ref().expect("beads_err set");
        assert!(e.contains("schema version mismatch"), "{e}");
    }
}
