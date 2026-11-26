//! The brief (DESIGN.md §1.4): the runes template, the operator's overlays, the
//! session-specific blocks and the split into system.md / task.md.
//!
//! Pure: every input is in the arguments. Nothing here claims, selects or reads the store —
//! claim selection lives in claim.rs and never touches this module (sp-f0qhr). What the
//! session is told about finishing is the `work` verbs' alone (sp-wmcvb, sp-v62vn).
//!
//! Texts are dwarf.sh's and lib.sh's, verbatim. Substitution is literal (DESIGN.md §8.1).

use std::collections::BTreeMap;
use std::path::Path;

// ---- the blocks computed from repo-map / the fixture / the clock ----------------------

pub fn landing_brief(repo_land: &str, branch: &str, base_branch: &str, repo_name: &str, base: &str) -> String {
    match repo_land {
        "pr" => format!("landing-pass pushes `{branch}` and opens a pull request against `{base_branch}`; {repo_name}'s own CI is the gate, so write the commit for a reviewer"),
        "hold" => format!("Spira does not advance {repo_name}'s `{base_branch}`, so the sentinel gates `{branch}` and leaves it for the operator to merge by hand"),
        _ => format!("the sentinel merges `{branch}` into `{base_branch}` and pushes it once the landing gate passes — there is no reviewer between your commit and `{base}`"),
    }
}

pub fn park_brief(repo_land: &str, branch: &str, repo_name: &str, base_branch: &str) -> String {
    if repo_land == "pr" {
        format!(
            "## Your lifetime: do the work, then exit

**There is no CI run for you to wait on.** `landing-pass` pushes `{branch}`, opens or
refreshes its pull request, and — once that pull request merges — closes the bead for real,
citing the landed commit. None of that is this session's job, and none of it needs `gh`
credentials this session may not have.

**So do not push the branch, do not open a pull request, and do not create a gh:run gate.**
A gate here is never read by anything: nothing discovers or checks one for a bead landing-pass
already owns. Leaving the bead open is just as inert, the opposite way — `landing-pass` acts
only on a closed bead, so an open one is simply skipped, forever, by the one thing that would
otherwise push it.

When the work is committed on your branch, close the bead with its evidence and exit.
$REPO_NAME's own CI runs against the pull request `landing-pass` opens — that pull request
is the review, and its CI is the gate — but watching it is not a builder's job."
        )
    } else {
        format!(
            "## Your lifetime: do the work, then exit

**There is no CI run to wait for in this repository.** {repo_name} lands by `{repo_land}`, so
nothing opens a pull request for `{branch}` and no run will ever report on it. The landing
gate is the only gate, and once it passes there is nothing further to wait for.

**So do not create a gh:run gate for this bead.** A gate means \"parked on a run somebody
else is watching\", and it excludes the bead from ready — the mechanism that would otherwise
make the bead claimable. Applied where no run exists it is a permanent, invisible hold: not
claimable, not reported, and shown to the operator as \"in CI\", which is the one description
that stops anybody looking for the real cause.

When the work is committed on your branch, close the bead with its evidence and exit. How the
branch reaches `{base_branch}` from there is described above, and none of it needs you."
        )
    }
}

/// `{{TESTENV}}`: the testenv runner, by bare name (sp-gypjk). A restricted session's PATH
/// names the release's `model-bin/`, which links `testenv`, so it is the release's binary — never a path relative to the
/// dwarf's worktree (a dwarf told `{{SPIRA_HOME}}/../bin/testenv` once ran
/// `./spira/testenv.sh`, the container helper, instead: sp-4vq2q).
pub const TESTENV: &str = "testenv";

/// The runes's tool placeholders. The model's PATH holds only `work` (sp-st0mm), so each
/// is the `work` verb that reaches the same tool through the spira-lc broker — `{{GROOM}}
/// close <id>` is `work groom close <id>`, run by the broker under the persona's own allow
/// row. Unconditional: the runes is written for these verbs.
pub fn tool_tokens() -> Vec<(&'static str, String)> {
    vec![
        ("SOP", "work sop".to_string()),
        ("INCIDENT", "work incident".to_string()),
        ("ASK", "work ask".to_string()),
        ("GROOM", "work groom".to_string()),
        ("DEP", "work dep-add".to_string()),
    ]
}

/// Tokens never rendered: the bd database path and the harness's home — the model reaches
/// neither (sp-st0mm), so no persona prompt names them.
pub const WITHHELD: &[&str] = &["DB", "SPIRA_HOME"];

/// `{{NO_BD}}`: the persona prompts' "you have no bd" line.
pub fn no_bd_brief() -> String {
    "- You have no `bd` and no database path: every bead operation is a `work` verb, run by the
  spira-lc broker under this persona's own allow row — a verb it may not run is refused."
        .to_string()
}

/// `{{FOLLOWUP}}`: how to file work discovered rather than done — the `work` verbs, which
/// parent the child for the model and never copy its `branch:` label (sp-o4co5).
pub fn followup_brief(bead_id: &str) -> String {
    format!(
        "**File it as a bead** — `work file-followup \"<title>\" --repo <name>` for work that follows
  from this one, `work split \"<title>\" --repo <name>` for a piece of this bead's own scope.
  Both are parented to `{bead_id}` for you and never carry its `branch:` label. `--repo` is
  required and is the repo the child's work lives in; it is never copied from this bead.
  A child for a builder needs a body (`--body-file -`, text on stdin) with a `## Build` section
  naming the artifact, component or change to make, and where. A problem with no named
  deliverable is not builder work: say so in your submission instead of filing it."
    )
}

/// `{{FINISH}}`: the `work` verbs, the model's only way to act on its bead.
pub fn finish_brief(bead_id: &str) -> String {
    format!(
        "**You have no `bd`.** `bd` is not on your PATH and no database credential is in your
environment (design §3.5) — the only way you act on your bead is `work`, bound to exactly
this one: `{bead_id}`. Naming any other bead to `work` is refused.

When the work is committed on your branch:

    work submit

The tip is read from your own worktree's `HEAD` — you never pass one. This moves the bead to
SUBMITTED; the landing pass carries it from there and lands it once it certifies.

When the deliverable is not code on this branch — child beads, a document, a mail message:

    work done --delivers \"<what you produced, or where it lives>\"

When you are blocked on a question only the operator can answer:

    work blocked \"<the question>\" --default \"<what you would do by default>\" --class <permissions|policy|destructive|design> --basis \"<why it is that class>\"

This both holds the bead and files the ask; you do not also send mail yourself. The question is
the human topic and must not start with a bead id: the bound bead is attached automatically.

When you discover other work rather than doing it:

    work file-followup \"<title>\" --repo <name>
    work split \"<title>\" --repo <name>

Both file through `bead.sh`'s own contract, parented to `{bead_id}` — you never name the
parent yourself.

When you believe this bead's work already landed under another id:

    work superseded-by <successor-id>

This is a *request* — it holds the bead pending confirmation; it does not close it.

To leave a plain note on `{bead_id}` (no lifecycle effect):

    work note \"<text>\"

If you genuinely cannot finish and none of the above fits — leave a note with `work note`
saying precisely what is blocked and what you would do by default, and exit non-zero. An
honest failure is cheap. A bead whose lifecycle event doesn't match what actually happened
is expensive, because everything downstream of it acts on that record."
    )
}

/// The fixture this session built, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureInfo {
    pub name: String,
    pub dir: String,
    pub baseline: String,
    pub bin: String,
    pub started_service: String,
    pub mode: String,
    pub server_init_hash: String,
}

pub fn fixture_brief(fixture: Option<&FixtureInfo>, testdb_lib: &str, fixture_ms: u64) -> String {
    match fixture {
        Some(f) => {
            let (engine, cleanup) = if f.mode == "server" {
                (
                    "a private Dolt sql-server started for this fixture alone".to_string(),
                    "testdb_drop (which stops that server)".to_string(),
                )
            } else {
                ("the embedded Dolt engine — no shared server, no external port".to_string(), "`rm -rf` on the fixture directory".to_string())
            };
            format!(
                "**The fixture is already built.** One throwaway database was created for this
session and exported into your environment (`TESTDB_SHARED=1`, `TESTDB_NAME={}`),
so a suite that sources `{testdb_lib}` and calls `testdb_up` resets it in a fraction of
a second instead of spending the {fixture_ms}ms that build cost. Never unset those variables
and never build a database of your own: a suite that reaches past `testdb_up` pays the build
again on every run, and nothing anywhere reports that it did.

The fixture uses {engine}. Cleanup is {cleanup}.",
                f.name
            )
        }
        None => "This repository has no shared test fixture, so a suite that needs one builds
its own. If that turns out to be the slowest thing in your session, say so when you close the
bead — the number is worth having."
            .to_string(),
    }
}

/// The kill time of a call starting at `now`: the wall counts from the call, never from a
/// time carried on the bead or from when the process began.
pub fn call_deadline(timeout: Option<u64>, now: i64) -> Option<i64> {
    timeout.map(|t| now + t as i64)
}

/// A call whose wall is spent is refused before launch, naming the cause.
pub fn spent_wall(deadline_at: Option<i64>, now: i64) -> Option<String> {
    let at = deadline_at?;
    (at - now <= 0).then(|| format!("session wall already spent at launch: kill time {at}, now {now} ({} s remaining); refusing to launch", at - now))
}

/// `render_deadline_brief <at> <now>`. `hms` is the local `%H:%M:%S %Z` of `at` (None when
/// it cannot be formatted, which renders `epoch <at>`).
pub fn deadline_brief(at: Option<i64>, now: i64, hms: Option<String>) -> String {
    match at {
        Some(at) => {
            let when = hms.unwrap_or_else(|| format!("epoch {at}"));
            format!(
                "**This session is killed at {when} — {} seconds from now.**
The kill comes from outside the session, on a clock, and it is a wall rather than a request:
work in progress is discarded and anything you learned that is not written down goes with it.
Do not estimate what is left — read it, as often as you need to:

    echo $(( {at} - $(date +%s) ))",
                at - now
            )
        }
        None => "**This session has no wall-clock deadline.** It runs until its work is
done. What ends a session that is not moving is the heartbeat: it stops when nothing has
observably changed for several checks, the lease then expires, and the bead returns to the
queue. So a long session is fine and a silent one is not."
            .to_string(),
    }
}

/// `render_resume_brief`: empty unless a prior session committed on this branch.
pub fn resume_brief(branch: &str, work: &str, n: &str, log: &str) -> String {
    if n.is_empty() || n == "0" || n == "?" {
        return String::new();
    }
    format!(
        "## Prior work on this branch

`{branch}` carries **{n}** commit(s) from a previous session:

```
{log}
```

Run `git -C {work} log --oneline` and read the bead's notes (shown in \"The bead\" above)
before doing any work. The notes record why the previous session did not land. Fix that
specific problem — do not redo work that is already committed."
    )
}

pub const SLAY_MARK: &str = ": wip — salvaged at slay (";

/// `render_slain_brief`: empty unless the last commit is a slay.sh wip salvage.
pub fn slain_brief(last_subject: &str, n: &str, base: &str, slay_when: &str, diffstat: &str, logf: &str) -> String {
    if !last_subject.contains(SLAY_MARK) {
        return String::new();
    }
    let why = last_subject.rsplit_once("salvaged at slay (").map(|(_, w)| w).unwrap_or("");
    let why = why.strip_suffix(')').unwrap_or(why);
    let when = if slay_when.is_empty() { "unknown time" } else { slay_when };
    let why = if why.is_empty() { "unknown reason" } else { why };
    let ds = if diffstat.is_empty() { String::new() } else { format!(" ({diffstat})") };
    format!(
        "## A previous attempt was slain

A prior session was slain at {when} ({why}). The branch carries **{n}** commit(s) beyond `{base}`{ds}; the last is a salvaged wip commit — review it before building on it.

Prior transcript (do not inline; read only if needed): `{logf}`"
    )
}

/// `render_holds_brief`: a visible marker when holds.sh could not check; else the other
/// beads' lines (this bead's own filtered out), or empty. Trailing newline stripped, as the
/// `$(...)` that captured it did.
pub fn holds_brief(bead_id: &str, rc: i32, out: &str) -> String {
    if rc != 0 {
        return "## Files already in flight — COULD NOT CHECK

holds.sh could not complete the check (see its stderr). Treat this as unknown, not as clear —
grep for other branches touching the same files yourself before you edit."
            .to_string();
    }
    let lines: Vec<(&str, &str)> = out
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| l.split('\t').next() != Some(bead_id))
        .map(|l| {
            let mut p = l.splitn(2, '\t');
            (p.next().unwrap_or(""), p.next().unwrap_or(""))
        })
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let mut s = "## Files already in flight

Another open bead already has a branch touching one of the files this bead names. Read it
before you edit — two branches converging on the same lines cannot both rebase to land.

"
    .to_string();
    for (bid, path) in lines {
        s.push_str(&format!("  {bid}  {path}\n"));
    }
    s.trim_end_matches('\n').to_string()
}

/// `bound_bead_notes <keep> <max>`: fold all but the newest `keep` recurrence notes into one
/// line, then tail-truncate the NOTES section to `max` characters.
pub fn bound_bead_notes(text: &str, keep: usize, max_chars: usize) -> String {
    let notes_re = regex::Regex::new(r"(?m)^NOTES$").unwrap();
    let Some(m) = notes_re.find(text) else { return text.to_string() };
    let start = m.end();
    let tail_re = regex::Regex::new(r"(?m)^(LABELS:|CHILDREN$|BLOCKS$|RELATED$|COMMENTS$)").unwrap();
    let end = tail_re.find(&text[start..]).map(|t| start + t.start()).unwrap_or(text.len());
    let (head, mut notes, tail) = (&text[..start], text[start..end].to_string(), &text[end..]);
    let lines: Vec<&str> = notes.split('\n').collect();
    let marker = regex::Regex::new(r"^\s*Recurrence (\d+) at \S+\.\s*$").unwrap();
    let starts: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| marker.is_match(l)).map(|(i, _)| i).collect();
    if starts.len() > keep {
        let cut = starts.len() - keep;
        let nums: Vec<String> = starts.iter().map(|&i| marker.captures(lines[i]).unwrap()[1].to_string()).collect();
        let preamble = lines[..starts[0]].join("\n");
        let banner = format!("\n  [{cut} earlier recurrence notes omitted — recurrences {}..{}]\n", nums[0], nums[cut - 1]);
        let kept = lines[starts[cut]..].join("\n");
        notes = preamble + &banner + &kept;
    }
    let chars: Vec<char> = notes.chars().collect();
    if chars.len() > max_chars {
        let tailpart: String = chars[chars.len() - max_chars..].iter().collect();
        notes = format!("\n  [notes truncated to the last {max_chars} characters]\n{tailpart}");
    }
    format!("{head}{notes}{tail}")
}

pub fn dirty_brief(dirty: &[String]) -> String {
    if dirty.is_empty() {
        return String::new();
    }
    let list = dirty.iter().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n");
    format!(
        "## Pre-existing dirty files — do not stage these

**These files were already modified or untracked in your worktree when this session started.** They belong to another process (an archivist, an operator, a prior session's scratch work) and must not appear in your commit.

```
{list}
```

**Never use `git add -A` or `git add .`** — they sweep everything and will pick these up. Stage only the paths you yourself wrote:

    git add -- <specific-path>

The pre-commit hook will refuse any commit that stages a pre-session path and name the offenders. Override (only when genuinely necessary):

    SPIRA_ALLOW_DIRTY_STAGE=1 git commit ..."
    )
}

pub fn rebase_brief(branch: &str, base: &str, work: &str, conflicts: &str) -> String {
    let c = if conflicts.is_empty() { "unknown" } else { conflicts };
    format!(
        "## Rebase your branch first

`{branch}` is behind `{base}` and does not rebase onto it cleanly. It will not land until
it does, so this is part of the bead, not a reason to stop:

    git -C {work} rebase {base}

conflicts in: {c}

Resolve every conflict, `git add` each file, `git rebase --continue`, then do the work.
A merge conflict is not an escalation — do not close the bead and do not ask about it.
"
    )
}

pub fn already_done_brief(base: &str, bead_id: &str, work: &str) -> String {
    // The model has no `bd` (sp-st0mm): the record is the supersede *request* `work
    // superseded-by` files, confirmed by the groomer or operator.
    let record = "2. Once confirmed on the base, **request the supersede**:

       work superseded-by <successor-id>

That holds the bead and asks for confirmation; once confirmed, the sentinel, landing pass and
cleanup checks all recognise this bead as retired and skip it correctly. A close reason alone
is not read by any of them.";
    format!(
        "## If you find the work is already done

If you conclude this bead's work has already landed on `{base}` under another commit — a
different bead already carried it — do **not** close with an \"already done\" reason. The
sentinel verifies landing by reading the commit graph, not the close reason: a bare close
without a commit naming `{bead_id}` is indistinguishable from a failed attempt, and the
sentinel reopens it and charges an attempt toward the poison threshold.

The machine-readable path:

1. **Verify the successor actually landed.** Closed is not landed — a bead can be closed
   without its commit on the base. Check the commit graph, not the bead's status:

       git -C {work} log --format='%s' -n ${{SPIRA_VERDICT_WINDOW:-400}} {base} | grep <successor-id>

{record}"
    )
}

/// The builder's whole system prompt: its rune with the three session substitutions and nothing else.
pub fn static_system(template: &str, id: &str, worktree: &str, repo_dir: &str) -> String {
    template.replace("{{ID}}", id).replace("{{WORKTREE}}", worktree).replace("{{REPO_DIR}}", repo_dir)
}

/// The named facts a static-prompt session is told, and only these.
pub struct TaskFacts<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub repo: &'a str,
    pub worktree: &'a str,
    pub branch: &'a str,
    pub landing_ref: &'a str,
    pub deadline: &'a str,
    pub description: &'a str,
    pub notes: &'a str,
    pub history: &'a str,
    pub prior: &'a str,
}

pub fn static_task(f: &TaskFacts) -> String {
    let mut s = format!(
        "# {}: {}\n\nRepository {}, worktree {}, branch {}, landing ref {}.\nDeadline {}\n\n{}\n",
        f.id, f.title, f.repo, f.worktree, f.branch, f.landing_ref, f.deadline, f.description.trim_end_matches('\n')
    );
    if !f.notes.trim().is_empty() {
        s.push_str(&format!("\n## Notes\n{}\n", f.notes.trim_end_matches('\n')));
    }
    if !f.history.trim().is_empty() {
        s.push_str(&format!("\n{}\n", f.history.trim_end_matches('\n')));
    }
    if !f.prior.trim().is_empty() {
        s.push_str(&format!("\n## Prior attempt\n{}\n", f.prior.trim_end_matches('\n')));
    }
    s
}

/// The builder's task prompt: the send-back sections, then the task.
pub fn static_prompt(lead: &str, f: &TaskFacts) -> String {
    format!("{lead}{}", static_task(f))
}

/// The newest `max_chars` characters of the bead's notes.
pub fn tail_notes(notes: &str, max_chars: usize) -> String {
    let chars: Vec<char> = notes.chars().collect();
    if chars.len() <= max_chars {
        return notes.to_string();
    }
    let tail: String = chars[chars.len() - max_chars..].iter().collect();
    format!("[earlier notes omitted]\n{tail}")
}

/// The bead's last fast-tier red, as the first section of the brief; empty when there is none.
pub fn fast_tier_red_brief(digest: &str) -> String {
    let d = digest.trim();
    if d.is_empty() {
        return String::new();
    }
    let repro = if crate::fast_tier::names_lint(d) {
        "Reproduce with the INSTALLED linter, the one that judged you (not `cargo run -p spira-lint`): `spira-lint --diff <base>` from your worktree, before you submit. Resubmitting an unchanged tip is refused."
    } else {
        "Reproduce with `cargo check` before you submit."
    };
    format!("## Your last closeout failed — fix this first\n\n{d}\n\n{repro}\n\n")
}

/// One send-back of a bead: a gate or round `eject-failure`, or a `send-back` fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendBack {
    pub actor: String,
    pub at_epoch: i64,
    pub tip: String,
    pub text: String,
}

impl SendBack {
    pub fn from_fact(f: crate::checkpoint::Fact) -> Option<Self> {
        let (tip, text) = match f.kind.as_str() {
            spira_config::ejection::KIND => (spira_config::ejection::parse(&f.cause).tip, f.cause.trim().to_string()),
            spira_config::send_back::KIND => {
                let (tip, reason) = spira_config::send_back::parse(&f.cause);
                (tip, reason.trim_end().to_string())
            }
            _ => return None,
        };
        Some(Self { actor: f.actor, at_epoch: f.at_epoch, tip, text })
    }
}

/// A bead this one was blocked on, as read at the claim. `state` is `cannot tell (<why>)` when
/// the store gave no answer, so an unreadable blocker is never rendered as absent.
pub struct Dep {
    pub id: String,
    pub state: String,
    pub since: Option<i64>,
    pub commit: Option<String>,
}

pub fn dependencies_brief(deps: &[Dep]) -> String {
    if deps.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Beads this one was blocked on, as of this claim:\n\n");
    for d in deps {
        match d.since {
            Some(at) => out.push_str(&format!("- {} {} since {}", d.id, d.state, crate::util::iso_utc(at))),
            None => out.push_str(&format!("- {} state {}", d.id, d.state)),
        }
        if let Some(c) = d.commit.as_deref().filter(|c| !c.is_empty()) {
            out.push_str(&format!(", {} on the landing ref", &c[..c.len().min(12)]));
        }
        out.push('\n');
    }
    out.push_str("\nA dependency that is LANDED or DONE is an input that changed since any earlier note on this bead.\n\n");
    out
}

/// The claim's stack, told to the builder: the base already merges each tip, and landing order
/// is the queue's to sequence, so there is nothing to ask the operator.
pub fn stacked_brief(stack: &std::collections::BTreeMap<String, String>) -> String {
    if stack.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Stacked on unlanded work\n\n");
    for (prereq, tip) in stack {
        out.push_str(&format!("- {prereq} at {tip}\n"));
    }
    out.push_str("\nYour worktree's base already merges these tips; do not duplicate their changes. Submit when your own work is done: the queue admits this item only after each prerequisite lands, and a later rebase onto the landing ref is the queue's, not a question for the operator.\n\n");
    out
}

const EARLIER_SENDS_MAX: usize = 5;

fn first_line(t: &str) -> &str {
    t.trim().lines().next().unwrap_or("")
}

/// Why the bead was sent back, as the first section of its brief: the newest send-back that
/// judged one of `heads` (the branch at claim, before the rebase, and after it), verbatim,
/// then the older ones. Empty when none judged a head.
pub fn send_back_brief(sends: &[SendBack], amendments: &[spira_config::amendment::Record], heads: &[&str]) -> String {
    let mut sends: Vec<&SendBack> = sends.iter().filter(|s| !s.text.trim().is_empty()).collect();
    sends.sort_by_key(|s| std::cmp::Reverse(s.at_epoch));
    let Some(at) = sends.iter().position(|s| heads.iter().any(|h| !h.is_empty() && *h == s.tip)) else { return String::new() };
    let lead = sends[at];
    let mut out = format!(
        "## Sent back — the reason, verbatim\n\nBy {} at {} on tip {}:\n\n{}\n\nFull output of every step that judged you: `work logs`.\n\nThis is the evidence the sender held; start from it, not from a search of the box.\n\n",
        lead.actor,
        crate::util::iso_utc(lead.at_epoch),
        lead.tip,
        lead.text.trim_end()
    );
    let earlier: Vec<&&SendBack> = sends[at + 1..].iter().take(EARLIER_SENDS_MAX).collect();
    if !earlier.is_empty() {
        out.push_str("### Earlier send-backs\n\n");
        for e in earlier {
            out.push_str(&format!("- {} {} on tip {}: {}\n", e.actor, crate::util::iso_utc(e.at_epoch), e.tip, first_line(&e.text)));
        }
        out.push('\n');
    }
    if amendments.iter().map(|a| a.created).max().is_some_and(|newest| lead.at_epoch > newest) {
        out.push_str("This send-back is newer than every amendment on this bead; it is the task, and the amendments below are history.\n\n");
    }
    out
}

/// The amendments already answered, as history: not instructions. Empty when none was answered.
pub fn amendment_history_brief(amendments: &[spira_config::amendment::Record]) -> String {
    use spira_config::amendment::State;
    let rows: Vec<String> = amendments
        .iter()
        .filter(|a| a.state != State::Pending)
        .map(|a| {
            let state = match &a.state {
                State::Accepted => "accepted".to_string(),
                State::Declined(why) => format!("declined ({why})"),
                State::Enforced => "enforced".to_string(),
                State::Pending => unreachable!(),
            };
            format!("- {} by {}, {}, {state}: {}", a.id, a.amender, crate::util::iso_utc(a.created), first_line(&a.text))
        })
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    format!(
        "## Amendments — history, already answered\n\nThese were answered in an earlier session. An answered amendment is merged into the description under `--- appended <date> ---`, and a \"Live amendment … awaiting acknowledgement\" note above is history once answered: neither is an open instruction.\n\n{}\n",
        rows.join("\n")
    )
}

pub fn thrash_banner(streak: &str, last: &str) -> String {
    let last = if last.is_empty() { "?" } else { last };
    format!(
        "STICKING POINT ({streak} consecutive thrash(es), nothing committed since): {last}
Start there — do not spend a turn rediscovering it from the note history below.
"
    )
}

// ---- the runes and its overlays ---------------------------------------------------

/// The runes brief after the operator's overlays (whole file, sections, append), and the
/// log lines the overlay pass printed. Trailing newlines stripped, as `$(cat …)` did.
pub fn runes_with_overlays(rune: &str, runes_file: &Path, overlay: &Path) -> (String, Vec<String>) {
    let mut logs = Vec::new();
    let whole = overlay.join(format!("{rune}.md"));
    let append = overlay.join(format!("{rune}.append.md"));
    let read = |p: &Path| std::fs::read_to_string(p).map(|s| s.trim_end_matches('\n').to_string());
    if !overlay.as_os_str().is_empty() && whole.is_file() {
        logs.push(format!("brief replaced whole-file by {}", whole.display()));
        return (read(&whole).unwrap_or_default(), logs);
    }
    let mut content = read(runes_file).unwrap_or_default();
    if overlay.as_os_str().is_empty() {
        return (content, logs);
    }
    let prefix = format!("{rune}.");
    let mut sections: Vec<std::path::PathBuf> = std::fs::read_dir(overlay)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    n.starts_with(&prefix) && n.ends_with(".md") && n.len() > prefix.len() + 3 && p.is_file()
                })
                .collect()
        })
        .unwrap_or_default();
    sections.sort();
    for f in sections {
        if f == append {
            continue;
        }
        let n = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let section = &n[prefix.len()..n.len() - 3];
        let heading = format!("## {}", section.replace('_', " "));
        if content.lines().any(|l| l == heading) {
            let repl: String = std::fs::read_to_string(&f).unwrap_or_default().lines().map(|l| format!("{l}\n")).collect();
            content = splice_section(&content, &heading, &repl);
            logs.push(format!("runes section '{heading}' overlaid by {}", f.display()));
        } else {
            logs.push(format!(
                "runes overlay {} names a section ({heading}) absent from {} — ignored",
                f.display(),
                runes_file.display()
            ));
        }
    }
    if append.is_file() {
        let a = read(&append).unwrap_or_default();
        content = format!("{content}\n\n{a}");
    }
    (content, logs)
}

/// dwarf.sh's awk: the heading line becomes `repl`; lines are skipped until the next `## `.
fn splice_section(content: &str, heading: &str, repl: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for line in content.lines() {
        if line == heading {
            out.push_str(repl);
            skip = true;
            continue;
        }
        if skip && line.starts_with("## ") {
            skip = false;
        }
        if skip {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end_matches('\n').to_string()
}

/// `block_overlay <name> <built-in>`: `blocks/<name>.md` verbatim if present.
pub fn block_overlay(overlay: &Path, name: &str, builtin: String) -> (String, Option<String>) {
    let f = overlay.join("blocks").join(format!("{name}.md"));
    if !overlay.as_os_str().is_empty() && f.is_file() {
        let t = std::fs::read_to_string(&f).unwrap_or_default();
        (t.trim_end_matches('\n').to_string(), Some(format!("{name} block overlaid by {}", f.display())))
    } else {
        (builtin.trim_end_matches('\n').to_string(), None)
    }
}

/// Every value the template may name.
#[derive(Debug, Clone, Default)]
pub struct Tokens {
    pub single: Vec<(&'static str, String)>,
    pub bead: String,
    pub park: String,
    pub fixture: String,
    pub deadline: String,
    pub finish: String,
}

/// The substitution, in dwarf.sh's order: the single-line tokens everywhere, then each
/// multi-line block into its FIRST occurrence, in the order BEAD, PARK, FIXTURE, DEADLINE,
/// FINISH; then the thrash banner just after the first `<!-- task -->` (or at the top).
pub fn render_prompt(runes: &str, t: &Tokens, banner: Option<&str>) -> String {
    let mut p = runes.to_string();
    for (k, v) in &t.single {
        p = p.replace(&format!("{{{{{k}}}}}"), v);
    }
    for (k, v) in [("BEAD", &t.bead), ("PARK", &t.park), ("FIXTURE", &t.fixture), ("DEADLINE", &t.deadline), ("FINISH", &t.finish)] {
        p = p.replacen(&format!("{{{{{k}}}}}"), v, 1);
    }
    if let Some(b) = banner.filter(|b| !b.is_empty()) {
        if p.contains("<!-- task -->") {
            p = p.replacen("<!-- task -->", &format!("<!-- task -->\n{b}"), 1);
        } else {
            p = format!("{b}\n{p}");
        }
    }
    p
}

/// `system_prompt_split`: (system.md, task.md). Everything before the first
/// `<!-- task -->` is the persona layer; with no marker the whole prompt is the task.
pub fn split(statutes: &str, prompt: &str) -> (String, String) {
    let (sys, task) = match prompt.split_once("<!-- task -->") {
        Some((s, t)) => (s.to_string(), t.strip_prefix('\n').unwrap_or(t).to_string()),
        None => (String::new(), prompt.to_string()),
    };
    (format!("# Memories in force\n\n{statutes}\n\n---\n\n{sys}"), task)
}

/// Appends this installation's own core slugs (`SPIRA_STATUTE_CORE_LOCAL`) to whatever core a
/// persona already resolved (its `RUNE_STATUTE_CORE`, else `SPIRA_STATUTE_CORE`). The shipped
/// runes and the shipped default both carry only mechanism
/// (law-harness-ships-mechanism-not-inventory); a slug naming this operator's own
/// repositories, labels or preferences lives in `SPIRA_STATUTE_CORE_LOCAL` instead, in config
/// that never ships. A missing local slug refuses exactly like any other missing core slug —
/// the caller runs the merged result through [`missing_core`] same as any other core CSV.
pub fn with_local_core(core_csv: &str, local_csv: &str) -> String {
    match (core_csv.trim().is_empty(), local_csv.trim().is_empty()) {
        (true, true) => String::new(),
        (true, false) => local_csv.trim().to_string(),
        (false, true) => core_csv.trim().to_string(),
        (false, false) => format!("{},{}", core_csv.trim(), local_csv.trim()),
    }
}

/// Declared core slugs that name no memory in the namespace: the persona would start without them.
pub fn missing_core(mem_json: &str, prefixes: &str, core_csv: &str) -> Vec<String> {
    let Ok(serde_json::Value::Object(d)) = serde_json::from_str::<serde_json::Value>(mem_json.trim()) else { return Vec::new() };
    let prefixes: Vec<&str> = prefixes.split(',').filter(|p| !p.is_empty()).collect();
    if !d.keys().any(|k| prefixes.iter().any(|p| k.starts_with(p))) {
        return Vec::new();
    }
    core_csv
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .filter(|k| !(prefixes.iter().any(|p| k.starts_with(p)) && d.get(*k).is_some_and(|v| v.is_string())))
        .map(String::from)
        .collect()
}

/// `render_memories <prefixes> [budget] [core]` over `bd memories --json`'s object.
/// Core statutes in full (within the budget), every other one as a slug in its namespace's
/// index. Trailing newlines stripped, as the `$(...)` that captured it did.
pub fn render_memories(mem_json: &str, prefixes: &str, budget: usize, core_csv: &str, harness: &str) -> String {
    let Ok(serde_json::Value::Object(d)) = serde_json::from_str::<serde_json::Value>(mem_json.trim()) else { return String::new() };
    let prefixes: Vec<&str> = prefixes.split(',').filter(|p| !p.is_empty()).collect();
    let core: std::collections::BTreeSet<&str> = core_csv.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let mem: BTreeMap<&str, String> = d
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s.trim().to_string())))
        .filter(|(k, _)| prefixes.iter().any(|p| k.starts_with(p)))
        .collect();
    let (mut core_out, mut used, mut fallback, mut index) = (Vec::new(), 0usize, Vec::new(), Vec::new());
    for (k, v) in &mem {
        if core.contains(k) {
            let block = format!("## {k}\n\n{v}\n");
            let len = block.chars().count();
            if used + len > budget {
                fallback.push(k.to_string());
            } else {
                core_out.push(block);
                used += len;
            }
        } else {
            index.push(k.to_string());
        }
    }
    index.extend(fallback);
    index.sort();
    let mut parts = Vec::new();
    if !core_out.is_empty() {
        parts.push(core_out.join("\n"));
    }
    if !index.is_empty() {
        let namespaces = [
            (
                "law-",
                format!(
                    "## Statutes in force — full text on request\n\nThese are law and bind you exactly as the text above does. The slug states\nthe rule; read the reasoning and the scar behind any of them with:\n\n    {harness}/rule.sh show <slug-without-law-prefix>\n"
                ),
            ),
            (
                "sop-",
                "## Runbooks on the shelf — full text on request\n\nThese bind exactly as the statutes above do. Read the full runbook —\nCHECK, FIX, ESCALATE — with:\n\n    sop show <slug-without-sop-prefix>\n".to_string(),
            ),
        ];
        for (ns, header) in namespaces {
            let group: Vec<&String> = index.iter().filter(|k| k.starts_with(ns)).collect();
            if !group.is_empty() {
                parts.push(header + &group.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
            }
        }
    }
    parts.join("\n\n").trim_end_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(n: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("dwarf-brief-{n}"))
    }

    // sp-4vq2q, sp-gypjk: the runner is the bare name — never a path relative to home.
    #[test_id(t0ajevq)]
    #[test]
    fn testenv_runner_is_the_bare_name() {
        assert_eq!(TESTENV, "testenv");
    }

    // sp-o4co5: {{FOLLOWUP}} names the `work` verbs, never raw `bd create`.
    #[test_id(taeh8a3)]
    #[test]
    fn followup_names_the_work_verbs() {
        let on = followup_brief("sp-a");
        assert!(on.contains("work file-followup"));
        assert!(on.contains("work split"));
        assert!(!on.contains("bead.sh file"));
    }

    const BUILDER_TASK_GOLDEN: &str = include_str!("../tests/fixtures/builder-task.md");

    fn facts<'a>(notes: &'a str, prior: &'a str) -> TaskFacts<'a> {
        TaskFacts {
            history: "",
            id: "sp-a",
            title: "Do the thing",
            repo: "spira",
            worktree: "w/sp-a",
            branch: "spira/sp-a",
            landing_ref: "local/main",
            deadline: "**This session has no wall-clock deadline.**",
            description: "## Outcome\n\nIt works.\n",
            notes,
            prior,
        }
    }

    #[test_id(ta5uzmd)]
    #[test]
    fn the_builder_system_prompt_is_its_rune_with_three_substitutions() {
        let rune = include_str!("../../spira/runes/builder.md");
        let sys = static_system(rune, "sp-a", "w/sp-a", "r");
        assert_eq!(sys, rune.replace("{{ID}}", "sp-a").replace("{{WORKTREE}}", "w/sp-a"));
        assert!(!sys.contains("{{") && sys.contains("`w/sp-a`") && sys.contains("work item `sp-a`"));
        assert!(sys.starts_with("You are a builder:") && !sys.contains("Warden"));
        assert!(rune.contains("{{ID}}"), "positive control: the template carries its tokens");
    }

    #[test_id(tb5idjr)]
    #[test]
    fn stacked_brief_names_each_prerequisite_its_tip_and_says_to_submit() {
        let b = stacked_brief(&[("sp-a".to_string(), "abc123def".to_string())].into());
        assert!(b.contains("## Stacked on unlanded work") && b.contains("- sp-a at abc123def"), "{b}");
        assert!(b.contains("already merges") && b.contains("do not duplicate") && b.contains("Submit when"), "{b}");
    }

    #[test_id(te6osr1)]
    #[test]
    fn stacked_brief_is_empty_for_an_empty_stack() {
        assert_eq!(stacked_brief(&Default::default()), "");
    }

    #[test_id(t96luy1)]
    #[test]
    fn dependencies_brief_is_empty_without_blockers() {
        assert_eq!(dependencies_brief(&[]), "");
    }

    #[test_id(tlxip87)]
    #[test]
    fn dependencies_brief_names_a_landed_dependency_with_its_time_and_commit() {
        let d = Dep { id: "sp-pvn16v".into(), state: "LANDED".into(), since: Some(1795123820), commit: Some("8ce75d2da5f0123456789".into()) };
        let b = dependencies_brief(&[d]);
        assert!(b.contains("- sp-pvn16v LANDED since 2026-11-19T21:30:20Z, 8ce75d2da5f0 on the landing ref"), "{b}");
        assert!(b.contains("an input that changed"), "{b}");
    }

    #[test_id(tjocv0n)]
    #[test]
    fn dependencies_brief_says_when_a_dependency_cannot_be_read() {
        let d = Dep { id: "sp-x".into(), state: "cannot tell (rc=2: down)".into(), since: None, commit: None };
        let b = dependencies_brief(&[d]);
        assert!(b.contains("- sp-x state cannot tell (rc=2: down)"), "{b}");
    }

    #[test_id(tzgm8ia)]
    #[test]
    fn the_builder_task_prompt_matches_its_golden_fixture() {
        let t = static_task(&facts("first\nsecond", "`spira/sp-a` carries 1 commit(s):\n\n```\n  abc1234 sp-a: x\n```"));
        assert_eq!(t, BUILDER_TASK_GOLDEN);
    }

    #[test_id(trys5jt)]
    #[test]
    fn the_builder_task_prompt_omits_empty_notes_and_prior_sections() {
        let t = static_task(&facts("", " \n"));
        assert!(!t.contains("## Notes") && !t.contains("## Prior attempt"), "{t}");
        assert!(static_task(&facts("n", "")).contains("## Notes\nn\n") && !static_task(&facts("n", "")).contains("Prior attempt"));
    }

    #[test_id(t5awj96)]
    #[test]
    fn tail_notes_keeps_the_newest() {
        assert_eq!(tail_notes("abc", 10), "abc");
        assert_eq!(tail_notes("abcdef", 2), "[earlier notes omitted]\nef");
    }

    fn runner_lines<'a>(p: &'a str, runner: &str) -> Vec<&'a str> {
        p.lines().filter(|l| l.contains(runner)).collect()
    }

    // The batcher brief, too, allows only the reproduce-one-named-suite run, never a pre-close one.
    #[test_id(tl89efc)]
    #[test]
    fn batcher_runes_runs_only_a_named_suite() {
        let runes = include_str!("../../spira/runes/batcher.md");
        let t = Tokens {
            single: vec![
                ("TESTENV", TESTENV.into()),
                ("REPO_NAME", "spira".into()),
            ],
            ..Default::default()
        };
        let p = render_prompt(runes, &t, None);
        assert!(p.contains("    testenv --suites <suite> <branch> spira\n"), "{p}");
        assert!(!p.contains("    testenv <branch> spira\n"), "{p}");
        let lines = runner_lines(&p, "    testenv ");
        assert!(!lines.is_empty() && lines.iter().all(|l| l.contains(" --suites <suite> ")));
        assert!(!p.contains("exactly what the landing"));
    }

    // {{FINISH}} is the `work` verbs (sp-wmcvb).
    #[test_id(tef2apb)]
    #[test]
    fn finish_is_the_work_verbs() {
        let on = finish_brief("sp-a");
        assert!(on.contains("**You have no `bd`.**"));
        assert!(on.contains("this one: `sp-a`"));
        assert!(on.contains("    work submit\n"));
        assert!(!on.contains("bd -C"));
    }

    #[test_id(thxeezy)]
    #[test]
    fn landing_and_park_by_mode() {
        assert!(landing_brief("pr", "spira/sp-a", "main", "svc", "origin/main").contains("opens a pull request against `main`; svc's own CI"));
        assert!(landing_brief("hold", "b", "main", "svc", "o/m").starts_with("Spira does not advance svc's `main`"));
        assert!(landing_brief("queue.local", "b", "main", "svc", "local/main").ends_with("between your commit and `local/main`"));
        let pr = park_brief("pr", "spira/sp-a", "svc", "main");
        assert!(pr.contains("`landing-pass` pushes `spira/sp-a`"));
        assert!(pr.contains("\n$REPO_NAME's own CI runs"), "dwarf.sh escaped $REPO_NAME in the pr text; kept");
        let push = park_brief("push", "spira/sp-a", "svc", "main");
        assert!(push.contains("svc lands by `push`"));
        assert!(push.ends_with("none of it needs you."));
    }

    #[test_id(t5xjfvn)]
    #[test]
    fn fixture_brief_is_truthful_about_what_exists() {
        let none = fixture_brief(None, "spira/testdb.sh", 0);
        assert!(none.starts_with("This repository has no shared test fixture"));
        let f = FixtureInfo { name: "dwarvespa".into(), dir: "d".into(), baseline: "b".into(), bin: String::new(), started_service: "0".into(), mode: "embedded".into(), server_init_hash: String::new() };
        let e = fixture_brief(Some(&f), "spira/testdb.sh", 1234);
        assert!(e.contains("`TESTDB_NAME=dwarvespa`"));
        assert!(e.contains("spending the 1234ms"));
        assert!(e.contains("embedded Dolt engine"));
        let s = fixture_brief(Some(&FixtureInfo { mode: "server".into(), ..f }), "spira/testdb.sh", 1);
        assert!(s.contains("private Dolt sql-server"));
        for text in [&none, &e, &s] {
            assert!(!text.contains("dolt-beads-test"), "no shared test server in the brief");
        }
    }

    // covers: UC-dwarf-execution-07
    #[test_id(tdd1dsm)]
    #[test]
    fn resume_slain_deadline_renderers() {
        assert_eq!(resume_brief("b", "w", "0", "x"), "");
        assert_eq!(resume_brief("b", "w", "?", "x"), "");
        let r = resume_brief("spira/sp-a", "w", "2", "  abc one\n  def two");
        assert!(r.starts_with("## Prior work on this branch\n\n`spira/sp-a` carries **2** commit(s)"));
        assert!(r.contains("```\n  abc one\n  def two\n```"));
        assert_eq!(slain_brief("sp-a — the work", "1", "origin/main", "", "", "l"), "");
        let s = slain_brief("sp-a: wip — salvaged at slay (operator stop)", "3", "origin/main", "2026-09-28 10:00:00 +0000", "", "run/sp-a.log");
        assert!(s.contains("slain at 2026-09-28 10:00:00 +0000 (operator stop)"));
        assert!(s.contains("beyond `origin/main`; the last"), "an empty diffstat adds no stray parentheses");
        let s2 = slain_brief("sp-a: wip — salvaged at slay (x)", "3", "o/m", "", " 2 files changed", "l");
        assert!(s2.contains("slain at unknown time (x)") && s2.contains("`o/m` ( 2 files changed);"));
        let dl = deadline_brief(Some(1_000_600), 1_000_000, Some("10:00:00 UTC".into()));
        assert!(dl.starts_with("**This session is killed at 10:00:00 UTC — 600 seconds from now.**"));
        assert!(dl.ends_with("    echo $(( 1000600 - $(date +%s) ))"));
        assert!(deadline_brief(Some(5), 0, None).contains("killed at epoch 5 —"));
        assert!(deadline_brief(None, 0, None).starts_with("**This session has no wall-clock deadline.**"));
    }

    // covers: UC-dwarf-execution-07
    #[test_id(t2bnoju)]
    #[test]
    fn a_prompt_rendered_from_every_brief_leaves_no_placeholder_for_the_model() {
        let t = Tokens {
            single: vec![("BEAD_ID", "sp-a".into()), ("BRANCH", "spira/sp-a".into())],
            bead: "the bead".into(),
            park: park_brief("local/main", "spira/sp-a", "spira", "main"),
            fixture: String::new(),
            deadline: deadline_brief(Some(1_000_600), 1_000_000, None),
            finish: format!(
                "{}\n{}\n{}",
                resume_brief("spira/sp-a", "w", "2", "abc one"),
                slain_brief("sp-a: wip — salvaged at slay (stop)", "3", "origin/main", "", "", "l"),
                already_done_brief("origin/main", "sp-a", "w")
            ),
        };
        let p = render_prompt("work {{BEAD_ID}} on {{BRANCH}}\n{{BEAD}}\n{{PARK}}\n{{FIXTURE}}{{DEADLINE}}\n{{FINISH}}", &t, None);
        assert!(!p.contains("{{"), "{p}");
        for part in ["Prior work on this branch", "slain at", "killed at epoch 1000600", "work superseded-by"] {
            assert!(p.contains(part), "{part} missing from {p}");
        }
    }

    #[test_id(t0d8wj3)]
    #[test]
    fn the_ops_brief_renders_with_every_placeholder_filled_and_offers_no_suites_tool() {
        let mut single = tool_tokens();
        single.extend([
            ("BEAD_ID", "sp-a".to_string()),
            ("BRANCH", "spira/sp-a".to_string()),
            ("REPO", "w/sp-a".to_string()),
            ("NO_BD", no_bd_brief()),
        ]);
        let t = Tokens { single, bead: "the bead".into(), park: "park".into(), deadline: "deadline".into(), ..Default::default() };
        let p = render_prompt(include_str!("../../spira/runes/ops.md"), &t, None);
        assert!(!p.contains("{{"), "{p}");
        assert!(!p.contains("testenv suites"), "{p}");
    }

    #[test_id(t0hmxw8)]
    #[test]
    fn a_call_on_an_old_incident_gets_a_positive_wall_from_its_own_start() {
        let incident_first_seen = 1_000_000 - 6 * 86_400;
        let call_start = 1_000_000;
        let at = call_deadline(Some(480), call_start);
        assert_eq!(at, Some(call_start + 480), "wall anchors to the call, not to {incident_first_seen}");
        assert_eq!(spent_wall(at, call_start), None);
        assert!(spent_wall(call_deadline(Some(480), incident_first_seen), call_start).unwrap().contains("already spent"));
        assert!(spent_wall(call_deadline(Some(0), call_start), call_start).is_some());
        assert_eq!(spent_wall(None, call_start), None);
    }

    fn send(actor: &str, at: i64, tip: &str, text: &str) -> SendBack {
        SendBack { actor: actor.into(), at_epoch: at, tip: tip.into(), text: text.into() }
    }

    fn amendment(id: &str, created: i64, state: spira_config::amendment::State) -> spira_config::amendment::Record {
        spira_config::amendment::Record { id: id.into(), bead: "sp-a".into(), amender: "operator".into(), text: "reword the commit\nmore".into(), created, deadline: created + 600, state }
    }

    #[test_id(tecc0yu)]
    #[test]
    fn an_eject_brief_leads_with_the_test_location_and_output_and_is_empty_without_one() {
        let d = spira_config::ejection::digest("aaa", &[], "---- t::boom stdout ----\nthread 't::boom' panicked at x/y.rs:7:1:\nit broke\n\nfailures:\n");
        let one = |tip: &str| vec![send("batcher", 100, &spira_config::ejection::parse(tip).tip, tip)];
        let b = send_back_brief(&one(&d), &[], &["aaa"]);
        assert!(b.starts_with("## Sent back") && b.contains("t::boom at x/y.rs:7") && b.contains("it broke"), "{b}");
        assert!(b.contains("`work logs`"), "the brief names the reader of the full output: {b}");
        assert_eq!(send_back_brief(&one(&d), &[], &["bbb"]), "", "a red that judged another tip says nothing about this one");
        assert!(send_back_brief(&one(&d), &[], &["aaa", "bbb"]).starts_with("## Sent back"), "judged before the rebase");
        assert_eq!(send_back_brief(&one(&d), &[], &["ccc", "bbb"]), "");
        assert_eq!(send_back_brief(&one(&d), &[], &["", "bbb"]), "");
        assert_eq!(send_back_brief(&[send("batcher", 1, "aaa", "  \n")], &[], &["aaa", "bbb"]), "");
    }

    #[test_id(tdjf7m5)]
    #[test]
    fn a_rework_after_an_accepted_amendment_leads_with_its_reason_and_demotes_the_amendment() {
        let reason = "run both suites:\n- test-spike.sh: 'not ok 2 - a spike bead is in the spike's partition / wanted [1] got [0]'\n- test-sim.sh: 'not ok 1 - x exits 0 in 33s'";
        let am = [amendment("am-1", 1_000, spira_config::amendment::State::Accepted)];
        let b = send_back_brief(&[send("concierge", 2_000, "aaa", reason)], &am, &["aaa"]);
        assert!(b.starts_with("## Sent back — the reason, verbatim"), "{b}");
        assert!(b.contains(reason) && b.contains("concierge") && b.contains(&crate::util::iso_utc(2_000)) && b.contains("aaa"), "{b}");
        assert!(b.contains("newer than every amendment on this bead; it is the task, and the amendments below are history"), "{b}");
        let h = amendment_history_brief(&am);
        assert!(h.starts_with("## Amendments — history, already answered") && h.contains("am-1") && h.contains("accepted") && h.contains("reword the commit") && !h.contains("more"), "{h}");
        let p = static_prompt(&b, &TaskFacts { history: &h, ..facts("n", "") });
        assert!(p.starts_with("## Sent back — the reason, verbatim"), "{p}");
        assert!(p.find("## Notes").unwrap() < p.find("## Amendments — history").unwrap(), "{p}");
        let amendments_at = p.find("am-1").unwrap();
        assert!(amendments_at > p.find(reason).unwrap(), "the amendment is read after the send-back");
        assert!(!send_back_brief(&[send("concierge", 500, "aaa", reason)], &am, &["aaa"]).contains("newer than every amendment"));
    }

    #[test_id(tmfuuqb)]
    #[test]
    fn every_sender_renders_through_one_section_newest_first() {
        let d = spira_config::ejection::digest("aaa", &[], "---- t::boom stdout ----\nboom\n\nfailures:\n");
        let sends = [
            send("batcher", 100, "aaa", &d),
            send("operator", 300, "aaa", "operator says no\nsecond line"),
            send("sift", 200, "aaa", "Sift sent this back\n- [no-rebase] x"),
        ];
        let b = send_back_brief(&sends, &[], &["aaa"]);
        assert!(b.contains("By operator") && b.contains("operator says no\nsecond line"), "{b}");
        let earlier = b.split("### Earlier send-backs").nth(1).unwrap();
        assert!(earlier.find("sift").unwrap() < earlier.find("batcher").unwrap(), "{earlier}");
        assert!(!earlier.contains("second line") && earlier.contains("Sift sent this back"), "{earlier}");
        assert!(!b.contains("newer than every amendment"));
        let other = [send("operator", 300, "zzz", "elsewhere"), send("sift", 200, "aaa", "mine")];
        let b = send_back_brief(&other, &[], &["aaa"]);
        assert!(b.contains("By sift") && !b.contains("elsewhere"), "a red on another tip is not the lead: {b}");
    }

    #[test_id(twt4grz)]
    #[test]
    fn pending_amendments_are_not_history() {
        let am = [amendment("am-p", 1, spira_config::amendment::State::Pending)];
        assert_eq!(amendment_history_brief(&am), "");
        assert_eq!(amendment_history_brief(&[]), "");
        let d = amendment_history_brief(&[amendment("am-d", 1, spira_config::amendment::State::Declined("no".into())), am[0].clone()]);
        assert!(d.contains("am-d") && d.contains("declined (no)") && !d.contains("am-p"), "{d}");
    }

    #[test_id(tn2omia)]
    #[test]
    fn holds_brief_rows() {
        assert!(holds_brief("sp-a", 2, "").contains("COULD NOT CHECK"));
        assert_eq!(holds_brief("sp-a", 0, "sp-a\tf.rs\n"), "", "a bead's own branch is not another holder");
        let h = holds_brief("sp-a", 0, "sp-b\tsrc/x.rs\nsp-a\tsrc/x.rs\n");
        assert!(h.ends_with("\n\n  sp-b  src/x.rs"), "{h}");
    }

    // test-brief-notes.sh T6: BEAD_BODY is passed through bound_bead_notes.
    #[test_id(treboyn)]
    #[test]
    fn bead_body_is_bounded() {
        let mut body = String::from("sp-a · incident\n\nNOTES\n");
        for i in 1..=8 {
            body.push_str(&format!("Recurrence {i} at 2026-09-2{i}T00:00:00Z.\nsomething\n"));
        }
        body.push_str("LABELS: spira\n");
        let b = bound_bead_notes(&body, 5, 8000);
        assert!(b.contains("[3 earlier recurrence notes omitted — recurrences 1..3]"));
        assert!(!b.contains("Recurrence 3 at") && b.contains("Recurrence 4 at") && b.ends_with("LABELS: spira\n"));
        let plain = "sp-a\nNOTES\n".to_string() + &"n".repeat(50);
        let t = bound_bead_notes(&plain, 5, 10);
        assert!(t.contains("[notes truncated to the last 10 characters]\nnnnnnnnnnn"));
        assert_eq!(bound_bead_notes("no notes here", 5, 1), "no notes here");
    }

    #[test_id(t2jiyww)]
    #[test]
    fn overlays_whole_section_append_and_absent() {
        let d = tmpdir("ov");
        let runes = d.join("builder.md");
        std::fs::write(&runes, "# Builder\n## Tests\nrelease tests\nmore\n## Finishing\n{{FINISH}}\n\n").unwrap();
        let ov = d.join("overlay");
        std::fs::create_dir_all(ov.join("blocks")).unwrap();
        let (c, logs) = runes_with_overlays("builder", &runes, &ov);
        assert_eq!(c, "# Builder\n## Tests\nrelease tests\nmore\n## Finishing\n{{FINISH}}");
        assert!(logs.is_empty());
        std::fs::write(ov.join("builder.Tests.md"), "## Tests\nmine\n").unwrap();
        std::fs::write(ov.join("builder.No_Such.md"), "## No Such\nx\n").unwrap();
        std::fs::write(ov.join("builder.append.md"), "APPENDED\n").unwrap();
        let (c, logs) = runes_with_overlays("builder", &runes, &ov);
        assert_eq!(c, "# Builder\n## Tests\nmine\n## Finishing\n{{FINISH}}\n\nAPPENDED");
        assert!(logs.iter().any(|l| l.contains("names a section (## No Such) absent from") && l.ends_with("— ignored")));
        assert!(logs.iter().any(|l| l.starts_with("runes section '## Tests' overlaid by")));
        std::fs::write(ov.join("builder.md"), "WHOLE\n").unwrap();
        let (c, logs) = runes_with_overlays("builder", &runes, &ov);
        assert_eq!(c, "WHOLE");
        assert!(logs[0].starts_with("brief replaced whole-file by"));
        std::fs::write(ov.join("blocks/PARK.md"), "my park\n").unwrap();
        let (p, l) = block_overlay(&ov, "PARK", "builtin".into());
        assert_eq!((p.as_str(), l.is_some()), ("my park", true));
        let (f, l) = block_overlay(&ov, "FINISH", "builtin\n".into());
        assert_eq!((f.as_str(), l), ("builtin", None));
    }

    #[test_id(tsuuesg)]
    #[test]
    fn substitution_is_literal_and_ordered() {
        let t = Tokens {
            single: vec![("BEAD_ID", "sp-a".into()), ("DB", "d&b|x\\y".into())],
            bead: "body with {{PARK}} & more".into(),
            park: "PARK!".into(),
            fixture: "FX".into(),
            deadline: "DL".into(),
            finish: "FIN".into(),
        };
        let p = render_prompt("id {{BEAD_ID}} db {{DB}}\n{{BEAD}}\n{{PARK}}\n{{FIXTURE}}{{FIXTURE}}\n{{DEADLINE}}{{FINISH}}{{UNKNOWN}}", &t, None);
        // BEAD first, so a {{PARK}} inside the bead body is the first occurrence (as before).
        assert_eq!(p, "id sp-a db d&b|x\\y\nbody with PARK! & more\n{{PARK}}\nFX{{FIXTURE}}\nDLFIN{{UNKNOWN}}");
    }

    #[test_id(t7jvmzx)]
    #[test]
    fn the_shipped_groomer_splits_operations_into_system_and_finishing_into_task() {
        let (sys, task) = split("LAW", &render_prompt(include_str!("../../spira/runes/groomer.md"), &Tokens::default(), None));
        assert!(sys.contains("Four operations, one refusal") && sys.contains("MUST NOT do"));
        assert!(task.contains("finish the trigger bead"));
        assert!(!task.contains("Memories in force") && !task.contains("Four operations, one refusal"));
    }

    #[test_id(tbsc0hu)]
    #[test]
    fn thrash_banner_lands_in_the_task_half() {
        let b = thrash_banner("2", "Bash: cargo test");
        let p = render_prompt("persona\n<!-- task -->\n## The bead\nx", &Tokens::default(), Some(&b));
        let (sys, task) = split("S", &p);
        assert!(!sys.contains("STICKING POINT"));
        assert!(task.starts_with("STICKING POINT (2 consecutive thrash(es), nothing committed since): Bash: cargo test\n"));
        assert!(task.find("STICKING POINT").unwrap() < task.find("## The bead").unwrap());
        let p2 = render_prompt("no marker", &Tokens::default(), Some(&b));
        assert!(p2.starts_with("STICKING POINT") && p2.ends_with("\n\nno marker"));
    }

    #[test_id(tac339s)]
    #[test]
    fn split_layers() {
        let (s, t) = split("LAW", "persona\n<!-- task -->\ntask body\n<!-- task -->\nmore");
        assert_eq!(s, "# Memories in force\n\nLAW\n\n---\n\npersona\n");
        assert_eq!(t, "task body\n<!-- task -->\nmore");
        let (s, t) = split("LAW", "everything");
        assert_eq!(s, "# Memories in force\n\nLAW\n\n---\n\n");
        assert_eq!(t, "everything");
    }

    #[test_id(tf0cieh)]
    #[test]
    fn memories_core_and_index() {
        let j = r#"{"law-b":" B text ","law-a":"A text","sop-x":"X","other":"no","law-c":7}"#;
        let m = render_memories(j, "law-,sop-", 120000, "law-a", "h");
        assert!(m.starts_with("## law-a\n\nA text\n\n\n## Statutes in force"), "{m}");
        assert!(m.contains("    h/rule.sh show <slug-without-law-prefix>\nlaw-b"));
        assert!(m.ends_with("    sop show <slug-without-sop-prefix>\nsop-x"));
        assert!(!m.contains("other"));
        let tight = render_memories(j, "law-", 5, "law-a", "h");
        assert!(tight.starts_with("## Statutes in force") && tight.contains("law-a\nlaw-b"), "over budget falls back to the index");
        assert_eq!(render_memories("garbage", "law-", 1, "", "h"), "");
    }

    #[test_id(trrlgls)]
    #[test]
    fn missing_core_names_unresolved_slugs() {
        let j = r#"{"law-a":"A","sop-x":"X"}"#;
        assert_eq!(missing_core(j, "law-", "law-a, law-gone"), vec!["law-gone"]);
        assert_eq!(missing_core(j, "law-", "sop-x"), vec!["sop-x"], "outside the prefixes it never renders");
        assert!(missing_core(j, "law-", "law-a").is_empty() && missing_core(j, "law-", "").is_empty());
        assert!(missing_core("garbage", "law-", "law-a").is_empty());
        assert!(missing_core("{}", "law-", "law-a").is_empty(), "a namespace with no memories has nothing to have renamed");
        assert!(missing_core(r#"{"sop-x":"X"}"#, "law-", "law-a").is_empty());
    }

    #[test_id(tn9dexe)]
    #[test]
    fn with_local_core_appends_and_handles_empties() {
        assert_eq!(with_local_core("", ""), "");
        assert_eq!(with_local_core("law-a", ""), "law-a");
        assert_eq!(with_local_core("", "law-local"), "law-local");
        assert_eq!(with_local_core("law-a,law-b", "law-local"), "law-a,law-b,law-local");
        assert_eq!(with_local_core(" law-a ", " law-local "), "law-a,law-local", "trims each side");
    }

    #[test_id(tr8f0c9)]
    #[test]
    fn local_core_slug_renders_in_full_and_a_missing_one_refuses() {
        let j = r#"{"law-a":"A text","law-local":"Local text"}"#;
        let merged = with_local_core("law-a", "law-local");
        assert!(missing_core(j, "law-", &merged).is_empty(), "both slugs resolve");
        let rendered = render_memories(j, "law-", 120000, &merged, "h");
        assert!(rendered.contains("## law-local\n\nLocal text"), "{rendered}");

        let merged_missing = with_local_core("law-a", "law-gone-local");
        assert_eq!(
            missing_core(j, "law-", &merged_missing),
            vec!["law-gone-local"],
            "a SPIRA_STATUTE_CORE_LOCAL slug the database lacks refuses exactly like any other missing core slug"
        );
    }

    // covers: UC-dwarf-execution-07
    #[test_id(t02oipo)]
    #[test]
    fn close_and_already_done_blocks() {
        let composed = format!("{}{}", already_done_brief("origin/main", "sp-a", "w"), rebase_brief("b", "o/m", "w", ""));
        assert!(!composed.contains("Before you close: rebase"), "{composed}");
        let on = already_done_brief("origin/main", "sp-a", "w");
        assert!(on.contains("-n ${SPIRA_VERDICT_WINDOW:-400} origin/main | grep"));
        assert!(on.contains("work superseded-by <successor-id>") && !on.contains("bd "), "{on}");
        assert!(rebase_brief("b", "o/m", "w", "").contains("conflicts in: unknown"));
        assert!(dirty_brief(&["a".into(), "b c".into()]).contains("```\n  a\n  b c\n```"));
        assert_eq!(dirty_brief(&[]), "");
    }

    #[test_id(tinw4ms)]
    #[test]
    fn the_tool_placeholders_are_work_verbs() {
        for (k, v) in tool_tokens() {
            assert!(v.starts_with("work "), "{{{{{k}}}}} is {v:?}");
        }
    }

    #[test_id(tjhx9y2)]
    #[test]
    fn the_bd_detector_sees_a_raw_command_and_spares_prose() {
        let raw_bd = regex::Regex::new(r"(?m)(^|[^A-Za-z0-9_./-])bd\s+(-C\b|[a-z][a-z-]*)").unwrap();
        assert!(raw_bd.is_match("    bd -C /db note sp-a \"x\""));
        assert!(raw_bd.is_match("run `bd show <id>` first"));
        assert!(!raw_bd.is_match("that one asked `bd` a question"));
        assert!(!raw_bd.is_match("work note \"x\""));
    }
}
