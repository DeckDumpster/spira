//! panel — the cockpit's bottom-left pane: decisions, FYI, notifications and alerts.
//!
//!     panel           interactive
//!     panel --once    print one frame and exit
//!
//! WHY IT EXISTS
//! -------------
//! Chat is a log, and a log cannot hold an open question. (the operator, verbatim: *"you'll
//! spit out 1,000 lines of shit and i don't even see the original thing you asked for"*.)
//! This is where a thing stays put while the transcript scrolls past it, and where the operator acts
//! on it without retyping which one they mean — *"an intuitive TUI for interacting with
//! certain kinds of beads out of band of our normal conversation"*.
//!
//! KEYS
//!     j k ↑ ↓     move                    tab / shift-tab   view
//!     d           act — close / dismiss / mark read / acknowledge, one keypress, no prompt
//!     D           the same, but prompt for a reason (not in ALERTS — see below)
//!     ⏎           decide (decisions) / comment (FYI, ALERTS)
//!     L           in FYI: enact — compose a statute from this insight and cite it
//!     s           in ALERTS: silence this condition for an hour
//!     h           in FYI and ALERTS: show the history; d there undoes
//!     r           force a sync            esc  cancel input
//!
//! `d` acts immediately because dismissing should not cost a sentence: *"i may want to
//! comment on an insight, but other times i can just read it and move on."*
//!
//! NOTHING IN THIS PANE CLOSES AN ALERT. An alert is a condition that self-clears, so the
//! only thing that may retract it is whatever asserted it; `D` is therefore unbound there,
//! and `d` acknowledges without touching the bead's status. A hand-closed alert whose
//! condition is still true comes straight back on the next pass, which would teach the operator that
//! acting on this tab does nothing — the exact way a pane becomes wallpaper
//! (law-alerts-must-be-actionable).
//!
//! NOTHING BLOCKS ON A SUBPROCESS. The store refreshes on a background thread; the UI only
//! reads the last snapshot and filters it in memory. Tab switching used to re-run two
//! identical `bd list` queries plus a mail query on every keypress — 1134 ms + 1134 ms +
//! 816 ms — which is what the operator measured as *"almost 5 seconds to switch tabs."*
//!
//! ACTIONS ARE OPTIMISTIC. The row disappears the moment you press the key, and the store
//! re-syncs behind it. Waiting a second for `bd` to confirm what you just asked for is the
//! same latency by another name.

mod model;
mod render;
mod store;
#[cfg(test)]
mod test_support;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use model::{act, comment, decline_law, enact, enact_law, is_ask_law, is_suit, reject_premise, suit_verdict, Act, Item, View};
use render::{frame, Frame};
use std::io::{stdout, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How often the store re-reads beads.
///
/// 60s, not 15s. A refresh is one `bd list --all` call, plus a comment fetch per displayable
/// bead. It used to also call `gt mail` — 816 ms per 60 s refresh, 1,440 subprocess calls a
/// day into a decommissioned store. Nothing here is real-time: it answers "what is waiting on
/// the operator", which changes a few times a day. The `r` key forces an immediate sync when
/// that is not good enough, and the header always says how stale the view is.
const REFRESH: Duration = Duration::from_secs(60);

struct App {
    shared: store::Shared,
    view: View,
    sel: usize,
    mode: Option<String>,
    buf: String,
    flash: String,
    /// Ids acted on locally, hidden until the next sync confirms them, paired with what state
    /// that confirmation looks like. This is what makes a keypress feel instant instead of
    /// costing a round trip.
    pending: Vec<(String, store::Expect)>,
    /// Failures from writes that ran off-thread. A write that fails must surface; the
    /// optimistic hide would otherwise lose the item silently.
    errors: Arc<Mutex<Vec<String>>>,
    /// Ids to put BACK on screen, because the write that hid them failed.
    ///
    /// The optimistic hide is only honest if it is undone when the write it was betting on
    /// does not land. It was not: `prune` keeps hiding an id for exactly as long as the store
    /// still returns it, so a failed `bd` left the row hidden until the next `r` — and the
    /// only trace was a flash that scrolls away on the next keypress. Harmless enough on a
    /// decision that is still open in the database; not harmless at all on a FIRING ALERT
    /// silenced by a write that failed, which is a condition removed from the pane with
    /// nothing anywhere saying it is still true.
    unhide: Arc<Mutex<Vec<String>>>,
    /// Writes still in flight, so the footer can say so rather than looking idle.
    inflight: Arc<Mutex<usize>>,
    /// A compose line the statute book would not take, kept so `L` can hand it back.
    ///
    /// Losing ~70 words to a refusal is not an error message, it is retyping. Stashed on a
    /// refusal AND on `esc`, cleared when a statute is enacted.
    draft: Arc<Mutex<Option<String>>>,
    /// The FYI view is showing what has already been dismissed.
    ///
    /// AN INSIGHT MUST BE ABLE TO LEAVE THE PANE (the operator, verbatim: *"responding to an
    /// insight is awkward because it doesn't leave the pane after i answer, so it stays
    /// cluttered"*) — and it must still be findable afterwards, or `d` is a delete key and they
    /// will weigh every press. `h` swaps the list for the dismissed ones and `d` there takes
    /// the label back off.
    ///
    /// It is deliberately NOT sticky across a tab change: a hidden filter that survives
    /// leaving the view is how a pane comes to report zero with items behind it, which is the
    /// failure `prune` already exists to prevent one layer down.
    dismissed: bool,
    /// Reading the selected item full-pane, and how far down.
    reading: bool,
    scroll: Scroll,
    /// The detail pane's own offset, so a body can be read without entering a mode.
    detail_scroll: Scroll,
    /// Bumped whenever `pending` changes, so the derived cache can key on it exactly
    /// rather than guessing from its length.
    pending_rev: u64,
    /// The instant every age and every deadline is measured against, frozen by `PANEL_NOW`.
    ///
    /// It lives on `App` rather than in the render loop because the ALERTS view needs it too:
    /// a silence carries its own expiry, so which items the view CONTAINS is a function of the
    /// clock and not only of the snapshot. See `derive` for what that costs the cache.
    frozen_now: Option<i64>,
    /// Derived lists, cached.
    ///
    /// WHY (the operator's call): *"when i use the arrow keys to scroll in one item's
    /// conversation, it can take second(s) ... likewise for scrolling through the open
    /// items themselves, and switching tabs."*
    ///
    /// The render path called `view_items` FIVE times per frame — once for the list, once
    /// per tab for the three counts, and once more inside `current()` — and each call scans
    /// every bead in every database and clones every description and comment thread it
    /// keeps. With ~800 rows and multi-kilobyte bodies that is megabytes of allocation per
    /// KEYSTROKE, under a load average north of 30. The data only changes when the store
    /// refreshes, every 15s, so deriving it per frame was pure waste.
    ///
    /// Cached against (snapshot generation, view, pending revision) — the three things that
    /// can change what the lists contain.
    cache: std::cell::RefCell<Option<Derived>>,
}

struct Derived {
    gen: u64,
    view: View,
    /// Part of the cache key: `h` changes what the lists contain without touching the
    /// snapshot, the view or `pending`, so a cache keyed on those three alone would keep
    /// serving the live insights after the toggle.
    dismissed: bool,
    pending_rev: u64,
    items: Result<Vec<Item>, String>,
    counts: [Option<usize>; 4],
}

/// Where the reader is, and how far down it can usefully go.
///
/// CLAMP THE STATE, NOT ONLY THE VIEW. Down used to be a bare `scroll += 1` with no upper
/// bound while the renderer clamped with `scroll.min(max_scroll)` at draw time, so every
/// keypress past the end incremented a number the frame ignored — and Up then had to burn
/// all of it off before the text moved. (the operator, verbatim: *"if i am at the bottom and
/// keep pressing down, i have to press up that many times in order to start scrolling up."*)
///
/// `max` is whatever the last reader frame reported, which is why it starts at `usize::MAX`
/// and returns there on every reset: an unknown bound must not clamp, or entering an item
/// and holding Down before the first paint would stop the reader short of the end. The paint
/// loop deliberately skips frames while input is still queued, so a burst of keypresses can
/// arrive against a bound from before the burst — hence `set_max`, which clamps the position
/// too. Together the two make the overshoot unreachable rather than merely invisible.
///
/// `Cell` because the render path holds `&App`; this is the same reason `cache` is a
/// `RefCell`.
struct Scroll {
    at: std::cell::Cell<usize>,
    max: std::cell::Cell<usize>,
}

impl Scroll {
    fn new() -> Self {
        Scroll { at: std::cell::Cell::new(0), max: std::cell::Cell::new(usize::MAX) }
    }
    fn get(&self) -> usize {
        self.at.get()
    }
    /// Back to the top, bound unknown again — the next frame will say what it is.
    fn reset(&self) {
        self.at.set(0);
        self.max.set(usize::MAX);
    }
    fn down(&self, d: usize) {
        self.at.set(self.at.get().saturating_add(d).min(self.max.get()));
    }
    fn up(&self, d: usize) {
        self.at.set(self.at.get().saturating_sub(d));
    }
    /// Record the bound the renderer measured, and pull the position back inside it.
    fn set_max(&self, max: usize) {
        self.max.set(max);
        if self.at.get() > max {
            self.at.set(max);
        }
    }
}

impl App {
    /// Drop optimistic hides the store has caught up with.
    ///
    /// `pending` used to grow without bound, and every stale entry kept filtering: after
    /// eight keypresses the panel showed NOTIFICATIONS 0 against a real 17 unread. An id is
    /// only worth hiding while the store still reports it; once the sync no longer returns
    /// it, the write landed and the hide has done its job.
    fn prune(&mut self) {
        // Failed writes first: a row hidden by a bet that lost comes back before anything
        // else is decided about what to draw.
        let failed: Vec<String> = std::mem::take(&mut *self.unhide.lock().unwrap());
        if !failed.is_empty() {
            self.pending.retain(|(id, _)| !failed.iter().any(|f| f == id));
            self.pending_rev = self.pending_rev.wrapping_add(1);
        }
        if self.pending.is_empty() {
            return;
        }
        let s = self.shared.lock().unwrap();
        if s.at.is_none() {
            return; // nothing has synced yet; keep hiding
        }
        let before = self.pending.len();
        // Drain entries whose expected post-write state has arrived in the snapshot.
        // Presence-based pruning fails for archived rows: the bead stays in the store
        // and only its labels change, so `present_ids` would keep it hidden forever.
        self.pending.retain(|(id, e)| !store::settled(&s, id, *e));
        if self.pending.len() != before {
            self.pending_rev = self.pending_rev.wrapping_add(1);
        }
    }

    /// What confirmation we expect after acting on the current item.
    fn expect(&self) -> store::Expect {
        if self.view.is_record() {
            // Archiving adds the label (expect archived=true); restoring removes it (expect archived=false).
            store::Expect::Archived(!self.dismissed)
        } else {
            store::Expect::Closed
        }
    }

    /// Whether this id is optimistically hidden.
    fn hidden(&self, id: &str) -> bool {
        self.pending.iter().any(|(pending_id, _)| pending_id == id)
    }

    /// Unix seconds, or whatever `PANEL_NOW` froze it at.
    fn now(&self) -> i64 {
        self.frozen_now.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        })
    }

    /// Rebuild the derived lists only when the inputs actually changed.
    ///
    /// A SILENCE EXPIRES ON THE NEXT REFRESH, not on the next frame. The clock is an input to
    /// the ALERTS list and is deliberately NOT in the cache key: keying on it would rebuild
    /// every frame, which is the per-keystroke megabyte-of-allocation cost this cache was
    /// written to remove. So an hour-long silence can run up to one refresh interval long.
    /// That is the right trade for a deadline measured in hours — and `r` forces it.
    ///
    /// Holds the store lock for the rebuild alone, never for a render.
    fn derive(&self) {
        let gen = {
            let s = self.shared.lock().unwrap();
            s.gen
        };
        if let Some(d) = self.cache.borrow().as_ref() {
            if d.gen == gen
                && d.view == self.view
                && d.dismissed == self.dismissed
                && d.pending_rev == self.pending_rev
            {
                return;
            }
        }
        let s = self.shared.lock().unwrap();
        let items = store::view_items(&s, self.view, self.dismissed, self.now()).map(|v| {
            v.into_iter()
                .filter(|i| !self.hidden(&i.id))
                .collect::<Vec<_>>()
        });
        let mut counts = [None; 4];
        for (i, v) in View::ALL.iter().enumerate() {
            counts[i] = if *v == self.view {
                items.as_ref().ok().map(|x| x.len())
            } else {
                // A view that is not on screen counts its LIVE items — the dismissed flag
                // only ever describes the view being looked at. Otherwise the FYI tab would
                // advertise a history nobody asked to see.
                store::view_items(&s, *v, false, self.now())
                    .ok()
                    .map(|x| x.iter().filter(|i| !self.hidden(&i.id)).count())
            };
        }
        *self.cache.borrow_mut() = Some(Derived {
            gen,
            view: self.view,
            dismissed: self.dismissed,
            pending_rev: self.pending_rev,
            items,
            counts,
        });
    }

    fn items(&self) -> Result<Vec<Item>, String> {
        self.derive();
        self.cache
            .borrow()
            .as_ref()
            .map(|d| d.items.clone())
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    fn current(&self) -> Option<Item> {
        self.derive();
        let c = self.cache.borrow();
        c.as_ref()?.items.as_ref().ok()?.get(self.sel).cloned()
    }

    /// NEVER BLOCK THE UI ON A WRITE. `gt mail mark-read` costs ~3.8 s and `bd close` a
    /// second or so; running either inline froze the pane for that long and navigation with
    /// it. the operator: *"it takes almost 10 seconds to get a TUI update when i type 'd' ... and i
    /// can't navigate while it's happening."* The row is hidden immediately, the write goes
    /// to a thread, and a failure puts the item back and says why.
    fn do_act(&mut self, reason: &str, what: Act) {
        let Some(it) = self.current() else { return };
        // NOT EVERY ACT REMOVES ITS ROW. Acknowledging an alert says "seen" and changes
        // nothing about the condition, so the row must stay — hiding a firing alert because
        // someone looked at it is precisely the reassuring lie this pane exists to prevent.
        let hides = model::act_hides(self.view, what);
        if hides {
            self.pending.push((it.id.clone(), self.expect()));
            self.pending_rev = self.pending_rev.wrapping_add(1);
        }
        self.flash = match what {
            Act::Silence => format!("silenced {} for an hour", it.id),
            Act::Primary => format!("{} {}", self.view.verb(self.dismissed), it.id),
        };

        let (view, reason, dismissed) = (self.view, reason.to_string(), self.dismissed);
        let now = self.now();
        let (errors, inflight, shared, unhide) = (
            Arc::clone(&self.errors),
            Arc::clone(&self.inflight),
            Arc::clone(&self.shared),
            Arc::clone(&self.unhide),
        );
        *inflight.lock().unwrap() += 1;
        thread::spawn(move || {
            if let Err(e) = act(view, &it, &reason, dismissed, what, now) {
                errors.lock().unwrap().push(format!("{}: {e}", it.id));
                // Put it back. The hide was a bet on this write, and the bet lost.
                if hides {
                    unhide.lock().unwrap().push(it.id.clone());
                }
            }
            *inflight.lock().unwrap() -= 1;
            store::refresh(&shared);
        });

        let n = self.items().map(|v| v.len()).unwrap_or(0);
        self.sel = self.sel.min(n.saturating_sub(1));
    }

    /// Archive every record in the current view at once. Only record views (Insights,
    /// Notifications) — closing every decision in one keystroke would discard verdicts with
    /// no undo.
    fn do_act_all(&mut self) {
        if !self.view.is_record() {
            self.flash = "bulk clear is only for record views".into();
            return;
        }
        let Ok(items) = self.items() else { return };
        let n = items.len();
        if n == 0 {
            return;
        }
        let ids: Vec<String> = items.iter().map(|it| it.id.clone()).collect();
        let expect = self.expect();
        for id in &ids {
            self.pending.push((id.clone(), expect));
        }
        self.pending_rev = self.pending_rev.wrapping_add(1);
        let verb = if self.dismissed { "restoring" } else { self.view.verb(false) };
        self.flash = format!("{verb} {n}…");
        let (errors, inflight, shared, view, dismissed) = (
            Arc::clone(&self.errors),
            Arc::clone(&self.inflight),
            Arc::clone(&self.shared),
            self.view,
            self.dismissed,
        );
        *inflight.lock().unwrap() += 1;
        thread::spawn(move || {
            if let Err(e) = model::act_all(view, &ids, dismissed) {
                errors.lock().unwrap().push(e);
            }
            *inflight.lock().unwrap() -= 1;
            store::refresh(&shared);
        });
        self.sel = 0;
    }

    /// Promote the selected insight into a statute.
    ///
    /// THE COMPOSE STEP IS THE FEATURE. An insight is longer than a statute by construction —
    /// it is a finding, with its evidence — and `rule.sh` refuses anything over 130 words, so
    /// the body cannot be piped through. It has to be rewritten with the source in view, and
    /// it is: the input row sits under the detail strip, which is still showing the insight
    /// being promoted.
    ///
    /// OFF THE UI THREAD like every other write, and more so. `rule.sh` shells out to
    /// `bd remember` and then runs the wiki synthesis, which rewrites the statute book as a
    /// page — seconds, not the fraction `bd update` costs. A pane frozen for that would be
    /// felt on the one key that takes a paragraph of typing to reach.
    fn do_enact(&mut self, input: &str) {
        let Some(it) = self.current() else { return };
        let (slug, text) = match model::parse_enact(input) {
            Ok(v) => v,
            Err(e) => {
                // Refused BEFORE anything was written, so nothing is hidden and nothing is
                // lost: the draft goes back in the pocket and the error says which key
                // returns it.
                *self.draft.lock().unwrap() = Some(input.to_string());
                self.flash = format!("FAILED: {e} — L restores what you typed");
                return;
            }
        };
        self.pending.push((it.id.clone(), store::Expect::Archived(true)));
        self.pending_rev = self.pending_rev.wrapping_add(1);
        self.flash = format!("enacting law-{}…", slug.trim_start_matches("law-"));

        let (errors, inflight, shared, unhide, draft) = (
            Arc::clone(&self.errors),
            Arc::clone(&self.inflight),
            Arc::clone(&self.shared),
            Arc::clone(&self.unhide),
            Arc::clone(&self.draft),
        );
        let raw = input.to_string();
        *inflight.lock().unwrap() += 1;
        thread::spawn(move || {
            match enact(&it, &slug, &text) {
                Err(e) => {
                    // The refusal verbatim — the word count and what to do about it — plus
                    // the row back and the text kept. A statute lost to a length limit
                    // nobody was shown is the "silently losing the input" this replaces.
                    errors.lock().unwrap().push(format!("{}: {e}", it.id));
                    unhide.lock().unwrap().push(it.id.clone());
                    *draft.lock().unwrap() = Some(raw);
                }
                Ok(()) => *draft.lock().unwrap() = None,
            }
            *inflight.lock().unwrap() -= 1;
            store::refresh(&shared);
        });

        let n = self.items().map(|v| v.len()).unwrap_or(0);
        self.sel = self.sel.min(n.saturating_sub(1));
    }

    /// Move a failed write's message into the flash, where it STAYS.
    ///
    /// It used to be popped inside the render and shown on that one frame. Every frame after
    /// it lost the message again — and a failure is invariably followed by another frame,
    /// because the same failure path also puts the row back and re-syncs the store. So the
    /// pane showed `FAILED: …` for one repaint and then went back to whatever the flash was:
    /// a footer actively saying the write was in flight, over a write that had already been
    /// refused. Measured while testing the >130-word refusal end to end, where a poll every
    /// 500 ms never once caught it.
    ///
    /// As a flash it persists until the next keypress clears it, which is the same lifetime
    /// every other thing the footer says has, and the footer already renders one starting
    /// with FAILED in the alert colour.
    fn drain_errors(&mut self) {
        let mut errs = self.errors.lock().unwrap();
        if let Some(e) = errs.pop() {
            self.flash = format!("FAILED: {e}");
            errs.clear(); // the newest failure is the one to answer
        }
    }

    fn do_comment(&mut self, text: &str) {
        let Some(it) = self.current() else { return };
        self.flash = format!("commented on {}", it.id);
        let (view, text) = (self.view, text.to_string());
        let (errors, inflight) = (Arc::clone(&self.errors), Arc::clone(&self.inflight));
        *inflight.lock().unwrap() += 1;
        thread::spawn(move || {
            if let Err(e) = comment(view, &it, &text) {
                errors.lock().unwrap().push(format!("{}: {e}", it.id));
            }
            *inflight.lock().unwrap() -= 1;
        });
    }

    /// Close the selected item as a premise rejection.
    ///
    /// Same optimistic-hide pattern as `do_act`: the row leaves instantly, the write runs
    /// off-thread, and a failure puts it back. The `why` is optional — an empty string is
    /// valid and records `premise-rejected` with no annotation.
    fn do_reject_premise(&mut self, why: &str) {
        let Some(it) = self.current() else { return };
        self.pending.push((it.id.clone(), store::Expect::Closed));
        self.pending_rev = self.pending_rev.wrapping_add(1);
        self.flash = format!("premise rejected: {}", it.id);
        let (errors, inflight, shared, unhide) = (
            Arc::clone(&self.errors),
            Arc::clone(&self.inflight),
            Arc::clone(&self.shared),
            Arc::clone(&self.unhide),
        );
        let why = why.to_string();
        *inflight.lock().unwrap() += 1;
        thread::spawn(move || {
            if let Err(e) = reject_premise(&it, &why) {
                errors.lock().unwrap().push(format!("{}: {e}", it.id));
                unhide.lock().unwrap().push(it.id.clone());
            }
            *inflight.lock().unwrap() -= 1;
            store::refresh(&shared);
        });
        let n = self.items().map(|v| v.len()).unwrap_or(0);
        self.sel = self.sel.min(n.saturating_sub(1));
    }

    /// Enact a law proposal from the panel.
    ///
    /// When `input` is empty, the title is used verbatim (`<slug>: <statute>`). When the
    /// operator typed an amendment (`E` key), `input` carries the edited text. The bead is
    /// closed only when `rule.sh enact` succeeds; a failed enact leaves it open and surfaces
    /// the refusal so the operator can amend and retry (law-absence-needs-a-positive-control).
    fn do_enact_law(&mut self, input: &str) {
        let Some(it) = self.current() else { return };
        let (errors, inflight, shared, unhide) = (
            Arc::clone(&self.errors),
            Arc::clone(&self.inflight),
            Arc::clone(&self.shared),
            Arc::clone(&self.unhide),
        );
        self.pending.push((it.id.clone(), store::Expect::Closed));
        self.pending_rev = self.pending_rev.wrapping_add(1);
        self.flash = format!("enacting law proposal {}…", it.id);
        let raw = input.to_string();
        let draft = Arc::clone(&self.draft);
        *inflight.lock().unwrap() += 1;
        thread::spawn(move || {
            match enact_law(&it, &raw) {
                Err(e) => {
                    errors.lock().unwrap().push(format!("{}: {e}", it.id));
                    unhide.lock().unwrap().push(it.id.clone());
                    // Keep the draft so `E` can hand it back.
                    if !raw.trim().is_empty() {
                        *draft.lock().unwrap() = Some(raw);
                    }
                }
                Ok(()) => *draft.lock().unwrap() = None,
            }
            *inflight.lock().unwrap() -= 1;
            store::refresh(&shared);
        });
        let n = self.items().map(|v| v.len()).unwrap_or(0);
        self.sel = self.sel.min(n.saturating_sub(1));
    }

    /// Decline a law proposal: close without touching the statute book.
    fn do_decline_law(&mut self, why: &str) {
        let Some(it) = self.current() else { return };
        self.pending.push((it.id.clone(), store::Expect::Closed));
        self.pending_rev = self.pending_rev.wrapping_add(1);
        self.flash = format!("declined law proposal {}", it.id);
        let (errors, inflight, shared, unhide) = (
            Arc::clone(&self.errors),
            Arc::clone(&self.inflight),
            Arc::clone(&self.shared),
            Arc::clone(&self.unhide),
        );
        let why = why.to_string();
        *inflight.lock().unwrap() += 1;
        thread::spawn(move || {
            if let Err(e) = decline_law(&it, &why) {
                errors.lock().unwrap().push(format!("{}: {e}", it.id));
                unhide.lock().unwrap().push(it.id.clone());
            }
            *inflight.lock().unwrap() -= 1;
            store::refresh(&shared);
        });
        let n = self.items().map(|v| v.len()).unwrap_or(0);
        self.sel = self.sel.min(n.saturating_sub(1));
    }

    /// Execute a lawsuit verdict (uphold / retire / amend).
    ///
    /// Same optimistic-hide pattern as `do_act`: the bead is hidden immediately, the write
    /// runs off-thread, and a failure puts it back. The distinction from `do_act` is that
    /// `suit_verdict` runs rule.sh before closing, so a rule.sh failure returns Err and the
    /// bead stays open — "A failed rule.sh leaves the bead open and says so."
    fn do_suit_verdict(&mut self, verdict: &str) {
        let Some(it) = self.current() else { return };
        self.pending.push((it.id.clone(), store::Expect::Closed));
        self.pending_rev = self.pending_rev.wrapping_add(1);
        self.flash = format!("suit verdict on {}…", it.id);
        let (errors, inflight, shared, unhide) = (
            Arc::clone(&self.errors),
            Arc::clone(&self.inflight),
            Arc::clone(&self.shared),
            Arc::clone(&self.unhide),
        );
        let verdict = verdict.to_string();
        *inflight.lock().unwrap() += 1;
        thread::spawn(move || {
            if let Err(e) = suit_verdict(&it, &verdict) {
                errors.lock().unwrap().push(format!("{}: {e}", it.id));
                unhide.lock().unwrap().push(it.id.clone());
            }
            *inflight.lock().unwrap() -= 1;
            store::refresh(&shared);
        });
        let n = self.items().map(|v| v.len()).unwrap_or(0);
        self.sel = self.sel.min(n.saturating_sub(1));
    }
}

/// The close reason `d` records, which only two of the four views have one to record.
///
/// FYI, ALERTS and NOTIFICATIONS write a label or an inbox flag and never a reason, so the
/// string is unread there; DECISIONS closes with it, and "done" is what a verdict-less close
/// has always said. Named rather than inlined because it used to be a two-arm `match` written
/// out twice — once for the list and once for the reader — which is how the two surfaces come
/// to disagree about what a key does.
fn primary_reason(view: View) -> &'static str {
    match view {
        View::Decisions => "done",
        _ => "read",
    }
}

fn main() {
    let shared: store::Shared = Arc::new(Mutex::new(store::Snapshot::default()));
    store::refresh(&shared);
    store::spawn_refresher(Arc::clone(&shared), REFRESH);

    // THE CLOCK EVERY AGE AND EVERY DEADLINE IS MEASURED AGAINST. Read per frame, and passed
    // into the render, which therefore stays a pure function of its inputs.
    //
    // It replaces a `date -u` subprocess that ran once at startup — harmless when all it
    // decided was whether a stamp printed a time or a date, and wrong for an age, which has
    // to move while the pane is open. `SystemTime` is a vDSO call, so reading it per frame
    // costs nothing where the subprocess did.
    //
    // PANEL_NOW freezes it, given either epoch seconds or an ISO stamp. `tests/fixture.json`
    // froze the DATA so a render change could be captured before and after against identical
    // rows; an age read from the wall clock would put that coin toss straight back, because
    // two captures taken a minute apart then differ for a reason that is not the change.
    // See capture.sh, which sets it.
    let frozen_now: Option<i64> = std::env::var("PANEL_NOW").ok().and_then(|v| {
        let v = v.trim();
        v.parse::<i64>().ok().or_else(|| render::epoch(v))
    });

    let mut app = App {
        shared,
        frozen_now,
        view: View::Decisions,
        sel: 0,
        mode: None,
        buf: String::new(),
        flash: String::new(),
        pending: Vec::new(),
        pending_rev: 0,
        dismissed: false,
        cache: std::cell::RefCell::new(None),
        errors: Arc::new(Mutex::new(Vec::new())),
        unhide: Arc::new(Mutex::new(Vec::new())),
        inflight: Arc::new(Mutex::new(0)),
        draft: Arc::new(Mutex::new(None)),
        reading: false,
        scroll: Scroll::new(),
        detail_scroll: Scroll::new(),
    };

    let once = std::env::args().any(|a| a == "--once");

    // `--dump` writes the live snapshot in the shape `PANEL_FIXTURE` reads back, so a
    // render change can be captured before and after against IDENTICAL data. See
    // `store::fixture` for why that is not optional.
    if std::env::args().any(|a| a == "--dump") {
        for _ in 0..200 {
            if app.shared.lock().unwrap().at.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        println!("{}", store::dump(&app.shared.lock().unwrap()));
        return;
    }

    // `--size WxH`. Without it `--once` printed a 100x16 frame while the live pane is
    // 107x19, so the thing being measured was never the thing on screen.
    let (once_w, once_h) = std::env::args()
        .skip_while(|a| a != "--size")
        .nth(1)
        .and_then(|s| {
            let (a, b) = s.split_once('x')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .unwrap_or((100usize, 16usize));

    if once {
        // Give the background fetch a moment to land rather than printing "loading…".
        for _ in 0..60 {
            if app.shared.lock().unwrap().at.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // `--sel N` starts on the Nth item, so a capture can be taken of a CHOSEN item rather
    // than whichever one happens to be at the top. Layout depends on what the selected item
    // contains — a short body hands its spare rows back to the list — so a render change
    // cannot be measured without being able to pick the item under test. It applies to the
    // interactive path too, because that is the only path that exercises the paint loop.
    if let Some(n) = std::env::args()
        .skip_while(|a| a != "--sel")
        .nth(1)
        .and_then(|v| v.parse::<usize>().ok())
    {
        for _ in 0..60 {
            if app.shared.lock().unwrap().at.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        app.derive();
        let count = app
            .cache
            .borrow()
            .as_ref()
            .and_then(|d| d.items.as_ref().ok())
            .map(|v| v.len())
            .unwrap_or(0);
        app.sel = n.min(count.saturating_sub(1));
    }

    let render_once = |app: &App, w: usize, h: usize| {
        let (age, refreshing) = {
            let s = app.shared.lock().unwrap();
            (s.age(), s.refreshing)
        };
        // The flash already carries any failure: `drain_errors` moves it there, so it
        // survives the repaints that follow a failed write instead of being consumed by the
        // first of them.
        let flash: &str = &app.flash;
        let busy = *app.inflight.lock().unwrap() > 0;
        // BORROW the derived lists; never clone them to render. `items()` hands back an
        // owned Vec<Item>, and an Item carries the full description and the whole comment
        // thread — cloning that on every frame is most of the cost this cache exists to
        // remove, and it would have made the fix look like it did nothing.
        app.derive();
        let cache = app.cache.borrow();
        let derived = match cache.as_ref() {
            Some(d) => d,
            None => return Vec::new(),
        };
        let items = &derived.items;
        if app.reading {
            if let Ok(v) = items {
                if let Some(it) = v.get(app.sel) {
                    let (lines, max) =
                        render::reader(it, app.view, app.now(), app.scroll.get(), w, h);
                    app.scroll.set_max(max);
                    return lines;
                }
            }
        }
        // CLAMP THE STATE, NOT THE VIEW. detail_lines and split are public precisely so the
        // caller can work out how far this body can actually scroll; without it every `down`
        // past the end banks an offset the renderer ignores, and the first `up` does nothing
        // — the bug the reader had, reintroduced one pane over.
        if let Ok(v) = items {
            if let Some(it) = v.get(app.sel) {
                let d = render::detail_lines(it, w);
                let ih = render::input_label(app.mode.as_deref())
                    .map_or(0, |l| render::input_h(&app.buf, l, w));
                let (_, detail_h) = render::split(h, ih, v.len(), d.len());
                app.detail_scroll.set_max(d.len().saturating_sub(detail_h));
            }
        }
        let f = Frame {
            view: app.view,
            detail_scroll: app.detail_scroll.get(),
            dismissed: app.dismissed,
            items,
            counts: derived.counts,
            sel: app.sel,
            mode: app.mode.as_deref(),
            buf: &app.buf,
            flash,
            age,
            refreshing: refreshing || busy,
            now: app.now(),
            w,
            h,
        };
        frame(&f)
    };

    if once || terminal::enable_raw_mode().is_err() {
        for l in render_once(&app, once_w, once_h) {
            println!("{l}");
        }
        return;
    }

    let mut so = stdout();
    let _ = write!(so, "\x1b[?25l");
    let _ = so.flush();
    let mut last: Vec<String> = Vec::new();

    loop {
        let (w, h) = terminal::size()
            .map(|(a, b)| (a as usize, b as usize))
            .unwrap_or((100, 16));
        // Drop optimistic hides the store has confirmed, before deciding what to draw.
        app.prune();
        // Move any failure from the background thread into the flash, where it persists
        // until the next keypress instead of being consumed by the first repaint.
        app.drain_errors();

        // DO NOT PAINT WHILE INPUT IS STILL QUEUED.
        //
        // the operator: *"when i use the arrow keys to scroll in one item's conversation, it can
        // take second(s) for the input to be reflected ... likewise for scrolling through
        // the open items themselves, and switching tabs."*
        //
        // This loop handles exactly ONE key per iteration and repaints on each. Holding an
        // arrow key delivers events far faster than a repaint completes: every frame is a
        // full-pane write plus flush, through a tmux server that is single-threaded and
        // serving twenty sessions on a loaded box. So the events queue, each one costs its
        // own round trip, and the cursor visibly crawls along behind the keyboard — which
        // is exactly what "seconds to reflect a scroll" looks like.
        //
        // Painting only when the queue is empty collapses a burst of N keypresses into ONE
        // repaint at the end of the burst. It is not throttling: every key is still handled,
        // in order, immediately. It only declines to draw a frame that is already stale.
        let more_input_queued = event::poll(Duration::ZERO).unwrap_or(false);
        if more_input_queued {
            // Fall straight through to reading it; the paint below would be discarded.
        } else {
        let mut cur = render_once(&app, w, h);
        cur.truncate(h.max(1));
        if cur != last {
            last = cur.clone();
            // NO NEWLINE AFTER THE LAST LINE. Writing h lines each followed by \r\n puts
            // h+1 rows into an h-row pane: the terminal scrolls by one and the top line —
            // the tab bar naming the current view — silently disappears. the operator: "there's no
            // header indicating which category of bead i'm currently reading".
            let mut s = String::from("\x1b[?2026h\x1b[H");
            let last_i = cur.len().saturating_sub(1);
            for (i, l) in cur.iter().enumerate() {
                s.push_str(l);
                s.push_str("\x1b[K");
                if i != last_i {
                    s.push_str("\r\n");
                }
            }
            s.push_str("\x1b[J\x1b[?2026l");
            let _ = write!(so, "{s}");
            let _ = so.flush();
        }
        }

        if !event::poll(Duration::from_millis(300)).unwrap_or(false) {
            continue;
        }
        let Ok(Event::Key(KeyEvent { code, modifiers, .. })) = event::read() else {
            continue;
        };
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            break;
        }

        if let Some(m) = app.mode.clone() {
            match code {
                KeyCode::Enter => {
                    let v = app.buf.trim().to_string();
                    // PREMISE ACCEPTS AN EMPTY WHY — the prompt says it is the training
                    // signal, so pressing ⏎ immediately is valid and records a bare
                    // "premise-rejected". Every other mode requires text before acting.
                    if m == "premise" {
                        app.do_reject_premise(&v);
                    } else if m == "decline" {
                        // decline accepts empty why — it is optional.
                        app.do_decline_law(&v);
                    } else if !v.is_empty() {
                        match m.as_str() {
                            "comment" => app.do_comment(&v),
                            "enact" => app.do_enact(&v),
                            // amend-and-enact: the edited text replaces the proposed title.
                            "amend" => app.do_enact_law(&v),
                            "suit" => app.do_suit_verdict(&v),
                            // decide and reason both close; the text is the record.
                            _ => app.do_act(&v, Act::Primary),
                        }
                    }
                    app.mode = None;
                    app.buf.clear();
                }
                KeyCode::Esc => {
                    // ESC KEEPS A STATUTE. Cancelling a one-line verdict costs a retype;
                    // cancelling a paragraph written with the insight in view costs the
                    // paragraph, and a fat-fingered esc would then be the most expensive key
                    // in the pane. `L` hands it straight back.
                    if (m == "enact" || m == "amend") && !app.buf.trim().is_empty() {
                        *app.draft.lock().unwrap() = Some(app.buf.clone());
                        app.flash = "draft kept — L to resume".into();
                    }
                    app.mode = None;
                    app.buf.clear();
                }
                KeyCode::Backspace => {
                    app.buf.pop();
                }
                KeyCode::Char(c) => app.buf.push(c),
                _ => {}
            }
            continue;
        }

        let n = app.items().map(|v| v.len()).unwrap_or(0);

        if app.reading {
            match code {
                KeyCode::Char('j') | KeyCode::Down => app.scroll.down(1),
                KeyCode::Char('k') | KeyCode::Up => app.scroll.up(1),
                KeyCode::PageDown | KeyCode::Char(' ') => app.scroll.down(10),
                KeyCode::PageUp => app.scroll.up(10),
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('o') => {
                    app.reading = false;
                    app.scroll.reset();
                }
                KeyCode::Enter if app.view == View::Decisions => {
                    // A suit bead uses a different mode: its three verdicts run rule.sh before
                    // closing. An ask-* bead closes with the typed text as the verdict. A work
                    // bead with no ask-* label gets a comment and stays open.
                    let mode = if app.current().map(|it| is_suit(&it)).unwrap_or(false) {
                        "suit"
                    } else if app.current().map(|it| model::is_ask(&it)).unwrap_or(false) {
                        "decide"
                    } else {
                        "comment"
                    };
                    app.mode = Some(mode.into());
                    app.buf.clear();
                    app.reading = false;
                    app.scroll.reset();
                }
                // ⏎ ON AN ASK IS THE DECISION — a deliberate ruling. `c` is the path for
                // someone who has read the body and wants to comment without closing (always
                // available for asks; only path for work beads), matching the list's `c comment`.
                KeyCode::Char('c') if app.view == View::Decisions => {
                    app.mode = Some("comment".into());
                    app.buf.clear();
                    app.reading = false;
                    app.scroll.reset();
                }
                // The reader's footer advertises this on an insight, and an advertised key
                // that does nothing is how a footer stops being believed.
                KeyCode::Enter if app.view == View::Insights || app.view == View::Alerts => {
                    app.mode = Some("comment".into());
                    app.buf.clear();
                    app.reading = false;
                    app.scroll.reset();
                }
                // THE THIRD ANSWER, from the surface where a long insight is actually read.
                // Leaving the reader is what puts the insight back in the detail strip, so
                // it is still on screen while the statute is written from it.
                KeyCode::Char('L') if app.view == View::Insights => {
                    app.mode = Some("enact".into());
                    app.buf = app.draft.lock().unwrap().clone().unwrap_or_default();
                    app.reading = false;
                    app.scroll.reset();
                }
                KeyCode::Char('d') => {
                    // `d` on a law proposal in DECISIONS declines it; elsewhere standard act.
                    if app.view == View::Decisions
                        && app.current().map(|it| is_ask_law(&it)).unwrap_or(false)
                    {
                        app.mode = Some("decline".into());
                        app.buf.clear();
                        app.reading = false;
                        app.scroll.reset();
                    } else {
                        app.do_act(primary_reason(app.view), Act::Primary);
                        // ACKNOWLEDGING DOES NOT CLOSE THE READER, because the item is still
                        // there — the condition has not changed and there is nothing to move on
                        // from. Every other view's `d` empties the thing being read.
                        if model::act_hides(app.view, Act::Primary) {
                            app.reading = false;
                            app.scroll.reset();
                        }
                    }
                }
                KeyCode::Char('s') if app.view == View::Alerts && !app.dismissed => {
                    app.do_act("", Act::Silence);
                    app.reading = false;
                    app.scroll.reset();
                }
                // `p` REJECTS THE PREMISE. Only in DECISIONS; inert in every other view.
                // Leaving the reader first so the item is still visible under the prompt.
                KeyCode::Char('p') if app.view == View::Decisions => {
                    app.mode = Some("premise".into());
                    app.buf.clear();
                    app.reading = false;
                    app.scroll.reset();
                }
                // `e` ENACTS from the reader too — the reader is where long statute text is read.
                KeyCode::Char('e') if app.view == View::Decisions => {
                    if app.current().map(|it| is_ask_law(&it)).unwrap_or(false) {
                        app.reading = false;
                        app.scroll.reset();
                        app.do_enact_law("");
                    }
                }
                // `E` AMEND-AND-ENACT from the reader.
                KeyCode::Char('E') if app.view == View::Decisions => {
                    if let Some(it) = app.current() {
                        if is_ask_law(&it) {
                            app.mode = Some("amend".into());
                            app.buf = app.draft.lock().unwrap().clone().unwrap_or_else(|| it.title.clone());
                            app.reading = false;
                            app.scroll.reset();
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        let switch = |a: &mut App, v: View| {
            a.view = v;
            a.sel = 0;
            a.flash.clear();
            // Leaving FYI forgets its history filter. A filter that outlives the view it
            // belongs to is invisible when you come back, and the tab count is then a number
            // about a set nobody asked for.
            a.dismissed = false;
        };
        match code {
            // THE ARROWS SCROLL THE BODY, `j`/`k` CHANGE THE ITEM. Reading a long bead used
            // to mean pressing `o` first, which made a mode out of something that should
            // just be a key. Both are available at once now, and neither needs a view.
            KeyCode::Down => app.detail_scroll.down(1),
            KeyCode::Up => app.detail_scroll.up(1),
            KeyCode::PageDown | KeyCode::Char(' ') => app.detail_scroll.down(10),
            KeyCode::PageUp => app.detail_scroll.up(10),
            KeyCode::Char('j') if n > 0 => {
                app.sel = (app.sel + 1).min(n - 1);
                app.detail_scroll.reset();
                app.flash.clear();
            }
            KeyCode::Char('k') => {
                app.sel = app.sel.saturating_sub(1);
                app.detail_scroll.reset();
                app.flash.clear();
            }
            // NO NUMERIC JUMP KEYS. the operator: *"i don't need numeric jump hotkeys for the
            // tabs; just the tab button cycling through them ... is sufficient."* Three
            // views is few enough that cycling always beats aiming, and the numbers were
            // costing a line of footer to advertise. Tab goes left to right, Shift+Tab back,
            // which is the convention everywhere else.
            KeyCode::Tab => {
                let v = app.view.next();
                switch(&mut app, v);
            }
            KeyCode::BackTab => {
                let v = app.view.prev();
                switch(&mut app, v);
            }
            KeyCode::Char('o') if n > 0 => {
                app.reading = true;
                app.scroll.reset();
            }
            // `h` — the retrieval half of dismissal. Only in record views and ALERTS:
            // DECISIONS has no dismissed set — a closed decision is a verdict, not a hidden row.
            KeyCode::Char('h') if app.view.has_history() => {
                app.dismissed = !app.dismissed;
                app.sel = 0;
                app.detail_scroll.reset();
                app.flash = match (app.dismissed, app.view) {
                    (false, _) => String::new(),
                    (true, View::Alerts) => "cleared and silenced — d lifts a silence".into(),
                    (true, _) => "showing dismissed — d restores".into(),
                };
            }
            KeyCode::Char('r') => {
                app.pending.clear();
                app.pending_rev = app.pending_rev.wrapping_add(1);
                app.flash.clear();
                store::refresh(&app.shared);
            }
            KeyCode::Char('d') | KeyCode::Char('x') if n > 0 => {
                // `d` on a law proposal in DECISIONS opens the decline prompt (optional why).
                // Every other view uses the standard act path.
                if app.view == View::Decisions
                    && app.current().map(|it| is_ask_law(&it)).unwrap_or(false)
                {
                    app.mode = Some("decline".into());
                    app.buf.clear();
                } else {
                    app.do_act(primary_reason(app.view), Act::Primary);
                }
            }
            // `s` — SILENCE, and only on a firing alert. Silencing something that has already
            // cleared is a key that appears to work on the wrong thing, and the history holds
            // both kinds side by side.
            KeyCode::Char('s') if n > 0 && app.view == View::Alerts && !app.dismissed => {
                app.do_act("", Act::Silence)
            }
            KeyCode::Char('A') if n > 0 => app.do_act_all(),
            // `D` PROMPTS FOR A REASON AND THEN CLOSES — which is why it is not bound in
            // ALERTS. Nothing in this pane may close an alert: the condition is retracted by
            // whatever asserted it, and a hand-closed one whose condition is still true is
            // straight back on the next pass.
            KeyCode::Char('D') | KeyCode::Char('X') if n > 0 && app.view != View::Alerts => {
                app.mode = Some("reason".into());
                app.buf.clear();
            }
            // ⏎ ON AN ASK IS THE DECISION. Typing an answer to a question IS answering
            // it — the close reason IS the verdict. For a suit, the verdict runs rule.sh
            // against the statute book before closing. For a work bead with no ask-* label,
            // ⏎ records a comment and leaves the bead open.
            KeyCode::Enter if n > 0 && app.view == View::Decisions => {
                let mode = if app.current().map(|it| is_suit(&it)).unwrap_or(false) {
                    "suit"
                } else if app.current().map(|it| model::is_ask(&it)).unwrap_or(false) {
                    "decide"
                } else {
                    "comment"
                };
                app.mode = Some(mode.into());
                app.buf.clear();
            }
            KeyCode::Enter if n > 0 && (app.view == View::Insights || app.view == View::Alerts) => {
                app.mode = Some("comment".into());
                app.buf.clear();
            }
            KeyCode::Char('c') if n > 0 && app.view != View::Notifications => {
                app.mode = Some("comment".into());
                app.buf.clear();
            }
            // `L` — ENACT. THE FYI VIEW ONLY, and the guard is the view rather than the
            // item: `L` in DECISIONS or NOTIFICATIONS does nothing, because a statute cites
            // the case that produced it and neither of those is one.
            //
            // The draft comes back if there is one, so a refusal — over 130 words, almost
            // always — is edited rather than retyped.
            KeyCode::Char('L') if n > 0 && app.view == View::Insights => {
                app.mode = Some("enact".into());
                app.buf = app.draft.lock().unwrap().clone().unwrap_or_default();
            }
            // `a` ACCEPTS THE RECOMMENDED DEFAULT, with no typing.
            //
            // the operator: *"when there is a default decision, i would like an easy way to accept
            // that default (right now i can SEE the default, but i still have to type
            // something)."* Every ask is supposed to carry a recommendation precisely so the
            // operator does not have to decide from scratch — and then the pane made them
            // restate it in their own words to agree with it. The whole point of a default is that
            // agreeing should cost one keystroke.
            //
            // The verdict recorded is the default's own text, not the word "accepted", so
            // the close reason still reads as a decision months later rather than as a
            // pointer to a sentence nobody kept.
            KeyCode::Char('a') if n > 0 && app.view == View::Decisions => {
                match app.current() {
                    Some(it) if !it.lead.is_empty() => {
                        let verdict = format!("{} (accepted the recommended default)", it.lead);
                        app.do_act(&verdict, Act::Primary);
                    }
                    _ => app.flash = "no default on this one — ⏎ to decide".into(),
                }
            }
            // `p` REJECTS THE PREMISE. Only in DECISIONS — inert in FYI, NOTIFICATIONS and
            // ALERTS. Prompts for a short why; the prompt says it is the training signal.
            KeyCode::Char('p') if n > 0 && app.view == View::Decisions => {
                app.mode = Some("premise".into());
                app.buf.clear();
            }
            // `e` ENACTS A LAW PROPOSAL. Only when the selected item is a law proposal —
            // the key is inert on ordinary decisions so it cannot be reached by accident
            // on the wrong bead. Runs rule.sh enact with the title as input; a failed
            // enact leaves the bead OPEN and surfaces the refusal verbatim.
            KeyCode::Char('e') if n > 0 && app.view == View::Decisions => {
                match app.current() {
                    Some(it) if is_ask_law(&it) => app.do_enact_law(""),
                    _ => app.flash = "e only enacts law proposals — use ⏎ for verdicts".into(),
                }
            }
            // `E` AMEND-AND-ENACT. Opens the editor pre-filled with the proposed title so
            // the operator can edit the slug or statute text before enacting.
            KeyCode::Char('E') if n > 0 && app.view == View::Decisions => {
                match app.current() {
                    Some(it) if is_ask_law(&it) => {
                        app.mode = Some("amend".into());
                        // Pre-fill with the proposed text so the operator edits rather than retyping.
                        app.buf = app.draft.lock().unwrap().clone().unwrap_or_else(|| it.title.clone());
                    }
                    _ => app.flash = "E only amends law proposals — use ⏎ for verdicts".into(),
                }
            }
            _ => {}
        }
    }

    let _ = terminal::disable_raw_mode();
    let _ = write!(so, "\x1b[?25h\x1b[?2026l\r\n");
    let _ = so.flush();
}

#[cfg(test)]
mod tests {
    use super::Scroll;

    /// The bead's acceptance criterion, verbatim: from the bottom, 20 Downs then one Up
    /// scrolls up by one line.
    #[test]
    fn down_past_the_end_banks_nothing() {
        let s = Scroll::new();
        s.set_max(5);
        for _ in 0..20 {
            s.down(1);
        }
        assert_eq!(s.get(), 5, "Down must stop at the last frame's bound");
        s.up(1);
        assert_eq!(s.get(), 4, "one Up from the bottom moves one line");
    }

    /// The paint loop skips frames while input is queued, so a whole burst can land against
    /// a stale bound. The next frame's `set_max` must pull the position back in.
    #[test]
    fn a_burst_against_an_unknown_bound_is_repaired_by_the_next_frame() {
        let s = Scroll::new();
        for _ in 0..20 {
            s.down(1);
        }
        assert_eq!(s.get(), 20, "an unknown bound must not clamp");
        s.set_max(5);
        assert_eq!(s.get(), 5);
        s.up(1);
        assert_eq!(s.get(), 4);
    }

    #[test]
    fn page_keys_clamp_at_both_ends() {
        let s = Scroll::new();
        s.set_max(7);
        s.down(10);
        assert_eq!(s.get(), 7);
        s.up(10);
        assert_eq!(s.get(), 0);
        s.up(10);
        assert_eq!(s.get(), 0, "Up at the top must not underflow");
    }

    /// A body that fits the pane has max 0: every key is a no-op, not a banked offset.
    #[test]
    fn a_body_that_fits_never_scrolls() {
        let s = Scroll::new();
        s.set_max(0);
        s.down(1);
        s.down(10);
        assert_eq!(s.get(), 0);
    }

    /// Leaving the reader forgets the bound; the next item's is measured, not inherited.
    #[test]
    fn reset_forgets_the_previous_items_bound() {
        let s = Scroll::new();
        s.set_max(2);
        s.down(9);
        assert_eq!(s.get(), 2);
        s.reset();
        assert_eq!(s.get(), 0);
        s.down(9);
        assert_eq!(s.get(), 9, "a longer item must not be capped by a shorter one");
    }
}
