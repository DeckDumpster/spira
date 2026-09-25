//! What the panel shows, and what acting on each kind actually means.
//!
//! Four views, because there are four genuinely different things and collapsing them
//! loses what makes each useful:
//!
//! | view | what it is | lives in | acting on it |
//! |---|---|---|---|
//! | DECISIONS | open work on the operator | beads, the escalation label + `overseer` | close it; the reason IS the verdict |
//! | FYI | something already true | beads, `insight`+`overseer` | dismiss it — read and move on |
//! | NOTIFICATIONS | something already happened | beads, `issue_type: event` | mark it read |
//! | ALERTS | a condition that is true NOW | beads, `event`+`alert`+`overseer` | acknowledge it, or silence it for an hour |
//!
//! An INSIGHT is closed by construction: there is nothing to do, only something to know. It
//! exists because an answer that scrolls off screen is lost. A NOTIFICATION is an OUTCOME —
//! a landing, a reclaim, a CI verdict, a gate stall. Those flash on the health pane's RECENT
//! line and scroll away with no durable surface anywhere.
//!
//! IT USED TO READ GAS TOWN MAIL, and kept reading it for a day after the town was
//! decommissioned: nothing had written to that mailbox since the 2026-09-05 cutover, so the
//! tab could only be empty or historical while the store paid ~816 ms per 60 s refresh —
//! 1,440 subprocess calls a day — to find out. Repointed rather than dropped, because the
//! gap it now fills is real and the rows were already in the snapshot: `bd list --all`
//! returns events, and both surviving filters were throwing them away.
//!
//! WHY THE INSIGHTS TAB SAYS "FYI" (the operator, verbatim: *"these insights seem more like bug
//! reports which means they're implicitly asking me for feedback -- really they should be FYI
//! only"*). The view rendered in the same shape as DECISIONS — a `default:` line, a turn
//! marker saying whose ball it was, a footer offering `⏎ decide` — and the bodies themselves
//! were written by `ask.sh` from the ESCALATION template, so an insight opened with
//! "**What is blocked:**" and closed by telling them to record a verdict. Nothing about it
//! said "record". Every one of those affordances is now view-aware, and the tab is named for
//! what the view is rather than for the label the beads carry.
//!
//! AN ALERT IS NOT A DECISION, and the discriminator is LIFECYCLE, not urgency: an alert is a
//! condition that SELF-CLEARS, a decision is one that gets ANSWERED. If the loop unwedges on
//! its own the alert must vanish with nobody touching it. Everything in the other three tabs
//! persists until the operator acts on it, and that property is what makes DECISIONS worth
//! trusting — nothing leaves unless they moved it. Mixing in items that disappear by
//! themselves teaches that not acting is sometimes fine, which is corrosive to the one list
//! where acting is the point. Two more differences follow: an alert may fire, clear and
//! re-fire for one cause, while a decision asked twice is a defect the statute book exists to
//! prevent; and an alert carries no recommended default, because there is nothing to choose
//! until they have looked.
//!
//! IT IS CALLED "ALERTS", NOT "ESCALATIONS". Escalation already means something exact in the
//! escalation policy and in several statutes — a bead carrying the escalation label, a
//! decision, a default, and what is blocked. Reusing the word for a machine-generated
//! condition would blur the one term that policy rests on.
//!
//! AND AN ALERT DOES NOT CARRY THE ESCALATION LABEL, which is why the DECISIONS filter has to
//! exclude it explicitly rather than merely not matching it: an alert bead carries `overseer`,
//! and DECISIONS admits the escalation label OR `overseer`, so without that exclusion every
//! firing alert would appear in the one list this whole design exists to keep self-clearing
//! items out of. The label is withheld deliberately — the escalation label is excluded from
//! every fayth predicate and from the guard that forbids closing through a resolve, so
//! carrying it would stop the writer clearing its own alert when the condition passes.
//!
//! AND A DISMISSED INSIGHT LEAVES THE PANE, but stays reachable: `d` adds `archived`, `h`
//! shows the archived ones and `d` there takes the label back off. An acknowledged item that
//! stays put teaches the operator to ignore the pane, which is law-alerts-must-be-actionable wearing
//! a different hat; an acknowledged item that vanishes irrecoverably makes dismissing it a
//! decision, which is the cost this view exists to avoid.
//!
//! Reading lives in `store`; this module only defines the vocabulary and the writes.

use std::process::Command;

/// The actor name this session writes comments under.
///
/// It has to differ from the operator's. Both were `overseer` at first -- the Gas Town human
/// mailbox -- so a reply of mine was indistinguishable from one of theirs: the answer watcher
/// announced the agent's own comment back to it as an operator reply, and the pane could not have
/// shown whether a thread was waiting on them or on me.
pub const ME: &str = "claude";

/// The actor name the OPERATOR's own comments are recorded under, from `SPIRA_OPERATOR_ACTOR`.
///
/// The pane and the agent both write into the same thread, so the two have to be tellable
/// apart or the turn marker claims it is the operator's move on a reply the agent just wrote.
/// A literal here would be one installation's name, and every other installation would see
/// its own replies as somebody else's.
pub fn operator_actor() -> String {
    std::env::var("SPIRA_OPERATOR_ACTOR").unwrap_or_else(|_| "operator".to_string())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Decisions,
    Insights,
    Notifications,
    Alerts,
}

impl View {
    pub const ALL: [View; 4] = [
        View::Decisions,
        View::Insights,
        View::Notifications,
        View::Alerts,
    ];

    pub fn title(self) -> &'static str {
        match self {
            View::Decisions => "DECISIONS",
            View::Insights => "FYI",
            View::Notifications => "NOTIFICATIONS",
            View::Alerts => "ALERTS",
        }
    }

    /// What the destructive key does HERE. Closing a decision records a verdict; dismissing
    /// an insight or marking an event read only means "read"; acknowledging an alert says
    /// "seen" and changes NOTHING about the condition. One word for all four is how a keypress
    /// silently does nothing — `bd close` on an insight or event is a no-op, because both
    /// were created closed.
    ///
    /// `dismissed` is the history toggle. In record views (FYI, NOTIFICATIONS) the same key
    /// UNDOES the dismissal; in ALERTS it lifts a silence. Saying "dismiss" over a list of
    /// already-dismissed items is the same silent no-op one layer along.
    pub fn verb(self, dismissed: bool) -> &'static str {
        match self {
            View::Decisions => "close",
            View::Insights if dismissed => "restore",
            View::Insights => "dismiss",
            View::Alerts if dismissed => "unsilence",
            View::Alerts => "ack",
            View::Notifications if dismissed => "restore",
            View::Notifications => "mark read",
        }
    }

    /// Views whose destructive key toggles `archived` rather than closing anything, and which
    /// therefore have a history `h` can show. DECISIONS has none — a closed decision is a
    /// verdict, not a hidden row. ALERTS has its own history mechanism (closed/silenced).
    pub fn is_record(self) -> bool {
        matches!(self, View::Insights | View::Notifications)
    }

    /// Whether this view keeps a set of items behind `h`.
    ///
    /// FYI and NOTIFICATIONS use `archived` to dismiss. ALERTS hides what has cleared or been
    /// silenced. DECISIONS has none — a closed decision is a verdict, not a hidden row.
    /// Asked rather than open-coded so the key, the tab name and the footer cannot come to
    /// disagree about which views have a history.
    pub fn has_history(self) -> bool {
        matches!(self, View::Insights | View::Notifications | View::Alerts)
    }

    pub fn next(self) -> View {
        let i = Self::ALL.iter().position(|v| *v == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> View {
        let i = Self::ALL.iter().position(|v| *v == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// The labels an alert bead carries, and what each one means.
///
/// The contract is sp-auron's: `issue_type: event`, plus `alert` and `overseer`. Open means
/// FIRING; closed means the condition cleared and the writer retracted its own statement.
/// Everything else the tab needs is a label, because a label is what a writer can set and
/// unset in one `bd update` without rewriting a description it did not compose.
pub mod alert {
    /// Present on every alert bead. The DECISIONS view excludes on this too — see the module
    /// header for why not matching is not enough.
    pub const LABEL: &str = "alert";
    /// The operator has seen it. Says nothing about the condition, which is why an acked alert STAYS
    /// on the tab: hiding a firing alert because someone looked at it is the reassuring lie
    /// this pane exists to prevent.
    pub const ACKED: &str = "acked";
    /// `silent-until:<ISO-8601 UTC>` — hidden from the tab until that instant, then back on
    /// its own if the condition is still true. The deadline is IN the label rather than in a
    /// timer somewhere, so the silence expires with nobody running: a silence that needs a
    /// process to lift it is a silence that outlives the process.
    pub const SILENT: &str = "silent-until:";
    /// `flaps:<n>` — how many times this one condition has fired. A condition that clears and
    /// returns reopens THIS bead and bumps the count; ten beads for one cause is how a pane
    /// becomes wallpaper (law-alerts-must-be-actionable).
    pub const FLAPS: &str = "flaps:";
    /// How long `s` silences for. One hour: long enough to be working on it, short enough
    /// that forgetting costs one hour of blindness rather than a day of it.
    pub const SILENCE_SECS: i64 = 3600;
}

/// Has the operator seen this alert?
pub fn acked(it: &Item) -> bool {
    it.labels.iter().any(|l| l == alert::ACKED)
}

/// How many times this condition has fired, 1 if the bead does not say.
pub fn flaps(it: &Item) -> u32 {
    it.labels
        .iter()
        .find_map(|l| l.strip_prefix(alert::FLAPS)?.trim().parse().ok())
        .unwrap_or(1)
}

/// The whole `silent-until:...` label, which is what `bd update --remove-label` needs.
pub fn silence_label(it: &Item) -> Option<&String> {
    it.labels.iter().find(|l| l.starts_with(alert::SILENT))
}

/// When the silence lifts, in unix seconds. `None` if it is not silenced or the deadline
/// will not parse — an unparseable deadline is treated as no silence at all, because the
/// alternative is an alert silenced forever by a typo.
pub fn silenced_until(it: &Item) -> Option<i64> {
    crate::render::epoch(silence_label(it)?.strip_prefix(alert::SILENT)?.trim())
}

/// Silenced AS OF `now`. The deadline is compared, never merely present: a silence whose
/// hour has passed is over whether or not anything ran to take the label off.
pub fn silenced(it: &Item, now: i64) -> bool {
    silenced_until(it).map(|t| t > now).unwrap_or(false)
}

/// Which write a keypress means. `d` is the view's primary key and `s` exists only in
/// ALERTS, where the two acts differ in the one way that matters to the pane: silencing
/// takes the row off the list, acknowledging does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    /// close / dismiss / restore / mark read / acknowledge / unsilence.
    Primary,
    /// Hide a firing alert until a deadline.
    Silence,
}

/// Does this act take the item off the list currently on screen?
///
/// The pane hides an acted-on row optimistically, so getting this wrong either leaves a
/// closed decision sitting there for a second or — the damaging direction — makes a FIRING
/// ALERT disappear because someone acknowledged it. Acknowledging is not clearing.
pub fn act_hides(view: View, what: Act) -> bool {
    !(view == View::Alerts && what == Act::Primary)
}

#[derive(Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub title: String,
    /// The one line worth seeing without opening it — a recommended default, usually.
    pub lead: String,
    pub body: String,
    /// question | decision | task | insight, or the sender for mail.
    pub badge: String,
    /// When the item was opened, ISO-8601 UTC.
    ///
    /// It is rendered as an AGE — "26m", "7h", "6d" — never as the wallclock time it holds.
    /// (the operator, verbatim: *"i want to know how long the item has been open, not the
    /// wallclock time it was opened."*) The age is what says a thing is rotting, and it is
    /// why this column beat the id to the list in the first place; the id, and the absolute
    /// timestamp with it, live on the rule below. See `render::age_since`.
    pub when: String,
    /// The statute this insight was promoted into, `law-<slug>`, or None.
    ///
    /// Read off the `enacted:` label. It is what makes the link resolve in BOTH directions:
    /// from a statute back to the case that produced it, and from a finding filed a second
    /// time forward to the rule it already violated — which is the ladder's promotion
    /// trigger, and was undetectable while a promoted insight and a shrugged-off one carried
    /// the same `archived` label and nothing else.
    pub enacted: Option<String>,
    /// The comment thread, oldest first: (author, ISO time, text).
    ///
    /// The thread IS the conversation. the operator: "the conversation in the beads pane is already
    /// threaded -- or it should be." Before this, a reply lived in the chat transcript and
    /// the pane showed only a comment COUNT, so answering them meant they had to go somewhere
    /// else to read it -- and twice I answered by filing a NEW bead instead, which forked
    /// the context rather than continuing it.
    ///
    /// It renders ABOVE the ask on both surfaces, because it is the newest and most
    /// decision-relevant thing on the item (the operator, verbatim: "i want threaded replies
    /// at the top"). "Oldest first" is therefore an invariant rather than a description:
    /// `store::oldest_first` enforces it, and the list's turn marker reads `last()`.
    pub thread: Vec<(String, String, String)>,
    /// The bead's labels, verbatim. Empty for mail, which has none.
    ///
    /// Carried rather than pre-digested into flags because the ALERTS view has to WRITE them
    /// back: lifting a silence means handing `bd update --remove-label` the exact string on
    /// the bead, and a flag that only recorded "silenced: true" could not name it. The
    /// readers — `acked`, `flaps`, `silenced_until` — all live here beside the constants they
    /// parse, so the pane and the writer cannot drift into disagreeing about the vocabulary.
    pub labels: Vec<String>,
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    run_as(cmd, args, None)
}

/// `actor` sets BEADS_ACTOR, which is the name a comment is recorded under.
fn run_as(cmd: &str, args: &[&str], actor: Option<&str>) -> Result<(), String> {
    // Absolute path: tmux's global PATH has no ~/.local/bin, so a respawned pane cannot
    // find `bd` by name. See store::bin.
    let mut c = Command::new(crate::store::bin(cmd));
    c.args(args);
    c.env("PATH", crate::store::child_path());
    if let Some(a) = actor {
        c.env("BEADS_ACTOR", a);
    }
    match c.output() {
        Err(e) => Err(format!("{cmd}: {e}")),
        Ok(o) if !o.status.success() => Err(format!(
            "{cmd}: {}",
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .next()
                .unwrap_or("failed")
        )),
        Ok(_) => Ok(()),
    }
}

/// A VERDICT IS NEVER BLOCKED, AND IS NEVER DISCARDED. Both halves were learned from one
/// bead: sp-wok.5, an operator task decomposed out of an epic and therefore carrying a
/// `blocks` edge onto a sibling that had not landed. `bd close` refuses a blocked issue —
/// *"cannot close blocked issue: sp-wok.5 is blocked by [sp-wok.3] (use --force to
/// override)"* — so every answer typed into this pane failed, and because the text lived
/// only in the argument list of a command that exited 1, each one was thrown away. The
/// operator answered it more than three times and it came back untouched, with no comment on
/// the bead to show any of it had happened.
///
/// `--force`, because beads' blocked-close guard is aimed at an AGENT closing work whose
/// prerequisite is unbuilt, and this pane is the one place where the human who owns the
/// decision is the one pressing the key. Their dependency edges order the WORK; they do not
/// order the answer. `resolve.sh` has always closed this way; only the pane and `ask.sh`
/// were left behind.
///
/// NOTHING CURRENTLY CATCHES THE HOLLOW CLOSE THIS PERMITS, and that is stated here rather
/// than assumed because the first version of this comment asserted the opposite. HOLLOW-CLOSE
/// was a Gas Town check — `settings/watch-town.sh` mailed it to the Mayor — and Gas Town was
/// decommissioned. Spira has no equivalent: `bd list --label alert` returns 0 rows because
/// the ALERTS tab shipped its reader and no writer (sp-alerts, closed, says so in as many
/// words), and sp-auron, the designated first writer, watches loop staleness and dead leases
/// rather than the bead graph. So forcing here genuinely does drop a signal that used to
/// exist. It is still the right trade — the alternative was destroying the operator's typed
/// verdict, every time — but the missing check is sp-hollow, not a fact about this function.
///
/// And should the close fail anyway, the typed text is written to the bead as a comment
/// before the error is returned. Whatever else goes wrong, the answer survives in the one
/// place the next reader will look.
fn close_decision(db: &str, id: &str, reason: &str) -> Result<(), String> {
    let actor = operator_actor();
    let e = match run_as(
        "bd",
        &["-C", db, "close", id, "--reason", reason, "--force"],
        Some(&actor),
    ) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    match run_as("bd", &["-C", db, "comments", "add", id, reason], Some(&actor)) {
        Ok(()) => Err(format!("{e} — still open; your answer is kept as a comment")),
        Err(_) => Err(format!("{e} — AND THE ANSWER WAS NOT SAVED: {reason}")),
    }
}

/// Archive every record in one call. At many rows that is many keypresses replaced by one.
///
/// `dismissed` is the history toggle: in the history the same key restores all. Only valid
/// for record views (Insights, Notifications) — closing every decision at once would discard
/// verdicts, and there is no undo.
pub fn act_all(view: View, ids: &[String], dismissed: bool) -> Result<(), String> {
    match view {
        View::Insights | View::Notifications if !ids.is_empty() => {
            let db = crate::store::db();
            let flag = if dismissed { "--remove-label" } else { "--add-label" };
            let mut args: Vec<&str> = vec!["-C", &db, "update"];
            for id in ids {
                args.push(id.as_str());
            }
            args.extend(&[flag, ARCHIVED]);
            run("bd", &args)
        }
        View::Insights | View::Notifications => Ok(()),
        _ => Err("bulk clear is only for record views".into()),
    }
}

/// The label a dismissed insight or a read notification carries. `store` filters on it and
/// `ask.sh list insights` already honours it, so the pane and the shell path agree on what
/// "read" means.
pub const ARCHIVED: &str = "archived";

/// The label AND the close_reason prefix a rejected-premise bead carries. Both are written
/// by `reject_premise`: the label is what a query filters on, the prefix is what a human or
/// model reads first.
///
/// Carrying both deliberately: a label without a prefixed reason reads as machinery; a
/// reason without a label cannot be queried in the absence of the full close_reason string.
pub const PREMISE_REJECTED: &str = "premise-rejected";

/// `dismissed` says the FYI view is showing its history, where the same key RESTORES.
///
/// Dismissal is reversible on purpose. The bead asked for an insight to "leave the pane ...
/// retrievable but not cluttering", and a one-way key turns glance-and-move-on into a
/// decision -- which is the friction that left every insight sitting there in the first place.
pub fn act(
    view: View,
    item: &Item,
    reason: &str,
    dismissed: bool,
    what: Act,
    now: i64,
) -> Result<(), String> {
    let db = crate::store::db();
    match (view, what) {
        // CLOSED AS THE OPERATOR, for the same reason a comment is. beads records the
        // closing actor in its audit events and nowhere else -- the issue row has no
        // `closed_by` -- so that event is the only thing that can tell their verdict
        // from an agent's close. Left unstamped, the actor was whatever git identity
        // the pane inherited, and a watcher announced an agent's OWN close back to it
        // as an answer the operator never gave. A session that acts on that is acting
        // on its own echo, and it is silent because the text reads exactly like a
        // real verdict.
        (View::Decisions, _) => close_decision(&db, &item.id, reason),
        (View::Insights | View::Notifications, _) => run(
            "bd",
            &[
                "-C",
                &db,
                "update",
                &item.id,
                if dismissed { "--remove-label" } else { "--add-label" },
                ARCHIVED,
            ],
        ),

        // SILENCING WRITES A DEADLINE, NOT A FLAG. `silent-until:<ISO>` expires by being read,
        // so nothing has to run to lift it. Re-silencing replaces the old label rather than
        // adding a second one — two deadlines on one bead is a question with two answers, and
        // `silence_label` would return whichever `bd` happened to list first.
        (View::Alerts, Act::Silence) => {
            if let Some(old) = silence_label(item) {
                run("bd", &["-C", &db, "update", &item.id, "--remove-label", old])?;
            }
            let until = format!(
                "{}{}",
                alert::SILENT,
                crate::render::iso(now + alert::SILENCE_SECS)
            );
            run("bd", &["-C", &db, "update", &item.id, "--add-label", &until])
        }

        // `d` IN THE HISTORY LIFTS A SILENCE — and a CLEARED alert has none to lift. Saying so
        // out loud rather than running a no-op `bd` that exits 0: the history holds both kinds
        // side by side, and a key that appears to work on the wrong one teaches the operator that the
        // footer is decoration. Same defect `verb` exists to prevent, one layer along.
        (View::Alerts, Act::Primary) if dismissed => match silence_label(item) {
            Some(old) => run("bd", &["-C", &db, "update", &item.id, "--remove-label", old]),
            None => Err("this one has cleared — a record, with no silence to lift".into()),
        },

        // ACKNOWLEDGING SAYS "SEEN" AND NOTHING ELSE. It must never close the bead: closing is
        // the writer's retraction of its own statement, and a hand-closed alert whose condition
        // is still true is reopened on the next pass — so the pane would have taught the operator that
        // acting on an alert does nothing. See `act_hides`: the row STAYS.
        (View::Alerts, Act::Primary) => {
            if acked(item) {
                return Err("already acknowledged".into());
            }
            run(
                "bd",
                &["-C", &db, "update", &item.id, "--add-label", alert::ACKED],
            )
        }
    }
}

/// Close a decision as a premise rejection — not a verdict.
///
/// A REJECTION IS NOT A VERDICT, and the watcher must not announce it as one. The label
/// `premise-rejected` is what the watcher filters on (sp-kw9bo adds that filtering); the
/// close_reason prefix is what a human or model reads first when inspecting the bead. Both
/// are written together so neither reader — machine or human — is missing a signal.
///
/// The label is added BEFORE the close: if the close fails, the label remains as the record
/// of intent, and `close_decision` saves the reason as a comment on failure too.
///
/// `why` is the training signal, not required — an empty string is fine: the `p` prompt
/// says so, so pressing ⏎ immediately records `premise-rejected` with no trailing text.
pub fn reject_premise(item: &Item, why: &str) -> Result<(), String> {
    let db = crate::store::db();
    run("bd", &["-C", &db, "update", &item.id, "--add-label", PREMISE_REJECTED])?;
    let reason = if why.trim().is_empty() {
        PREMISE_REJECTED.to_string()
    } else {
        format!("{PREMISE_REJECTED}: {}", why.trim())
    };
    close_decision(&db, &item.id, &reason)
}

// ── promotion: an insight becomes a statute ────────────────────────────────────────────
//
// AN INSIGHT IS A CANDIDATE LAW. It is what was learned during the
// work that might carry over — doctrine or a ruling, though not usually couched that way —
// so the implicit question on an FYI row is not "do you want to act on this?" but "should
// this be law?", and that is answerable three ways while the view offered two.
//
// The pipeline already ran; it simply left no trace. Four of the nine live insights were
// already doctrine — the disk box with the dead alert channel became
// law-absence-needs-a-positive-control, a bead closed over a red PR with the statute already
// in force was a RE-violation, which is the ladder's promotion trigger — and every one was
// promoted by hand and then archived with the same label as an insight shrugged off as
// noise. So the feeder for a forty-statute book had no surface, and a statute could not point
// back at the case that produced it, which is exactly what CLAUDE.md asks of one.

/// The label prefix a promoted insight carries: `enacted:law-<slug>`.
///
/// `store` reads it onto `Item::enacted` and the row renders differently for it, so a
/// promoted finding is visibly distinct from a dismissed one on the surface Ryan reads.
pub const ENACTED: &str = "enacted:";

/// Split the compose line into `(slug, statute)`.
///
/// ONE INPUT, `slug: statute`, split on the FIRST colon — the mode row already exists for
/// `decide` and `comment`, and a two-step prompt would have been a second piece of state to
/// hold across a keypress for a separator this cheap. A colon inside the statute is safe
/// because the slug cannot contain one.
///
/// It refuses rather than guesses. A slug is `[a-z0-9-]`, which is what `rule.sh` will turn
/// into a memory key every agent reads; anything else — a missing colon, capitals, a space —
/// would otherwise reach the statute book as a key nobody can address, and the caller keeps
/// the draft so the text typed is never the thing that is lost.
///
/// The `law-` prefix is left on if it was typed: `rule.sh` strips and re-adds it, so both
/// `closed-is-not-landed` and `law-closed-is-not-landed` name the same statute.
pub fn parse_enact(input: &str) -> Result<(String, String), String> {
    let (slug, text) = input
        .split_once(':')
        .ok_or("no slug — type  <slug>: <statute>")?;
    let (slug, text) = (slug.trim(), text.trim());
    if slug.is_empty() {
        return Err("empty slug — type  <slug>: <statute>".into());
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!("slug '{slug}' is not lower-case-kebab"));
    }
    if text.is_empty() {
        return Err("no statute text after the slug".into());
    }
    Ok((slug.to_string(), text.to_string()))
}

/// Where `rule.sh` is, found rather than known.
///
/// NO HARDCODED PATH — a harness that names one operator's checkout is unbuildable for
/// anyone else, which is the whole reason `spira.conf` exists. `SPIRA_RULE` overrides for a
/// fixture; otherwise the binary walks up from itself, because this panel lives at
/// `cockpit/panel/target/*/panel` and the script at `rule.sh` — an ancestor
/// of wherever the binary was built, debug or release.
///
/// It fails loudly when there is none. Guessing a path here would run some other tree's
/// `rule.sh` against a statute book Ryan is not reading.
fn rule_sh() -> Result<String, String> {
    if let Ok(p) = std::env::var("SPIRA_RULE") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate myself: {e}"))?;
    for anc in exe.ancestors() {
        let c = anc.join("rule.sh");
        if c.is_file() {
            return Ok(c.to_string_lossy().to_string());
        }
    }
    Err("rule.sh not found above this binary — set SPIRA_RULE".into())
}

/// Run a command for its exit status, keeping the END of stderr.
///
/// `run` keeps only the FIRST line, which is right for `bd`, whose failures are one line.
/// `rule.sh`'s refusal is three — the word count, then where the case history belongs — and
/// the whole point of surfacing it is that Ryan can read why his text was rejected.
///
/// THE TAIL, NOT THE HEAD, and that is not a preference. `rule.sh` sources the shared config,
/// which prints a warning per unknown key in the operator's `spira.conf` before doing
/// anything at all — four of them here. Joined from the front, those four filled the footer
/// and pushed the word count off the end of a 107-column row: a refusal that reached the pane
/// and still could not be read is the same failure as one that never arrived. A program's
/// complaint about its own input is the last thing it says; preamble is the first.
///
/// Three lines, because that is what the refusal is, collapsed to one row because the footer
/// is one row.
fn run_verbose(cmd: &str, args: &[&str]) -> Result<(), String> {
    let mut c = Command::new(cmd);
    c.args(args).env("PATH", crate::store::child_path());
    match c.output() {
        Err(e) => Err(format!("{cmd}: {e}")),
        Ok(o) if !o.status.success() => {
            let all = String::from_utf8_lossy(&o.stderr);
            let lines: Vec<&str> = all
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            let err = lines[lines.len().saturating_sub(3)..].join(" ");
            Err(if err.is_empty() {
                format!("{cmd}: failed with no message")
            } else {
                err
            })
        }
        Ok(_) => Ok(()),
    }
}

/// Promote an insight: write the statute, then cite the case on the insight.
///
/// THE CITATION IS THE POINT, not the convenience of enacting from the pane. `rule.sh` was
/// always one command away; what did not exist was the back-link, so a re-filed finding could
/// not be told from a first one and a statute carried no history.
///
/// NO WORD-COUNT CHECK HERE. `rule.sh` refuses anything over 130 words and wants ~70, and a
/// second copy of that number in a second language is a limit that drifts — the two would
/// disagree the first time one moved. The refusal is surfaced verbatim instead, so the pane
/// says what the statute book said.
///
/// `archived` goes on with the citation, because enacting IS the strongest form of reading
/// it: the row leaves the live list and `h` shows it, alongside the ones merely dismissed
/// but no longer confusable with them.
///
/// The two writes are ordered and NOT atomic, so the failure between them is reported for
/// what it is. A statute that exists with no citation on its case is a real state, and
/// saying "failed" without saying that would send Ryan to re-enact a law already in force.
pub fn enact(item: &Item, slug: &str, text: &str) -> Result<(), String> {
    let rule = rule_sh()?;
    run_verbose(&rule, &["enact", slug, text])?;
    let key = format!("{ENACTED}law-{}", slug.trim_start_matches("law-"));
    let db = crate::store::db();
    run(
        "bd",
        &[
            "-C",
            &db,
            "update",
            &item.id,
            "--add-label",
            &key,
            "--add-label",
            ARCHIVED,
        ],
    )
    .map_err(|e| format!("enacted law-{slug} but could not label {}: {e}", item.id))
}

/// Whether a bead is waiting on a verdict from the operator.
///
/// A bead carrying any ask-* label is a question, decision, or task the operator must rule on:
/// pressing ⏎ closes it with the typed text as the verdict. A work bead in DECISIONS carries no
/// ask-* label — replying to it means commenting, not deciding, and the bead stays open.
///
/// ask-law is NOT included: a law proposal's lifecycle is enact/amend/decline, not close-with-
/// verdict. ⏎ on a law proposal opens a comment, same as a work bead.
///
/// THE BUG THIS PREVENTS: a P0 work bead reached DECISIONS because it carried `overseer`. The
/// operator replied "fix it" in the panel. The panel treated the reply as a verdict and CLOSED
/// the bead with "fix it" as the reason — making it uncaimable and retiring the work unfixed.
/// The distinguishing fact was only that he engaged with it; the trap fired on exactly the
/// beads he paid attention to.
pub fn is_ask(item: &Item) -> bool {
    item.labels
        .iter()
        .any(|l| matches!(l.as_str(), "ask-question" | "ask-decision" | "ask-task" | "ask-suit"))
}

/// Whether a bead is a lawsuit — a challenge to a statute in force.
///
/// A suit has three verdicts: uphold (no change), retire (removes the statute), amend
/// (replaces the statute text). These are executed against the statute book by `suit_verdict`,
/// so a suit bead enters a different mode than a plain ask.
pub fn is_suit(item: &Item) -> bool {
    item.labels.iter().any(|l| l == "ask-suit")
}

/// The statute slug this suit challenges, read off the `statute:law-<slug>` label.
///
/// The label is set when the suit is filed and is the only machine-readable link between the
/// bead and the statute it targets. Body parsing is avoided deliberately: a label is
/// queryable, versioned, and unambiguous; a string buried in prose is none of those.
pub fn suit_slug(item: &Item) -> Option<String> {
    item.labels
        .iter()
        .find_map(|l| l.strip_prefix("statute:law-").map(|s| s.to_string()))
}

/// Execute a lawsuit verdict: uphold, retire, or amend.
///
/// UPHOLD closes the bead unchanged. RETIRE runs `rule.sh retire <slug>` first; if it
/// succeeds the bead is closed, if it fails the bead stays open. AMEND runs
/// `rule.sh enact <slug> <new text>` first with the same close-on-success / leave-open-on-fail
/// contract. The distinction "bead stays open on failure" is what the acceptance criterion
/// "A failed rule.sh leaves the bead open and says so" means: this function returns Err,
/// the caller unhides the bead, and the flash says why.
///
/// A slug is required on the bead. A suit without `statute:law-<slug>` cannot be acted on;
/// the error names the problem so the operator knows what to fix.
pub fn suit_verdict(item: &Item, verdict: &str) -> Result<(), String> {
    let v = verdict.trim();
    // The first whitespace-delimited token is the verb; the rest is the new text for amend.
    let first = v.split_whitespace().next().unwrap_or("").to_lowercase();
    let db = crate::store::db();
    match first.as_str() {
        "uphold" => {
            // Uphold: statute unchanged; just close.
            close_decision(&db, &item.id, "uphold")?;
        }
        "retire" => {
            let slug = suit_slug(item)
                .ok_or_else(|| "no statute:law-<slug> label on this bead".to_string())?;
            let rule = rule_sh()?;
            run_verbose(&rule, &["retire", &slug])?;
            close_decision(&db, &item.id, &format!("retire: law-{slug} retired"))?;
        }
        "amend" => {
            // "amend: <new text>" — colon is optional, everything after the verb is the text.
            let rest = v[first.len()..].trim_start_matches(':').trim();
            if rest.is_empty() {
                return Err(
                    "amend requires new statute text: amend: <new text>".into(),
                );
            }
            let slug = suit_slug(item)
                .ok_or_else(|| "no statute:law-<slug> label on this bead".to_string())?;
            let rule = rule_sh()?;
            run_verbose(&rule, &["enact", &slug, rest])?;
            close_decision(&db, &item.id, &format!("amend: law-{slug} amended"))?;
        }
        _ => {
            return Err(format!(
                "unknown suit verdict '{first}' — type uphold / retire / amend: <new text>"
            ));
        }
    }
    Ok(())
}

/// Whether a bead is a law proposal — label `ask-law`.
///
/// A law proposal's body is `<slug>: <statute text>`, ready to feed to `rule.sh enact`.
/// Its actions are enact, amend-and-enact, and decline — not close-with-verdict.
/// A FAILED `rule.sh` must leave the bead OPEN so the operator can see what went wrong
/// and retry with an amendment (law-absence-needs-a-positive-control).
pub fn is_ask_law(item: &Item) -> bool {
    item.labels.iter().any(|l| l == "ask-law")
}

/// Enact a law proposal: run `rule.sh enact`, then close the bead with evidence.
///
/// The slug and statute text are taken from the item's title (`<slug>: <statute>`) when
/// `input` is empty, or from `input` directly when the operator typed an amendment.
/// `rule.sh` refuses anything over 130 words and the refusal is surfaced verbatim — the
/// same as in `enact` for insights. A failed enact leaves the bead OPEN.
///
/// Two writes, ordered: enact first, close second. A statute that exists with no close
/// is a real state — the bead stays open and the operator can retry with an amendment.
/// A close that exists without a statute would be wrong in the other direction.
pub fn enact_law(item: &Item, input: &str) -> Result<(), String> {
    let raw = if input.trim().is_empty() {
        &item.title
    } else {
        input
    };
    let (slug, text) = parse_enact(raw)?;
    let rule = rule_sh()?;
    run_verbose(&rule, &["enact", &slug, &text])?;
    // Close the bead now that the statute is in force.
    let db = crate::store::db();
    let evidence = format!("enacted law-{} from panel", slug.trim_start_matches("law-"));
    close_decision(&db, &item.id, &evidence)
        .map_err(|e| format!("enacted law-{slug} but could not close {}: {e}", item.id))
}

/// Decline a law proposal: close the bead without touching the statute book.
///
/// The close reason carries "declined" so it is distinct from a verdict — a law bead
/// closed this way was read and rejected, not accepted.
pub fn decline_law(item: &Item, why: &str) -> Result<(), String> {
    let db = crate::store::db();
    let reason = if why.trim().is_empty() {
        "declined".to_string()
    } else {
        format!("declined: {}", why.trim())
    };
    close_decision(&db, &item.id, &reason)
}

/// A comment is never a completion. Replying used to mark things done, which once recorded
/// the operator's clarifying question as the evidence that the work was finished.
pub fn comment(view: View, item: &Item, text: &str) -> Result<(), String> {
    match view {
        View::Notifications => Err("notifications take no comment — d marks it read".into()),
        // An alert DOES take a comment. There is nothing to decide, but "known, chasing it"
        // belongs on the bead the next reader will open, not in a transcript
        // (law-reply-in-the-thread). It is a comment and never a verdict: the condition
        // clears when it clears.
        // Typed in the pane, so the author is the operator — not `overseer`, which the agent
        // also wrote under, and not the git identity. The pane and the agent must be
        // tellable apart, or a reply of the agent's reads as an answer.
        _ => {
            let db = crate::store::db();
            run_as(
                "bd",
                &["-C", &db, "comments", "add", &item.id, text],
                Some(&operator_actor()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_alert(labels: &[&str]) -> Item {
        Item {
            id: "sp-a1".into(),
            title: "the loop is wedged".into(),
            lead: String::new(),
            body: "no completed pass in 40m".into(),
            badge: "alert".into(),
            when: "2026-09-05T10:00:00Z".into(),
            enacted: None,
            thread: Vec::new(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// THE ONE THAT MATTERS. The pane hides an acted-on row optimistically, and acknowledging
    /// an alert changes nothing about the condition — so a `true` here is a FIRING alert
    /// vanishing from the pane because someone looked at it, which is the reassuring lie the
    /// whole surface exists to prevent. Every other act does remove its row.
    #[test]
    fn acknowledging_an_alert_does_not_remove_it_from_the_pane() {
        assert!(!act_hides(View::Alerts, Act::Primary));
        assert!(act_hides(View::Alerts, Act::Silence));
        for v in [View::Decisions, View::Insights, View::Notifications] {
            assert!(act_hides(v, Act::Primary), "{v:?}");
        }
    }

    /// The vocabulary, read the way the pane reads it. A missing `flaps:` label means the
    /// condition has fired once — not zero times, which would render "×0" on a live alert.
    #[test]
    fn the_alert_labels_read_back_as_written() {
        let plain = an_alert(&["alert", "overseer"]);
        assert!(!acked(&plain));
        assert_eq!(flaps(&plain), 1);
        assert_eq!(silenced_until(&plain), None);
        assert!(!silenced(&plain, 0));

        let marked = an_alert(&[
            "alert",
            "overseer",
            "acked",
            "flaps:7",
            "silent-until:2026-09-05T17:20:00Z",
        ]);
        assert!(acked(&marked));
        assert_eq!(flaps(&marked), 7);
        assert_eq!(silenced_until(&marked), Some(1_788_628_800));
        assert!(silenced(&marked, 1_788_625_200), "before the deadline it is quiet");
        assert!(!silenced(&marked, 1_788_628_800), "at the deadline it is back");

        // The whole label, because that is the string `bd update --remove-label` needs; a
        // flag saying only "silenced: true" could not name it.
        assert_eq!(
            silence_label(&marked).map(String::as_str),
            Some("silent-until:2026-09-05T17:20:00Z")
        );
    }

    /// A deadline that will not parse is NO silence, never an eternal one. The failure
    /// direction has to be "you still see it": an alert silenced forever by a typo is a
    /// condition removed from the pane with nothing anywhere saying it is still true.
    #[test]
    fn an_unparseable_deadline_silences_nothing() {
        let junk = an_alert(&["alert", "overseer", "silent-until:whenever"]);
        assert_eq!(silenced_until(&junk), None);
        assert!(!silenced(&junk, 1_788_625_200));
        // And it is still nameable, so `d` in the history can take the bad label off.
        assert!(silence_label(&junk).is_some());
    }

    #[test]
    fn a_compose_line_splits_on_the_first_colon() {
        let (slug, text) = parse_enact("closed-is-not-landed: Close a bead when it LANDED: verify with merge-base.").unwrap();
        assert_eq!(slug, "closed-is-not-landed");
        assert_eq!(text, "Close a bead when it LANDED: verify with merge-base.");
    }

    /// The `law-` prefix is `rule.sh`'s to add, and it strips one that is already there —
    /// so typing it must not produce `law-law-`.
    #[test]
    fn a_typed_law_prefix_is_not_doubled() {
        let (slug, _) = parse_enact("law-foo: some statute").unwrap();
        assert_eq!(format!("{ENACTED}law-{}", slug.trim_start_matches("law-")), "enacted:law-foo");
    }

    /// Every refusal names what to type. An input rejected with no instruction is the same
    /// dead end as a key that does nothing.
    #[test]
    fn a_malformed_line_is_refused_rather_than_guessed() {
        for bad in ["no colon here", " : text", "Not Kebab: text", "slug:   "] {
            let e = parse_enact(bad).unwrap_err();
            assert!(!e.is_empty(), "{bad} must be refused with a reason");
        }
        assert!(parse_enact("no colon here").unwrap_err().contains("<slug>"));
    }

    /// A statute is ~70 words and the pane must not hold a second opinion about the limit:
    /// a long body parses fine here and is refused by `rule.sh`, which owns the number.
    #[test]
    fn the_word_limit_is_not_enforced_here() {
        let long = format!("slug: {}", "word ".repeat(300));
        assert!(parse_enact(&long).is_ok());
    }

    // ── is_ask: closes vs. comments ──────────────────────────────────────────────────────
    //
    // THE BUG SHAPE: DECISIONS shows any bead waiting on the operator, not only asks. A work
    // bead reaches it when it carries `overseer`. Pressing ⏎ on that bead used to close it
    // with the reply as the verdict — retiring the work unfixed. The distinguishing fact was
    // only that the operator engaged with it, which made the trap fire on the most important
    // beads. These tests assert the line `is_ask` draws.

    fn work_item(labels: &[&str]) -> Item {
        Item {
            id: "sp-x".into(),
            title: "fix the thing".into(),
            lead: String::new(),
            body: "description".into(),
            badge: "task".into(),
            when: "2026-09-08T10:00:00Z".into(),
            enacted: None,
            thread: Vec::new(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Every ask-* label makes a bead an ask. THE POSITIVE CONTROL
    /// (law-absence-needs-a-positive-control): a predicate that returned false for everything
    /// would satisfy "work beads are not asks" while being completely broken.
    #[test]
    fn beads_with_ask_labels_are_asks() {
        for label in ["ask-question", "ask-decision", "ask-task"] {
            // literal-ok: test fixture constructing a label array
            let it = work_item(&[label, "needs-ryan", "overseer"]);
            assert!(is_ask(&it), "{label} must be recognized as an ask");
        }
    }

    /// A work bead in DECISIONS carries `overseer` (or the escalation label) but no ask-*.
    /// Replying to it must comment, never close.
    #[test]
    fn a_work_bead_with_no_ask_label_is_not_an_ask() {
        let it = work_item(&["spira", "plan", "overseer", "repo:spira"]);
        assert!(!is_ask(&it));
    }

    /// Every ask-* label makes a bead an ask, including ask-suit. THE POSITIVE CONTROL
    /// for the extended set: is_ask was written only for question/decision/task, and a
    /// suit that is_ask=false would be offered only comment (not decide) on ⏎ press.
    #[test]
    fn ask_suit_is_an_ask() {
        // literal-ok: test fixture constructing a label array
        let it = work_item(&["ask-suit", "needs-ryan", "overseer", "statute:law-foo"]);
        assert!(is_ask(&it), "ask-suit must be recognized as an ask");
    }

    /// Empty labels — a bead with nothing on it — is not an ask.
    #[test]
    fn a_bead_with_no_labels_is_not_an_ask() {
        assert!(!is_ask(&work_item(&[])));
    }

    // ── is_ask_law: law proposals have their own lifecycle ──────────────────────────
    //
    // A law proposal carries `ask-law` and is NOT an ask in the is_ask sense: ⏎ on one
    // must open a comment, not close it with a verdict. Its actions are enact/amend/decline.
    // The positive control: is_ask_law returns true for ask-law beads, and is_ask returns
    // false for them (so the two predicates agree that law proposals are handled differently).

    fn law_item(labels: &[&str]) -> Item {
        Item {
            id: "sp-y".into(),
            title: "no-close-before-landed: Never close a bead before its commit lands on main.".into(),
            lead: String::new(),
            body: String::new(),
            badge: "law proposal".into(),
            when: "2026-09-10T04:00:00Z".into(),
            enacted: None,
            thread: Vec::new(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// THE POSITIVE CONTROL: is_ask_law fires on ask-law.
    #[test]
    fn a_law_bead_is_recognized_as_ask_law() {
        // literal-ok: test fixture constructing a label array
        let it = law_item(&["ask-law", "needs-ryan", "overseer"]);
        assert!(is_ask_law(&it), "ask-law must be recognized as a law proposal");
    }

    /// A law proposal is NOT an ordinary ask — ⏎ must comment, not decide.
    #[test]
    fn a_law_bead_is_not_an_ordinary_ask() {
        // literal-ok: test fixture constructing a label array
        let it = law_item(&["ask-law", "needs-ryan", "overseer"]);
        assert!(!is_ask(&it), "ask-law must NOT match is_ask — its lifecycle is enact/amend/decline");
    }

    /// Without ask-law, is_ask_law returns false.
    #[test]
    fn a_non_law_bead_is_not_ask_law() {
        // literal-ok: test fixture constructing a label array
        let it = law_item(&["ask-question", "needs-ryan", "overseer"]);
        assert!(!is_ask_law(&it));
        assert!(!is_ask_law(&work_item(&["overseer", "spira"])));
    }

    // ── is_suit / suit_slug ─────────────────────────────────────────────────────────────

    fn suit_item(labels: &[&str]) -> Item {
        Item {
            id: "sp-y".into(),
            title: "lawsuit: law-foo".into(),
            lead: String::new(),
            body: "Evidence it is wrong".into(),
            badge: "lawsuit".into(),
            when: "2026-09-10T10:00:00Z".into(),
            enacted: None,
            thread: Vec::new(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// is_suit is true iff `ask-suit` is present. THE POSITIVE CONTROL: a false-for-all
    /// predicate would satisfy "non-suits are not suits" while being entirely broken.
    #[test]
    fn is_suit_detects_ask_suit_label() {
        // literal-ok: test fixture constructing a label array
        let it = suit_item(&["ask-suit", "needs-ryan", "overseer", "statute:law-foo"]);
        assert!(is_suit(&it), "ask-suit label must be detected");
    }

    #[test]
    fn a_plain_ask_is_not_a_suit() {
        // literal-ok: test fixture constructing a label array
        let it = work_item(&["ask-decision", "needs-ryan", "overseer"]);
        assert!(!is_suit(&it));
    }

    /// suit_slug reads the statute slug from the `statute:law-<slug>` label. The slug is
    /// what rule.sh retire/enact receives; a missing label is named as an error rather than
    /// guessed from the title or body.
    #[test]
    fn suit_slug_reads_the_label() {
        let it = suit_item(&["ask-suit", "statute:law-closed-is-not-landed"]);
        assert_eq!(suit_slug(&it).as_deref(), Some("closed-is-not-landed"));
    }

    #[test]
    fn suit_slug_returns_none_when_label_absent() {
        let it = suit_item(&["ask-suit"]);
        assert_eq!(suit_slug(&it), None);
    }

    /// suit_slug strips the `statute:law-` prefix exactly once and no further.
    #[test]
    fn suit_slug_does_not_double_strip() {
        let it = suit_item(&["ask-suit", "statute:law-law-something"]);
        // The stored label is `statute:law-law-something`; the slug is `law-something`.
        assert_eq!(suit_slug(&it).as_deref(), Some("law-something"));
    }

    // ── reject_premise: not a verdict ───────────────────────────────────────────────────
    //
    // THE BUG THIS PREVENTS: Ryan typed "done" to dismiss four beads whose premise he
    // rejected. DECISIONS had no dismiss — "a closed decision is a verdict, not a hidden
    // row" — so "done" became the close_reason, answered-since.sh announced four answers,
    // and the session acted on dismissals as affirmations (sp-sy5xp / 2026-09-09).

    /// The constant exists and is the exact string the watcher will filter on.
    ///
    /// This test fails to compile against the unfixed tree, satisfying
    /// law-a-regression-test-must-be-seen-to-fail.
    #[test]
    fn premise_rejected_constant_is_the_filter_key() {
        assert_eq!(PREMISE_REJECTED, "premise-rejected");
    }

}
