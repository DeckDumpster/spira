//! The brief (DESIGN.md §1.4): the chamber template, the operator's overlays, the
//! session-specific blocks and the split into system.md / task.md.
//!
//! Pure: every input is in the arguments. Nothing here claims, selects or reads the store —
//! claim selection lives in claim.rs and never touches this module (sp-f0qhr). What the
//! session is told about finishing is `lifecycle_enforce`'s alone (sp-wmcvb).
//!
//! Texts are aeon.sh's and lib.sh's, verbatim. Substitution is literal (DESIGN.md §8.1).

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

/// `{{TESTENV}}`: the testenv runner, ABSOLUTE. `SPIRA_TESTENV_BIN` as the aeon resolved it
/// wins; without one, the harness's `bin/testenv` beside `SPIRA_HOME`, joined lexically so
/// no `..` survives. Never a path relative to the aeon's worktree, which has no `bin/` — an
/// aeon told `{{SPIRA_HOME}}/../bin/testenv` ran `./spira/testenv.sh`, the container helper,
/// instead (sp-4vq2q).
pub fn testenv_runner(testenv_bin: &str, home: &str) -> String {
    if !testenv_bin.is_empty() {
        return testenv_bin.to_string();
    }
    let h = Path::new(home);
    h.parent().unwrap_or(h).join("bin").join("testenv").display().to_string()
}

/// `{{FOLLOWUP}}`: how to file work discovered rather than done, selected by
/// `lifecycle_enforce` exactly as `{{FINISH}}` is. With it OFF every `work` verb refuses
/// (exit 3), and an aeon that fell back to raw `bd create --parent` gave all six children
/// the parent's `branch:` label (sp-o4co5).
pub fn followup_brief(lifecycle_enforce: bool, home: &str, bead_id: &str, repo_name: &str) -> String {
    if lifecycle_enforce {
        format!(
            "**File it as a bead** — `work file-followup \"<title>\"` for work that follows from
  this one, `work split \"<title>\"` for a piece of this bead's own scope. Both are parented
  to `{bead_id}` for you and never carry its `branch:` label."
        )
    } else {
        format!(
            "**File it as a bead** through the contract, never with raw `bd create`:

      {home}/bead.sh file \"<title>\" --for <persona> --repo {repo_name} [--priority N] [--body-file F] [--parent {bead_id}]

  `--for` names the persona that should claim it (`{home}/bead.sh contract` lists the legal
  personas, repos and types). A follow-up must never carry this bead's `branch:` label —
  `branch:` names one bead's own worktree, and raw `bd create --parent` copies it onto every
  child. `bead.sh file` does not. (The `work` verbs refuse while lifecycle enforcement is
  off; do not use them.)"
        )
    }
}

/// `{{FINISH}}`: selected by `lifecycle_enforce` and nothing else.
pub fn finish_brief(lifecycle_enforce: bool, bead_id: &str, db: &str) -> String {
    if lifecycle_enforce {
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

    work blocked \"<the question>\" --default \"<what you would do by default>\"

This both holds the bead and files the ask; you do not also send mail yourself.

When you discover other work rather than doing it:

    work file-followup \"<title>\"
    work split \"<title>\"

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
    } else {
        format!(
            "When the work is committed on your branch, close the bead with evidence. The first line of
the reason must declare the terminal outcome:

    bd -C {db} close {bead_id} --reason-file - <<'REASON'
    OUTCOME: submitted
    <what landed, and how it was verified>
    REASON

The seven valid outcomes, and when each applies:

| Outcome     | When to use |
|-------------|-------------|
| `submitted` | Work committed on your branch; the landing pass carries it from here |
| `delivered` | Deliverable is not code — beads, a note, a document; name what you wrote |
| `escalated` | Blocked on an operator decision; name the ask bead (which must list this bead as a dependent) |
| `blocked`   | Blocked on another bead; name it |
| `abandoned` | Bead should not be done; explain why |
| `parked`    | Out of lifetime; name what remains |
| `landed`    | Work is already on the base branch (sentinel's record) |

`submitted` is the standard outcome for a builder. Use `delivered` when the work is
child beads, a mail message, or a document rather than a code commit.

`--reason-file -`, never `--reason -`. `bd close` does not read stdin for `--reason`: it
stores the literal string `-`, prints a success line and exits 0, so a close whose whole
value is its evidence silently becomes a dash. Prose belongs on stdin anyway — backticks
and `$( )` inside a double-quoted argument are command substitution
(`law-commit-messages-via-stdin`).

**A bead whose deliverable is child beads closes with `--force`.** From bd v1.2.1 a close is
refused while the bead has open children — *\"cannot close X: 1 open child issue(s); close
children first or use --force to override\"*. When you filed those children deliberately and
said so with `delivers:beads`, that refusal is aimed at the wrong thing: the children ARE the
work, and closing them first would be a lie. Pass `--force` in that case and only that case —
if you did not declare `delivers:beads`, an open child means you are not finished. A close
that fails leaves the bead `in_progress`, so the verdict finds no commit and reopens it, and
the attempt counts toward poisoning the bead.

If you cannot finish — the bead is ambiguous, needs a credential, or needs a decision that
is the operator's to make — do **not** close it. Leave it open, add a note saying precisely what is
blocked and what you would do by default, and exit non-zero:

    bd -C {db} note {bead_id} \"BLOCKED: <what is blocked>. Default: <what you would do>.\"

An honest failure is cheap. A bead closed without its work landing is expensive, because
everything downstream of it unblocks on a lie."
        )
    }
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

pub fn fixture_brief(fixture: Option<&FixtureInfo>, testdb_lib: &str, port: &str, fixture_ms: u64) -> String {
    match fixture {
        Some(f) => {
            let (engine, cleanup) = if f.mode == "server" {
                (
                    format!("the dolt-beads-test server (port {})", if port.is_empty() { "3308" } else { port }),
                    "testdb_drop (which stops dolt-beads-test.service if this session started it)".to_string(),
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

pub fn already_done_brief(base: &str, bead_id: &str, work: &str, db: &str) -> String {
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

2. Once confirmed on the base, **run `bd supersede`**:

       bd -C {db} supersede {bead_id} --with <successor-id>

That records the relation so the sentinel, landing pass, and cleanup checks all recognise this
bead as retired and skip it correctly. A close reason alone is not read by any of them."
    )
}

pub fn close_brief(base: &str, work: &str, base_remote: &str) -> String {
    let fetch = if base_remote.is_empty() {
        format!("# {base} is a local ref already in this checkout; no fetch needed")
    } else {
        format!("git -C {work} fetch {base_remote}")
    };
    format!(
        "## Before you close: rebase onto `{base}`

Other aeons land while you work, so `{base}` has probably moved. The last thing you do
before closing the bead — after your commits, before the close — is:

    {fetch}
    git -C {work} rebase {base}

Resolve any conflict yourself: you wrote these commits and you know what they mean, and the
landing pass does not — it would reopen the bead and hand the conflict to a stranger. Then
run the gate once more on the rebased tree, and close. A bead closed behind `{base}` that
does not rebase cleanly is reopened by the harness, which costs a whole second session."
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

// ---- the chamber and its overlays ---------------------------------------------------

/// The chamber brief after the operator's overlays (whole file, sections, append), and the
/// log lines the overlay pass printed. Trailing newlines stripped, as `$(cat …)` did.
pub fn chamber_with_overlays(fayth: &str, chamber_file: &Path, overlay: &Path) -> (String, Vec<String>) {
    let mut logs = Vec::new();
    let whole = overlay.join(format!("{fayth}.md"));
    let append = overlay.join(format!("{fayth}.append.md"));
    let read = |p: &Path| std::fs::read_to_string(p).map(|s| s.trim_end_matches('\n').to_string());
    if !overlay.as_os_str().is_empty() && whole.is_file() {
        logs.push(format!("brief replaced whole-file by {}", whole.display()));
        return (read(&whole).unwrap_or_default(), logs);
    }
    let mut content = read(chamber_file).unwrap_or_default();
    if overlay.as_os_str().is_empty() {
        return (content, logs);
    }
    let prefix = format!("{fayth}.");
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
            logs.push(format!("chamber section '{heading}' overlaid by {}", f.display()));
        } else {
            logs.push(format!(
                "chamber overlay {} names a section ({heading}) absent from {} — ignored",
                f.display(),
                chamber_file.display()
            ));
        }
    }
    if append.is_file() {
        let a = read(&append).unwrap_or_default();
        content = format!("{content}\n\n{a}");
    }
    (content, logs)
}

/// aeon.sh's awk: the heading line becomes `repl`; lines are skipped until the next `## `.
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

/// The substitution, in aeon.sh's order: the single-line tokens everywhere, then each
/// multi-line block into its FIRST occurrence, in the order BEAD, PARK, FIXTURE, DEADLINE,
/// FINISH; then the thrash banner just after the first `<!-- task -->` (or at the top).
pub fn render_prompt(chamber: &str, t: &Tokens, banner: Option<&str>) -> String {
    let mut p = chamber.to_string();
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
                format!(
                    "## Runbooks on the shelf — full text on request\n\nThese bind exactly as the statutes above do. Read the full runbook —\nCHECK, FIX, ESCALATE — with:\n\n    {harness}/spira/sop.sh show <slug-without-sop-prefix>\n"
                ),
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
        testkit::TempDir::new(&format!("aeon-brief-{n}"))
    }

    // sp-4vq2q: the runner is absolute — the resolved bin, else bin/testenv beside SPIRA_HOME.
    #[test]
    fn testenv_runner_is_absolute_never_relative_to_home() {
        assert_eq!(testenv_runner("/opt/art/testenv", "/srv/harness/spira"), "/opt/art/testenv");
        let r = testenv_runner("", "/srv/harness/spira");
        assert_eq!(r, "/srv/harness/bin/testenv");
        assert!(!r.contains(".."));
    }

    // sp-o4co5: {{FOLLOWUP}} follows lifecycle_enforce exactly as {{FINISH}} does.
    #[test]
    fn followup_follows_lifecycle_enforce() {
        let off = followup_brief(false, "/h/spira", "sp-a", "spira");
        assert!(off.contains("/h/spira/bead.sh file \"<title>\" --for <persona> --repo spira"));
        assert!(off.contains("never with raw `bd create`"));
        assert!(off.contains("`branch:` label"));
        assert!(!off.contains("work file-followup"));
        let on = followup_brief(true, "/h/spira", "sp-a", "spira");
        assert!(on.contains("work file-followup"));
        assert!(on.contains("work split"));
        assert!(!on.contains("bead.sh file"));
    }

    // The shipped builder chamber renders both tokens, and leaves no relative runner path.
    #[test]
    fn builder_chamber_renders_runner_and_followup() {
        let chamber = include_str!("../../spira/chamber/builder.md");
        for enforce in [false, true] {
            let t = Tokens {
                single: vec![
                    ("TESTENV", testenv_runner("", "/srv/h/spira")),
                    ("FOLLOWUP", followup_brief(enforce, "/srv/h/spira", "sp-a", "spira")),
                    ("BRANCH", "spira/sp-a".into()),
                    ("REPO_NAME", "spira".into()),
                    ("SPIRA_HOME", "/srv/h/spira".into()),
                ],
                ..Default::default()
            };
            let p = render_prompt(chamber, &t, None);
            // The only runner line is the reproduce-one-named-suite form; a no-`--suites`
            // pre-close run duplicates the gate's work outside its admission (sp-4vq2q follow-up).
            assert!(p.contains("    /srv/h/bin/testenv --suites <suite> spira/sp-a spira\n"), "{p}");
            assert!(!p.contains("    /srv/h/bin/testenv spira/sp-a spira\n"), "{p}");
            assert!(runner_lines(&p, "/srv/h/bin/testenv").iter().all(|l| l.contains(" --suites <suite> ")));
            assert!(p.contains("do not run the suites"));
            assert!(!p.contains("exactly the selection"));
            assert!(!p.contains("{{TESTENV}}") && !p.contains("{{FOLLOWUP}}"));
            assert!(!p.contains("/../bin/testenv"));
            assert!(!p.contains("--suites test-"));
            assert_eq!(p.contains("/srv/h/spira/bead.sh file"), !enforce);
            assert_eq!(p.contains("work file-followup"), enforce);
            if std::env::var_os("AEON_PRINT_BRIEF").is_some() {
                println!("==== lifecycle_enforce={enforce} ====\n{p}");
            }
        }
    }

    fn runner_lines<'a>(p: &'a str, runner: &str) -> Vec<&'a str> {
        p.lines().filter(|l| l.contains(runner)).collect()
    }

    // The batcher brief, too, allows only the reproduce-one-named-suite run, never a pre-close one.
    #[test]
    fn batcher_chamber_runs_only_a_named_suite() {
        let chamber = include_str!("../../spira/chamber/batcher.md");
        let t = Tokens {
            single: vec![
                ("TESTENV", testenv_runner("", "/srv/h/spira")),
                ("REPO_NAME", "spira".into()),
            ],
            ..Default::default()
        };
        let p = render_prompt(chamber, &t, None);
        assert!(p.contains("    /srv/h/bin/testenv --suites <suite> <branch> spira\n"), "{p}");
        assert!(!p.contains("    /srv/h/bin/testenv <branch> spira\n"), "{p}");
        let lines = runner_lines(&p, "/srv/h/bin/testenv");
        assert!(!lines.is_empty() && lines.iter().all(|l| l.contains(" --suites <suite> ")));
        assert!(!p.contains("exactly what the landing"));
    }

    // test-aeon-chamber-overlay.sh GOLDEN (sp-wmcvb): {{FINISH}} follows lifecycle_enforce.
    #[test]
    fn finish_follows_lifecycle_enforce_only() {
        let off = finish_brief(false, "sp-a", "/db");
        assert!(!off.contains("You have no `bd`"));
        assert!(off.contains("bd -C /db close sp-a --reason-file - <<'REASON'"));
        assert!(off.contains("bd -C /db note sp-a \"BLOCKED:"));
        let on = finish_brief(true, "sp-a", "/db");
        assert!(on.contains("**You have no `bd`.**"));
        assert!(on.contains("this one: `sp-a`"));
        assert!(on.contains("    work submit\n"));
        assert!(!on.contains("bd -C /db close"));
    }

    #[test]
    fn landing_and_park_by_mode() {
        assert!(landing_brief("pr", "spira/sp-a", "main", "svc", "origin/main").contains("opens a pull request against `main`; svc's own CI"));
        assert!(landing_brief("hold", "b", "main", "svc", "o/m").starts_with("Spira does not advance svc's `main`"));
        assert!(landing_brief("queue.local", "b", "main", "svc", "local/main").ends_with("between your commit and `local/main`"));
        let pr = park_brief("pr", "spira/sp-a", "svc", "main");
        assert!(pr.contains("`landing-pass` pushes `spira/sp-a`"));
        assert!(pr.contains("\n$REPO_NAME's own CI runs"), "aeon.sh escaped $REPO_NAME in the pr text; kept");
        let push = park_brief("push", "spira/sp-a", "svc", "main");
        assert!(push.contains("svc lands by `push`"));
        assert!(push.ends_with("none of it needs you."));
    }

    #[test]
    fn fixture_brief_is_truthful_about_what_exists() {
        let none = fixture_brief(None, "spira/testdb.sh", "3308", 0);
        assert!(none.starts_with("This repository has no shared test fixture"));
        let f = FixtureInfo { name: "aeonspa".into(), dir: "/d".into(), baseline: "b".into(), bin: String::new(), started_service: "0".into(), mode: "embedded".into(), server_init_hash: String::new() };
        let e = fixture_brief(Some(&f), "spira/testdb.sh", "3308", 1234);
        assert!(e.contains("`TESTDB_NAME=aeonspa`"));
        assert!(e.contains("spending the 1234ms"));
        assert!(e.contains("embedded Dolt engine"));
        let s = fixture_brief(Some(&FixtureInfo { mode: "server".into(), ..f }), "spira/testdb.sh", "", 1);
        assert!(s.contains("dolt-beads-test server (port 3308)"));
    }

    // test-aeon-resume.sh's render rows.
    #[test]
    fn resume_slain_deadline_renderers() {
        assert_eq!(resume_brief("b", "/w", "0", "x"), "");
        assert_eq!(resume_brief("b", "/w", "?", "x"), "");
        let r = resume_brief("spira/sp-a", "/w", "2", "  abc one\n  def two");
        assert!(r.starts_with("## Prior work on this branch\n\n`spira/sp-a` carries **2** commit(s)"));
        assert!(r.contains("```\n  abc one\n  def two\n```"));
        assert_eq!(slain_brief("sp-a — the work", "1", "origin/main", "", "", "/l"), "");
        let s = slain_brief("sp-a: wip — salvaged at slay (operator stop)", "3", "origin/main", "2026-09-28 10:00:00 +0000", "", "/run/sp-a.log");
        assert!(s.contains("slain at 2026-09-28 10:00:00 +0000 (operator stop)"));
        assert!(s.contains("beyond `origin/main`; the last"), "an empty diffstat adds no stray parentheses");
        let s2 = slain_brief("sp-a: wip — salvaged at slay (x)", "3", "o/m", "", " 2 files changed", "/l");
        assert!(s2.contains("slain at unknown time (x)") && s2.contains("`o/m` ( 2 files changed);"));
        let dl = deadline_brief(Some(1_000_600), 1_000_000, Some("10:00:00 UTC".into()));
        assert!(dl.starts_with("**This session is killed at 10:00:00 UTC — 600 seconds from now.**"));
        assert!(dl.ends_with("    echo $(( 1000600 - $(date +%s) ))"));
        assert!(deadline_brief(Some(5), 0, None).contains("killed at epoch 5 —"));
        assert!(deadline_brief(None, 0, None).starts_with("**This session has no wall-clock deadline.**"));
    }

    #[test]
    fn holds_brief_rows() {
        assert!(holds_brief("sp-a", 2, "").contains("COULD NOT CHECK"));
        assert_eq!(holds_brief("sp-a", 0, "sp-a\tf.rs\n"), "", "a bead's own branch is not another holder");
        let h = holds_brief("sp-a", 0, "sp-b\tsrc/x.rs\nsp-a\tsrc/x.rs\n");
        assert!(h.ends_with("\n\n  sp-b  src/x.rs"), "{h}");
    }

    // test-brief-notes.sh T6: BEAD_BODY is passed through bound_bead_notes.
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

    #[test]
    fn overlays_whole_section_append_and_absent() {
        let d = tmpdir("ov");
        let chamber = d.join("builder.md");
        std::fs::write(&chamber, "# Builder\n## Tests\nrelease tests\nmore\n## Finishing\n{{FINISH}}\n\n").unwrap();
        let ov = d.join("overlay");
        std::fs::create_dir_all(ov.join("blocks")).unwrap();
        let (c, logs) = chamber_with_overlays("builder", &chamber, &ov);
        assert_eq!(c, "# Builder\n## Tests\nrelease tests\nmore\n## Finishing\n{{FINISH}}");
        assert!(logs.is_empty());
        std::fs::write(ov.join("builder.Tests.md"), "## Tests\nmine\n").unwrap();
        std::fs::write(ov.join("builder.No_Such.md"), "## No Such\nx\n").unwrap();
        std::fs::write(ov.join("builder.append.md"), "APPENDED\n").unwrap();
        let (c, logs) = chamber_with_overlays("builder", &chamber, &ov);
        assert_eq!(c, "# Builder\n## Tests\nmine\n## Finishing\n{{FINISH}}\n\nAPPENDED");
        assert!(logs.iter().any(|l| l.contains("names a section (## No Such) absent from") && l.ends_with("— ignored")));
        assert!(logs.iter().any(|l| l.starts_with("chamber section '## Tests' overlaid by")));
        std::fs::write(ov.join("builder.md"), "WHOLE\n").unwrap();
        let (c, logs) = chamber_with_overlays("builder", &chamber, &ov);
        assert_eq!(c, "WHOLE");
        assert!(logs[0].starts_with("brief replaced whole-file by"));
        std::fs::write(ov.join("blocks/PARK.md"), "my park\n").unwrap();
        let (p, l) = block_overlay(&ov, "PARK", "builtin".into());
        assert_eq!((p.as_str(), l.is_some()), ("my park", true));
        let (f, l) = block_overlay(&ov, "FINISH", "builtin\n".into());
        assert_eq!((f.as_str(), l), ("builtin", None));
    }

    #[test]
    fn substitution_is_literal_and_ordered() {
        let t = Tokens {
            single: vec![("BEAD_ID", "sp-a".into()), ("DB", "/d&b|x\\y".into())],
            bead: "body with {{PARK}} & more".into(),
            park: "PARK!".into(),
            fixture: "FX".into(),
            deadline: "DL".into(),
            finish: "FIN".into(),
        };
        let p = render_prompt("id {{BEAD_ID}} db {{DB}}\n{{BEAD}}\n{{PARK}}\n{{FIXTURE}}{{FIXTURE}}\n{{DEADLINE}}{{FINISH}}{{UNKNOWN}}", &t, None);
        // BEAD first, so a {{PARK}} inside the bead body is the first occurrence (as before).
        assert_eq!(p, "id sp-a db /d&b|x\\y\nbody with PARK! & more\n{{PARK}}\nFX{{FIXTURE}}\nDLFIN{{UNKNOWN}}");
    }

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

    #[test]
    fn split_layers() {
        let (s, t) = split("LAW", "persona\n<!-- task -->\ntask body\n<!-- task -->\nmore");
        assert_eq!(s, "# Memories in force\n\nLAW\n\n---\n\npersona\n");
        assert_eq!(t, "task body\n<!-- task -->\nmore");
        let (s, t) = split("LAW", "everything");
        assert_eq!(s, "# Memories in force\n\nLAW\n\n---\n\n");
        assert_eq!(t, "everything");
    }

    #[test]
    fn memories_core_and_index() {
        let j = r#"{"law-b":" B text ","law-a":"A text","sop-x":"X","other":"no","law-c":7}"#;
        let m = render_memories(j, "law-,sop-", 120000, "law-a", "/h");
        assert!(m.starts_with("## law-a\n\nA text\n\n\n## Statutes in force"), "{m}");
        assert!(m.contains("    /h/rule.sh show <slug-without-law-prefix>\nlaw-b"));
        assert!(m.ends_with("    /h/spira/sop.sh show <slug-without-sop-prefix>\nsop-x"));
        assert!(!m.contains("other"));
        let tight = render_memories(j, "law-", 5, "law-a", "/h");
        assert!(tight.starts_with("## Statutes in force") && tight.contains("law-a\nlaw-b"), "over budget falls back to the index");
        assert_eq!(render_memories("garbage", "law-", 1, "", "/h"), "");
    }

    #[test]
    fn close_and_already_done_blocks() {
        assert!(close_brief("local/main", "/w", "").contains("    # local/main is a local ref already in this checkout; no fetch needed\n    git -C /w rebase local/main"));
        assert!(close_brief("origin/main", "/w", "origin").contains("    git -C /w fetch origin\n"));
        assert!(already_done_brief("origin/main", "sp-a", "/w", "/db").contains("-n ${SPIRA_VERDICT_WINDOW:-400} origin/main | grep"));
        assert!(rebase_brief("b", "o/m", "/w", "").contains("conflicts in: unknown"));
        assert!(dirty_brief(&["a".into(), "b c".into()]).contains("```\n  a\n  b c\n```"));
        assert_eq!(dirty_brief(&[]), "");
    }
}
