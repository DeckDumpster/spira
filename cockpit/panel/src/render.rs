//! The panel's look: numbered tabs, a scannable list, and a permanent detail area.
//!
//! MASTER / DETAIL, not an expanding list. The pane is wide and short — about 107x19 — so a
//! list that grows inline when you select something pushes everything else off screen and
//! makes the list unscannable. Instead the list stays one line per item, always, and the
//! bottom third is a fixed area showing the selected item in full. Nothing moves as you
//! arrow through; only the detail changes. There is no expand key because there is nothing
//! to expand.
//!
//! TABS ARE NUMBERED because they are jumpable — 1, 2, 3 — and the number is the key.
//!
//! THE HEADER SAYS HOW OLD THE DATA IS. The store refreshes on a timer, so the panel is
//! always showing something slightly stale; saying so is the difference between a display
//! and a claim. It also says when a refresh is in flight, so a slow `bd` looks like work
//! rather than a freeze.
//!
//! FLICKER. Never clear the screen: home the cursor, erase per line as it is rewritten, and
//! skip the write entirely when the frame is unchanged. That is why every age here is
//! rendered in coarse buckets rather than exact seconds — an exact clock would change every
//! second and repaint the whole pane forever.
//!
//! AGE, NOT WALLCLOCK. Every stamp on an item says how long it has been OPEN — "26m", "7h",
//! "6d" — because that is the question this pane is asked. (the operator, verbatim: *"i want
//! to know how long the item has been open, not the wallclock time it was opened."*) A
//! wallclock time answers it only for someone holding the current time and willing to
//! subtract, and the format it replaced switched units at midnight — today's rows read
//! "16:04" and older ones "Sep02" — so the two rows most worth comparing were the two
//! rendered in incomparable notations. The absolute timestamp stays on the rule and in the
//! reader's header, BEHIND the age, because rows are cut from the right.
//!
//! ── LEGIBILITY ────────────────────────────────────────────────────────────────────────
//!
//! This pane is nineteen rows tall and it is where every decision is read, so three rules
//! govern every colour and every row spent here. the operator: *"it's hard to read (small space,
//! poor coloring, bad framing)."*
//!
//! 1. CONTENT IS NEVER DIM. The body of a bead was rendered in `\x1b[2m` — faint grey on
//!    black, the least readable thing on the screen — while the chrome around it was full
//!    brightness. That is the contrast budget spent backwards. Body text is `TXT`, bright
//!    white; `MUT` and the band backgrounds are for chrome, and nothing else.
//!
//! 2. THE FRAME IS TWO BANDS. The header and the footer sit on a filled background and
//!    nothing else in the pane does. Everything between them is content, by construction,
//!    and the eye finds the boundary without reading a word. Before this the tabs, the
//!    list and the key hints all rendered as text on the same black.
//!
//! 3. EVERY ROW IS EARNED. Three of nineteen rows are chrome and that is the whole budget:
//!    the rule between list and detail also NAMES the selected item, so the id / badge /
//!    timestamp line the detail used to spend a row on is free. And when the detail needs
//!    fewer rows than it was given, the surplus goes back to the list instead of being
//!    painted as blank lines under a short body.

use crate::model::{Item, View};

pub const RST: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const OK: &str = "\x1b[92m";
pub const BAD: &str = "\x1b[91m";
pub const ACC: &str = "\x1b[96m";

/// Content text. Bright white, never DIM — see rule 1 above.
pub const TXT: &str = "\x1b[97m";
/// Genuinely secondary chrome inside the content area: rules, stamps, badges.
pub const MUT: &str = "\x1b[90m";
/// The chrome band: header and footer, and nothing else.
pub const BAR: &str = "\x1b[100;37m";
/// A key letter sitting on the band. Sets the foreground only, so the band's background
/// survives; emit `BAR` again afterwards to get the band's foreground back.
pub const KEY: &str = "\x1b[96m";
/// The current thing — the active tab, the selected row. Black on white, stated rather than
/// reverse-video, so it looks the same on a band as it does on black.
pub const SEL: &str = "\x1b[107;30;1m";
/// A failure the operator must see. A full-width red band, because a failed write used to
/// render as one more line of grey key hints.
pub const ALERT: &str = "\x1b[41;97;1m";
/// A transient notice that is not a failure.
pub const NOTE: &str = "\x1b[43;30m";
/// Emphasis that sets no background, so it is legible on black AND inside a band. Used for
/// the one thing on a chrome row that is content: how long the item has been open.
pub const HI: &str = "\x1b[1;97m";

pub fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(20);
    let (mut out, mut line) = (Vec::new(), String::new());
    for w in s.split_whitespace() {
        let n = w.chars().count();
        if !line.is_empty() && line.chars().count() + 1 + n > width {
            out.push(std::mem::take(&mut line));
        }
        if n > width {
            let mut rest: Vec<char> = w.chars().collect();
            while rest.len() > width {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                out.push(rest[..width].iter().collect());
                rest = rest[width..].to_vec();
            }
            line = rest.into_iter().collect();
            continue;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(w);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Coarse buckets, never exact seconds — an exact clock repaints the pane every second and
/// defeats the unchanged-frame check that keeps it from flickering.
fn age_text(age: Option<u64>, refreshing: bool) -> String {
    if refreshing {
        return "syncing".into();
    }
    match age {
        None => "no data".into(),
        Some(s) if s < 20 => "live".into(),
        Some(s) if s < 60 => "<1m".into(),
        Some(s) => format!("{}m", s / 60),
    }
}

/// "2026-09-05T05:41:00Z" -> unix seconds. `None` for anything short of a full stamp.
///
/// Hand-rolled rather than taking a date crate: this pane ships as one static binary into a
/// tmux pane, and the whole of what it wants from a date library is days-from-civil. `bd`
/// and `gt mail` both write UTC with a trailing `Z` (checked against both), but a
/// numeric offset is honoured anyway — an unhandled `+02:00` would not fail, it would render
/// an age wrong by two hours, and a number that is quietly wrong is worse on this surface
/// than a blank one.
pub fn epoch(iso: &str) -> Option<i64> {
    if iso.len() < 19 {
        return None;
    }
    let f = |a: usize, b: usize| iso.get(a..b)?.parse::<i64>().ok();
    let (year, mon, day) = (f(0, 4)?, f(5, 7)?, f(8, 10)?);
    let (hh, mm, ss) = (f(11, 13)?, f(14, 16)?, f(17, 19)?);
    // Howard Hinnant's days_from_civil, exact for every proleptic-Gregorian date.
    let y = year - i64::from(mon <= 2);
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (mon + if mon > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let secs = (era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss;
    // A trailing offset, if there is one: "+02:00" means the stamp runs two hours AHEAD of
    // UTC, so it is subtracted to get back to UTC.
    let off = match iso.as_bytes().get(19) {
        Some(&c @ b'+') | Some(&c @ b'-') => {
            let sign = if c == b'-' { -1 } else { 1 };
            sign * (f(20, 22).unwrap_or(0) * 3600 + f(23, 25).unwrap_or(0) * 60)
        }
        _ => 0,
    };
    Some(secs - off)
}

/// Unix seconds -> "2026-09-05T16:20:00Z". The exact inverse of `epoch`.
///
/// It exists because a silence carries its own deadline in a label, and that deadline has to
/// be readable in `bd show`, in the JSONL export and on the phone — an epoch integer there is
/// a number nobody can check. Hand-rolled for the same reason `epoch` is: this pane ships as
/// one static binary and days-from-civil is the whole of what it wants from a date library.
///
/// Round-tripping is asserted against `epoch`, and `epoch` itself is asserted against the
/// system `date` — an arithmetic that agrees only with its own inverse can be wrong in both
/// directions at once and still pass.
pub fn iso(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + i64::from(m <= 2);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// How long ago `iso` was, coarsely: "now", "26m", "7h", "6d". Blank if there is no stamp.
///
/// THIS IS THE QUESTION THE PANE IS ASKED. (the operator, verbatim: *"i want to know how long
/// the item has been open, not the wallclock time it was opened."*) The age is what says a
/// thing is rotting; the wallclock time it arrived says that only to a reader doing
/// arithmetic against a clock that is not on screen.
///
/// COARSE, AND THAT IS THE FLICKER RULE, NOT LAZINESS. The pane writes a frame only when it
/// differs from the last one, so anything that ticks is a repaint. Minutes below an hour,
/// hours below a day, days above it means a row's text changes at most once a minute, and
/// only while an item is under an hour old — the same cadence the header's own staleness
/// readout has always run at. Seconds would repaint the whole pane forever.
///
/// `now` is a parameter rather than a call, so the frame stays a pure function of its
/// inputs: a capture of the frozen fixture must not differ because it was taken a minute
/// later. `PANEL_NOW` is the other half of that.
pub fn age_since(iso: &str, now: i64) -> String {
    let Some(t) = epoch(iso) else {
        return String::new();
    };
    // A stamp in the future is two clocks disagreeing, not a negative age. Clamping renders
    // "now"; the alternative renders "-3m", which reads as a defect and informs no one.
    match (now - t).max(0) {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", (s / 86_400).min(999)),
    }
}

/// "3d old · 2026-09-05T05:41" — the age first, the absolute timestamp behind it.
///
/// Wherever there is horizontal room both are shown. The age is the question; the timestamp
/// is what correlates a bead with a log line, so it stays reachable rather than being
/// dropped. Their ORDER is a precedence rule rather than a preference: `fit` cuts every row
/// from the right, so a pane too narrow for both loses the timestamp and keeps the age.
///
/// `dim` is the colour to return to after the age — `MUT` on the rule, `BAR` inside the
/// header band, whose background has to be re-emitted after any escape.
fn age_and_stamp(it: &Item, now: i64, dim: &str) -> String {
    let ago = age_since(&it.when, now);
    let ts = when16(&it.when);
    match (ago.is_empty(), ts.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!("{}{}", dim, ts),
        (false, true) => format!("{}{} old{}", HI, ago, dim),
        (false, false) => format!("{}{} old{} · {}", HI, ago, dim, ts),
    }
}

/// The first sixteen characters of an ISO stamp — "2026-09-05T05:41" — or blank.
fn when16(iso: &str) -> &str {
    if iso.len() >= 16 {
        &iso[..16]
    } else {
        ""
    }
}

/// Truncate to `w` VISIBLE columns, passing escapes through untouched.
///
/// The last line written to the pane must not be wider than the pane. An over-long row wraps,
/// the terminal scrolls by one, and the top row — the tab bar naming the view the operator is reading
/// — silently disappears; the same failure as writing h+1 lines into an h-row pane, which is
/// already commented in `main`. It was reachable: the footer's key hints run to 64 columns and
/// were never measured against the width.
fn fit(s: &str, w: usize) -> String {
    if strip_len(s) <= w {
        return s.to_string();
    }
    let (mut out, mut seen, mut esc) = (String::new(), 0, false);
    for c in s.chars() {
        if esc {
            out.push(c);
            esc = c != 'm';
            continue;
        }
        if c == '\x1b' {
            out.push(c);
            esc = true;
            continue;
        }
        if seen == w {
            break;
        }
        out.push(c);
        seen += 1;
    }
    out.push_str(RST);
    out
}

/// A chrome band filling the full width. `s` may carry escapes; `band` is re-emitted by the
/// caller after any of them (see `KEY`).
fn band(colour: &str, s: &str, w: usize) -> String {
    let pad = w.saturating_sub(strip_len(s));
    fit(&format!("{}{}{}{}", colour, s, " ".repeat(pad), RST), w)
}

/// A rule that NAMES what is beneath it — rule 3, the same trick the list/detail rule plays.
fn rule(label: &str, w: usize) -> String {
    let tag = format!(" {}{}{} ", MUT, label, RST);
    let fill = w.saturating_sub(2 + strip_len(&tag));
    format!("{}──{}{}{}{}", MUT, tag, MUT, "─".repeat(fill), RST)
}

/// What the rule beneath the thread names, which is not the same sentence in every view.
///
/// On a decision the body IS the ask, so "the question" / "the decision" stops the ask from
/// reading as one more reply. On an insight there is no ask: the body is the reason the
/// finding was worth keeping, and calling it "the insight" invites the reader to answer it.
/// (the operator, verbatim: insights "should be FYI only".)
fn section(it: &Item) -> &'static str {
    if it.badge == "insight" {
        "why it matters"
    } else if it.badge.starts_with("alert") {
        // Not "the alert", which names the message; the body says what is TRUE, and it is a
        // condition rather than a request. Prefix-matched because the badge carries the flap
        // count — "alert ×3".
        "the condition"
    } else if it.badge == "law proposal" {
        "the statute"
    } else if it.badge == "decision" {
        "the decision"
    } else if it.badge == "task" {
        "the task"
    } else {
        "the question"
    }
}

/// The statute this insight became, or nothing.
///
/// It reads as a citation rather than as a status — `enacted: law-closed-is-not-landed` is
/// addressable: it is the exact argument `rule.sh show` takes, so the rule can be read from
/// the case in one step. Shaped like the `default:` line because it plays the same part: the
/// one thing about this item worth seeing before its body.
fn enacted_line(it: &Item, w: usize) -> Option<String> {
    let key = it.enacted.as_ref()?;
    Some(format!(
        " {}{}enacted:{} {}{}{}",
        BOLD,
        ACC,
        RST,
        TXT,
        clip(key, w.saturating_sub(12)),
        RST
    ))
}

/// The comment thread, oldest first, ready to place above the body.
///
/// ORDER: THE THREAD COMES FIRST, THE ORIGINAL ASK BENEATH IT. (the operator, verbatim: *"i
/// want threaded replies at the top."*) The thread is the newest and most decision-relevant
/// content in the item — the ask is what was already read once — and it used to sit under a
/// body that routinely runs past the fold, so the freshest thing on the item was the one
/// furthest from the eye in a pane already short on rows.
///
/// WITHIN the thread, NEWEST FIRST. (the operator, verbatim: *"i also want to see the threaded
/// responses from newest-to-oldest so i don't have to scroll endlessly to see the
/// current/last state of the conversation"*). A long thread is read to find out where the
/// conversation got to, and oldest-first put that answer at the bottom of a pane that is
/// already short on rows — so the one line they needed was the one they had to scroll for.
///
/// THE STORE ORDER IS UNCHANGED, and deliberately so: `store::oldest_first` stays canonical
/// because the list's turn marker asks `thread.last()` whose ball it is — `↩` for me, `●` for
/// the operator — and that inverts outright if the newest entry moves to the front. Reversing here,
/// at the point of drawing, gives the reading order without touching the semantics anything
/// else depends on.
///
/// Shared by the reader and the detail strip so the two surfaces cannot drift into
/// disagreeing about the order — the previous version had the layout written out once, and
/// the strip simply had no thread at all.
fn thread_lines(it: &Item, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    for (author, when, text) in it.thread.iter().rev() {
        let mine = author == crate::model::ME;
        let who = if mine { "me" } else { author.as_str() };
        let colour = if mine { ACC } else { OK };
        out.push(format!(
            " {}{}{}{} {}{}",
            colour,
            who,
            RST,
            MUT,
            when16(when),
            RST
        ));
        for para in text.split('\n') {
            if para.trim().is_empty() {
                out.push(String::new());
            } else {
                for l in wrap(para, w.saturating_sub(4)) {
                    out.push(format!("   {}{}{}", TXT, l, RST));
                }
            }
        }
        // The blank an entry ends with is also the gap above whatever follows the thread.
        out.push(String::new());
    }
    out
}

/// Full-pane reader. The 4-line detail strip is for glancing; a decision whose body runs to
/// twenty lines cannot be made from a glance. the operator: "i can't read the entire goddamn text of
/// the bead ... how am i supposed to make a decision when i can't see the information."
///
/// So reading gets the whole pane and scrolls. The list is for choosing what to read; this
/// is for reading it.
///
/// Returns the frame AND the largest scroll offset that shows anything new. The caller must
/// write that number back onto the scroll state, because only this function knows how long
/// the body is: it is built here, from a wrap width the caller does not have. Clamping the
/// view alone is what let Down bank offsets past the end that Up then had to burn off one
/// keypress at a time (the operator, verbatim: *"if i am at the bottom and keep pressing down,
/// i have to press up that many times in order to start scrolling up."*).
pub fn reader(
    it: &Item,
    view: View,
    now: i64,
    scroll: usize,
    w: usize,
    h: usize,
) -> (Vec<String>, usize) {
    let mut out = vec![band(
        BAR,
        &format!(
            "{} {} {}{}  {}  {}",
            SEL,
            it.id,
            RST,
            BAR,
            it.badge,
            age_and_stamp(it, now, BAR)
        ),
        w,
    )];
    let mut body: Vec<String> = Vec::new();
    for l in wrap(&it.title, w.saturating_sub(2)) {
        body.push(format!(" {}{}{}{}", BOLD, TXT, l, RST));
    }
    body.extend(enacted_line(it, w));
    body.push(String::new());
    // THE THREAD, ABOVE THE ASK. This is the conversation, and it belongs where the decision
    // is read — not in a chat transcript the operator would have to go and find, and not under a
    // body long enough to push it off the first screen. See `thread_lines`.
    let threaded = !it.thread.is_empty();
    body.extend(thread_lines(it, w));
    // The rule names what is beneath it, so the ask cannot read as one more reply. It costs
    // a row only when there is a thread AND an ask to separate; the blank above it is the
    // one the last comment ended with, not a second row.
    if threaded && !it.body.is_empty() {
        body.push(rule(section(it), w));
    }
    // Preserve the source's own paragraph breaks; a wall of reflowed text is its own kind
    // of unreadable.
    for para in it.body.split('\n') {
        if para.trim().is_empty() {
            body.push(String::new());
        } else {
            for l in wrap(para, w.saturating_sub(2)) {
                body.push(format!(" {}{}{}", TXT, l, RST));
            }
        }
    }

    let room = h.saturating_sub(2);
    let max_scroll = body.len().saturating_sub(room);
    let start = scroll.min(max_scroll);
    out.extend(body[start..body.len().min(start + room)].iter().cloned());
    while out.len() < h.saturating_sub(1) {
        out.push(String::new());
    }
    let more = if body.len() > room {
        format!("{}-{} of {}", start + 1, (start + room).min(body.len()), body.len())
    } else {
        "all".into()
    };
    // ⏎ IS NOT "DECIDE" EVERYWHERE. It was advertised as such on every item in every view,
    // including insights — where the key is not even bound, so the one affordance saying
    // loudest that a record wanted an answer also did nothing when pressed. It now names what
    // ⏎ actually does here, and on a notification it names nothing because ⏎ does nothing.
    let is_law = crate::model::is_ask_law(it);
    let enter = match view {
        View::Decisions if is_law => format!(
            " · {}e{} enact · {}E{} amend+enact · {}d{} decline · {}c{} comment",
            KEY, BAR, KEY, BAR, KEY, BAR, KEY, BAR
        ),
        View::Decisions => format!(
            " · {}⏎{} decide · {}p{} reject · {}c{} comment",
            KEY, BAR, KEY, BAR, KEY, BAR
        ),
        // `L` is offered wherever it is bound, and it is bound here: the reader is where a
        // long insight is actually read, so it is where the question "should this be law?"
        // is answered. A key that works but is advertised on only one of two surfaces is
        // one nobody finds.
        View::Insights => format!(" · {}⏎{} comment · {}L{} enact", KEY, BAR, KEY, BAR),
        // An alert takes a comment and never a verdict. There is nothing to decide here:
        // the condition clears when it clears.
        View::Alerts => format!(" · {}⏎{} comment", KEY, BAR),
        View::Notifications => String::new(),
    };
    out.push(band(
        BAR,
        &format!(
            " {} · {}j/k{} scroll{} · {}esc{} back",
            more, KEY, BAR, enter, KEY, BAR
        ),
        w,
    ));
    // Exactly h rows, each no wider than w. A pane too small for the layout's own floor
    // loses its bottom rows here rather than scrolling its top row away.
    out.truncate(h);
    for l in out.iter_mut() {
        *l = fit(l, w);
    }
    (out, max_scroll)
}

pub struct Frame<'a> {
    pub view: View,
    /// The FYI view is showing the insights already dismissed, not the live ones.
    ///
    /// It renames the tab rather than adding chrome: a filter you cannot see is a pane
    /// lying about how much is waiting, and this pane's whole job is that number.
    pub dismissed: bool,
    pub items: &'a Result<Vec<Item>, String>,
    pub counts: [Option<usize>; 4],
    pub sel: usize,
    pub mode: Option<&'a str>,
    pub buf: &'a str,
    pub flash: &'a str,
    pub age: Option<u64>,
    pub refreshing: bool,
    /// How far the DETAIL pane is scrolled, in lines.
    ///
    /// Scrolling a body used to require entering the reader with `o`, which made a mode out
    /// of something that should just be a key: the body is the thing you read, and needing
    /// to open a view to move through it is a step that earns nothing. `j`/`k` still move
    /// between items; the arrows move within one.
    ///
    /// Clamped by the caller against `detail_lines().len()` and `split()`, both public for
    /// that reason — an unclamped offset banks dead scroll and the first `up` does nothing,
    /// which is exactly the bug the reader had.
    pub detail_scroll: usize,
    /// Unix seconds, read once per frame. Every age on screen is measured against it.
    pub now: i64,
    pub w: usize,
    pub h: usize,
}

/// The selected item's content, in reading order, with no chrome and no trailing blanks.
///
/// Built BEFORE the pane is divided, because how many rows it actually needs decides how
/// many the list gets. It carries no id/badge/timestamp line: that lives on the rule, which
/// the layout is paying for anyway.
///
/// TRAILING BLANKS ARE DROPPED, and that is load-bearing rather than tidiness — a blank
/// counted as content would claim a row the list could have used, which is the exact defect
/// this measurement exists to remove.
pub fn detail_lines(it: &Item, w: usize) -> Vec<String> {
    let mut d: Vec<String> = Vec::new();
    for l in wrap(&it.title, w.saturating_sub(3)) {
        d.push(format!(" {}{}{}{}", BOLD, TXT, l, RST));
    }
    d.extend(enacted_line(it, w));
    if !it.lead.is_empty() {
        for (n, l) in wrap(&it.lead, w.saturating_sub(12)).iter().enumerate() {
            d.push(format!(
                " {}{}{}{} {}{}{}",
                BOLD,
                ACC,
                if n == 0 { "default:" } else { "        " },
                RST,
                TXT,
                l,
                RST
            ));
        }
    }
    // THEN THE THREAD, and only then the ask — the same order as the reader, from the same
    // builder. The strip carried no thread at all before this, so the most recent thing said
    // about an item was invisible on the surface the operator actually glances at; if the thread
    // pushes the ask below the fold that is the right trade, because the ask is what they have
    // already read and `⏎` opens the reader for it.
    //
    // No naming rule here, unlike the reader: a rule is a row, the strip is four to ten rows
    // deep, and the indent already tells a reply (three columns) from the ask (one).
    let threaded = !it.thread.is_empty();
    if threaded && !it.lead.is_empty() {
        d.push(String::new());
    }
    d.extend(thread_lines(it, w));
    // The body follows the default rather than being replaced by it. Previously a bead with
    // a recommended default showed ONLY that line, so enlarging this area would have bought
    // blank rows — the space has to have something to put in it.
    if !it.body.is_empty() {
        if !it.lead.is_empty() && !threaded {
            d.push(String::new());
        }
        for para in it.body.split('\n') {
            if para.trim().is_empty() {
                d.push(String::new());
            } else {
                for l in wrap(para, w.saturating_sub(3)) {
                    d.push(format!(" {}{}{}", TXT, l, RST));
                }
            }
        }
    }
    while d.last().map(|l| l.is_empty()).unwrap_or(false) {
        d.pop();
    }
    d
}

/// The most rows the reply editor may take. Four is enough for a considered verdict without
/// the queue above it collapsing; past that the view scrolls with the cursor.
pub const INPUT_MAX_ROWS: usize = 4;

/// The reply buffer wrapped to the pane, as the rows that should be drawn.
///
/// THE CURSOR IS ALWAYS ON SCREEN. render.rs pushed the whole buffer as one row and `fit`
/// truncated it at the pane edge, so past roughly one line the operator was typing blind into
/// the one place on this pane where they write rather than read (the operator, verbatim: *"after
/// that threshold i can't see what i'm typing"*). When the answer outgrows INPUT_MAX_ROWS this
/// keeps the LAST rows rather than the first: what matters is the words being typed now.
pub fn input_rows(buf: &str, label: &str, w: usize) -> Vec<String> {
    let indent = label.chars().count() + 3; // " label▸ "
    let avail = w.saturating_sub(indent).max(8);
    // The cursor block occupies a cell, so it wraps like any other character.
    let mut cells: Vec<char> = buf.chars().collect();
    cells.push('█');
    let mut rows: Vec<String> = cells
        .chunks(avail)
        .map(|c| c.iter().collect::<String>())
        .collect();
    if rows.is_empty() {
        rows.push(String::from("█"));
    }
    if rows.len() > INPUT_MAX_ROWS {
        rows = rows.split_off(rows.len() - INPUT_MAX_ROWS);
    }
    rows
}

/// The editor's label for a mode, or None when no editor is open. ONE definition, because
/// main.rs clamps the detail scroll against `split` and the renderer draws against it; when
/// each worked the label out for itself they could disagree about the row budget, which is
/// the class of bug that let the buffer run off the pane in the first place.
pub fn input_label(mode: Option<&str>) -> Option<&'static str> {
    mode.map(|m| match m {
        "comment" => "comment",
        "decide" => "verdict",
        "enact" => "statute",
        // amend: operator is rewriting the proposed statute text before enacting.
        "amend" => "amended statute",
        // decline: optional training signal (why this law was not needed).
        "decline" => "why declined (optional)",
        // THE WHY IS THE TRAINING SIGNAL. A key that prompts "reason" over a rejection
        // reads the same as the one that prompts for a verdict, which is exactly the
        // confusion that made dismissals look like answers.
        "premise" => "why (training signal)",
        // A lawsuit verdict is not the same label as a plain verdict — the three options
        // are different enough that the prompt must name them.
        "suit" => "uphold / retire / amend: <new text>",
        _ => "reason",
    })
}

/// How many rows the editor will occupy — the number `split` must reserve. Kept beside the
/// wrapper so the reservation and the drawing can never disagree; they did, and the buffer
/// simply ran off the pane.
pub fn input_h(buf: &str, label: &str, w: usize) -> usize {
    input_rows(buf, label, w).len()
}

/// How the pane divides: `(list rows, detail rows)`.
///
/// Split out from `frame` so the row budget can be asserted in a test rather than eyeballed
/// in a screenshot. `need` is `detail_lines().len()`.
pub fn split(h: usize, input_h: usize, items: usize, need: usize) -> (usize, usize) {
    let chrome = 2 + input_h; // the two bands (+ however many rows the editor needs)
    let avail = h.saturating_sub(chrome).max(3);
    let usable = avail.saturating_sub(1); // the rule, which also names the selected item
    let mut list_h = (usable * 2) / 5; // 40% navigating, 60% reading
    list_h = list_h.max(1).min(usable.saturating_sub(1));
    if items < list_h {
        list_h = items.max(1); // give the slack to the detail
    }
    let mut detail_h = usable.saturating_sub(list_h).max(1);
    // AND THE SURPLUS COMES BACK. the operator's capture showed six blank rows under a four-line
    // body: the detail had been given ten rows, needed four, and painted the difference as
    // nothing. Rows the detail cannot fill go to the list, up to the number of items there
    // actually are, so a short body buys visible queue instead of empty pane. When there is
    // nothing to put in them either, they stay blank — but then nothing was being hidden.
    if need < detail_h {
        let grow = (detail_h - need)
            .min(items.saturating_sub(list_h))
            .min(detail_h.saturating_sub(1));
        list_h += grow;
        detail_h -= grow;
    }
    (list_h, detail_h)
}

pub fn frame(f: &Frame) -> Vec<String> {
    let mut out = Vec::new();

    // ── the top band ───────────────────────────────────────────────────────────────
    let mut tabs = String::new();
    for (i, v) in View::ALL.iter().enumerate() {
        let n = f.counts[i].map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        // No leading number: the tabs are cycled with Tab / Shift+Tab, not jumped to.
        // The FYI tab RENAMES while its history is on screen, because the count beside it
        // then means dismissed-so-far rather than waiting-to-read, and a tab whose number
        // silently changes meaning is worse than one that says which number it is showing.
        let name = match (*v, f.dismissed && *v == f.view) {
            // Record views (Insights, Notifications) both use "DISMISSED" — only the view on
            // screen is renamed; the other tabs keep their ordinary title.
            (View::Insights, true) | (View::Notifications, true) => "DISMISSED",
            // "CLEARED" rather than "SILENCED": the history holds both, and what is in it
            // mostly is conditions that went away. Either way the count beside the tab now
            // means something else, and a tab whose number silently changes meaning is worse
            // than one that says which set it is showing.
            (View::Alerts, true) => "CLEARED",
            _ => v.title(),
        };
        let label = format!(" {} {} ", name, n);
        if *v == f.view {
            tabs.push_str(&format!("{}{}{}{}", SEL, label, RST, BAR));
        } else {
            tabs.push_str(&label);
        }
    }
    let age = age_text(f.age, f.refreshing);
    let pad = f.w.saturating_sub(strip_len(&tabs) + age.chars().count() + 1);
    out.push(band(BAR, &format!("{}{} {}", tabs, " ".repeat(pad), age), f.w));

    let items = match f.items {
        Err(e) => {
            out.push(format!(" {}{}{}", BAD, e, RST));
            while out.len() < f.h.saturating_sub(1) {
                out.push(String::new());
            }
            out.push(footer(f, 0, 0));
            out.truncate(f.h);
            return out.into_iter().map(|l| fit(&l, f.w)).collect();
        }
        Ok(v) => v,
    };

    if items.is_empty() {
        // WHAT AN EMPTY VIEW SAYS IS ALSO FRAMING. "No unread insights" describes a queue
        // that has been drained; nothing owed describes a record that has been read, which
        // is what this view holds.
        // AND AN EMPTY ALERTS TAB IS A CLAIM, so it says what it checked. "No alerts" and
        // "the reader could not see the database" are the same pixels otherwise, and the
        // wrong one reads as all-clear (law-absence-needs-a-positive-control). `store::alerts`
        // refuses to return an empty list at all unless it read beads — so reaching this line
        // means the read is PROVED, and the wording is allowed to assert it. A reader that is
        // actually broken lands in the error frame above, in red, naming what it could not do.
        let msg = match (f.view, f.dismissed) {
            (View::Decisions, _) => "nothing waiting on you",
            (View::Insights, true) => "nothing dismissed yet",
            (View::Insights, false) => "nothing new to know",
            (View::Alerts, true) => "nothing has cleared or been silenced yet",
            (View::Alerts, false) => "read the alerts — none firing",
            (View::Notifications, true) => "nothing marked read yet",
            (View::Notifications, false) => "nothing has happened",
        };
        out.push(String::new());
        out.push(format!("  {}{}{}", OK, msg, RST));
        while out.len() < f.h.saturating_sub(1) {
            out.push(String::new());
        }
        out.push(footer(f, 0, 0));
        out.truncate(f.h);
        return out.into_iter().map(|l| fit(&l, f.w)).collect();
    }

    // ── layout: list on top, fixed detail area beneath ─────────────────────────────
    //
    // Detail gets the larger share, ~60%. the operator: *"most of the time is reading and
    // understanding, rather than navigating, and when the number of beads is low there's a
    // lot of dead space."* The old split gave the detail a FIXED four lines — about a
    // quarter — so the half of the pane doing the work you actually came for was the small
    // half, and a bead body was read four lines at a time.
    //
    // The split is measured against what the detail actually needs, so it has to be built
    // first. See `split`.
    let it = &items[f.sel];
    let detail = detail_lines(it, f.w);
    // The editor's label decides its indent and therefore its wrapping, so it is resolved
    // before the split rather than inside the block that draws it.
    let in_label = input_label(f.mode);
    let ih = in_label.map_or(0, |l| input_h(f.buf, l, f.w));
    let (list_h, detail_h) = split(f.h, ih, items.len(), detail.len());

    let start = f.sel.saturating_sub(list_h.saturating_sub(1));
    for (i, it) in items.iter().enumerate().skip(start).take(list_h) {
        let cur = i == f.sel;
        // HOW LONG IT HAS BEEN OPEN, not the id and not the clock. At a glance the age is
        // what tells you whether to care; the id is only needed once you are acting on it,
        // so it lives on the rule below, and so does the absolute timestamp. Four columns
        // holds everything `age_since` can produce — "now", "26m", "999d" — so the fifth
        // that the old wallclock stamp needed goes back to the title.
        let marker = if cur { "▶" } else { " " };
        let when = format!("{:>4}", age_since(&it.when, f.now));
        // WHO SPOKE LAST. the operator asked for a visual sign that I had answered in the thread,
        // because otherwise a reply of mine is invisible here and they have to open the bead
        // to discover whether anything happened.
        //   ↩  I replied last   — the ball is with them
        //   ●  they spoke last    — the ball is with me
        //   ·  written before this session and the pane had separate identities: both wrote
        //      as `overseer`, so the last speaker genuinely cannot be attributed. Guessing
        //      would put a confident arrow on a thread nobody can vouch for; these age out.
        //      no comments yet
        //
        // AND THERE IS NO TURN IN THE FYI VIEW. A ball marker says somebody owes somebody a
        // reply, which is exactly the claim an insight must not make — a `●` on a record is
        // the pane telling them it is their move on something that was only ever a note. There
        // it degrades to `·`: a conversation exists, nobody is waiting.
        //
        // AND IN ALERTS THE COLUMN SAYS SOMETHING ELSE ENTIRELY. There is no turn to take —
        // nobody owes anybody a reply about a condition — so the column carries the state of
        // the condition itself: `!` firing and unseen, `·` firing and acknowledged, `z`
        // silenced, `✓` cleared.
        //
        // THE LAST TWO ONLY EXIST IN THE HISTORY, and getting them there is not cosmetic: the
        // history holds cleared and silenced side by side, and a `!` on a condition that went
        // away by itself says the one thing about this tab that must never be false. `d` also
        // does different things to the two — it lifts a silence and refuses a cleared one —
        // so the row has to say which it is before the key is pressed.
        let op = crate::model::operator_actor();
        let turn = match (f.view, it.thread.last()) {
            (View::Alerts, _) if f.dismissed && crate::model::silenced(it, f.now) => "z",
            (View::Alerts, _) if f.dismissed => "✓",
            (View::Alerts, _) if crate::model::acked(it) => "·",
            (View::Alerts, _) => "!",
            (_, None) => " ",
            (View::Insights, Some(_)) => "·",
            (_, Some((a, _, _))) if a == crate::model::ME => "↩",
            (_, Some((a, _, _))) if *a == op => "●",
            (_, Some(_)) => "·",
        };
        // THE FLAP COUNT RIDES THE TITLE, because it is what says "this one keeps coming
        // back" and that is a property of the row rather than of the selection. It is clipped
        // WITH the title so a long title cannot push it off the end silently.
        let flaps = crate::model::flaps(it);
        let title = if f.view == View::Alerts && flaps > 1 {
            format!("×{flaps} {}", it.title)
        } else {
            it.title.clone()
        };
        // § — THIS ONE BECAME LAW, and a dismissed one did not.
        //
        // Before this the two were the same row: `d` and a promotion both ended with
        // `archived` and nothing else, so the feeder for the statute book could not be read
        // back — you could not tell a finding that had been shrugged off from one that is now
        // a rule every agent obeys, and a finding filed a SECOND time could not be recognised
        // as the re-violation that is the ladder's promotion trigger.
        //
        // The column is reserved for the whole FYI view rather than only for the rows that
        // have one, so the titles stay aligned and the mark is scannable down the edge
        // instead of shifting every line it appears on. It costs nothing in the other two
        // views, which have no citation to carry.
        let law = match (f.view, it.enacted.is_some()) {
            (View::Insights, true) => " §",
            (View::Insights, false) => "  ",
            _ => "",
        };
        let title = clip(&title, f.w.saturating_sub(12 + strip_len(law)));
        out.push(if cur {
            let line = format!(" {} {} {}{} {}", marker, when, turn, law, title);
            format!(
                "{}{}{}{}",
                SEL,
                line,
                " ".repeat(f.w.saturating_sub(strip_len(&line))),
                RST
            )
        } else {
            let tc = match turn {
                "↩" => ACC,
                "●" => OK,
                // A firing, unacknowledged condition is the one thing in this pane that is
                // wrong right now. It gets the only red in the list.
                "!" => BAD,
                "✓" => OK,
                _ => MUT,
            };
            format!(
                " {} {}{}{} {}{}{}{}{}{} {}{}{}",
                marker, MUT, when, RST, tc, turn, RST, ACC, law, RST, TXT, title, RST
            )
        });
    }
    while out.len() < list_h + 1 {
        out.push(String::new());
    }

    // ── the rule, which also names what is below it ────────────────────────────────
    // One row doing two jobs. The detail used to open with its own id/badge/timestamp line
    // directly under a blank horizontal rule, which is two rows of chrome stacked on top of
    // each other in a nineteen-row pane.
    let tag = format!(
        " {}{}{}{}  {}{}{}  {}{} ",
        BOLD,
        ACC,
        it.id,
        RST,
        MUT,
        it.badge,
        RST,
        age_and_stamp(it, f.now, MUT),
        RST
    );
    let fill = f.w.saturating_sub(2 + strip_len(&tag));
    out.push(format!(
        "{}──{}{}{}{}{}",
        MUT,
        RST,
        tag,
        MUT,
        "─".repeat(fill),
        RST
    ));

    // ── detail: the selected item, in full, in a place that never moves ────────────
    //
    // A PARAGRAPH BREAK IS NEVER THE LAST VISIBLE ROW. It separates nothing there, because
    // whatever it was separating is below the fold — so it is a blank row charged to the
    // body. Skip it and pull the next line up.
    let end = out.len() + detail_h;
    for l in detail.into_iter().skip(f.detail_scroll) {
        if out.len() >= end {
            break;
        }
        if out.len() + 1 == end && l.is_empty() {
            continue;
        }
        out.push(l);
    }
    while out.len() < f.h.saturating_sub(1 + ih) {
        out.push(String::new());
    }

    if let Some(label) = in_label {
        // The label prefixes the first row; continuation rows are indented to line up under
        // it, so a wrapped answer reads as one field rather than several.
        let rows = input_rows(f.buf, label, f.w);
        let indent = " ".repeat(label.chars().count() + 3);
        for (i, r) in rows.iter().enumerate() {
            if i == 0 {
                out.push(format!(" {}{}{}▸{} {}{}{}", BOLD, OK, label, RST, TXT, r, RST));
            } else {
                out.push(format!("{}{}{}{}", indent, TXT, r, RST));
            }
        }
    }
    out.push(footer(f, f.sel + 1, items.len()));
    out.truncate(f.h);
    for l in out.iter_mut() {
        *l = fit(l, f.w);
    }
    out
}

/// Visible length, ignoring ANSI escapes.
fn strip_len(s: &str) -> usize {
    let mut n = 0;
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            if c == 'm' {
                in_esc = false;
            }
        } else if c == '\x1b' {
            in_esc = true;
        } else {
            n += 1;
        }
    }
    n
}

/// Position plus the keys, and the destructive key is named for what it ACTUALLY does in
/// this view — "close", "archive" and "mark read" are different acts, and one word for all
/// three is how a keypress silently does nothing.
///
/// A failure takes the whole band red. It used to render as grey text in the same row that
/// normally lists key hints, which is where the eye has learned there is nothing to read.
fn footer(f: &Frame, pos: usize, total: usize) -> String {
    if f.mode.is_some() {
        return band(
            BAR,
            &format!(" {}⏎{} submit · {}esc{} cancel", KEY, BAR, KEY, BAR),
            f.w,
        );
    }
    if !f.flash.is_empty() {
        let colour = if f.flash.starts_with("FAILED") { ALERT } else { NOTE };
        return band(colour, &format!(" {}", f.flash), f.w);
    }
    let where_ = if total > 0 {
        format!("{}/{}", pos, total)
    } else {
        "—".into()
    };
    let mut s = format!(" {} · {}d{} {}", where_, KEY, BAR, f.view.verb(f.dismissed));
    match f.view {
        View::Decisions => {
            let cur = f.items.as_ref().ok().and_then(|v| v.get(f.sel));
            let is_law = cur.map(crate::model::is_ask_law).unwrap_or(false);
            if is_law {
                // Law proposals: enact runs rule.sh; amend opens the editor with the title;
                // d declines without touching the statute book.
                s.push_str(&format!(
                    " · {}e{} enact · {}E{} amend+enact · {}d{} decline · {}c{} comment",
                    KEY, BAR, KEY, BAR, KEY, BAR, KEY, BAR
                ));
            } else {
                // `a` is only offered when there is actually a default to accept — a key
                // advertised on an item it cannot act on teaches you to distrust the footer.
                let has_default = cur.map(|it| !it.lead.is_empty()).unwrap_or(false);
                if has_default {
                    s.push_str(&format!(" · {}a{} accept default", KEY, BAR));
                }
                s.push_str(&format!(
                    " · {}⏎{} decide · {}p{} reject · {}c{} comment",
                    KEY, BAR, KEY, BAR, KEY, BAR
                ));
            }
        }
        // NO `a accept default` AND NO `⏎ decide` HERE, ever — those are the two keys that
        // made the view read as a queue of asks. `h` is the retrieval half of dismissal: an
        // insight that leaves the pane has to be findable, or dismissing it is a deletion and
        // they will hesitate over every one.
        // AND `L enact`, the third answer. `d` says this does not generalise and `⏎` keeps
        // the thread; neither can say "this is law now", which is what an insight most often
        // turns out to be — four of the nine live ones were already doctrine, each promoted
        // by hand and then filed as though it had been shrugged off.
        View::Insights => s.push_str(&format!(
            " · {}⏎{} comment · {}L{} enact · {}h{} {}",
            KEY,
            BAR,
            KEY,
            BAR,
            KEY,
            BAR,
            if f.dismissed { "back" } else { "dismissed" }
        )),
        // NO `a accept default` AND NO `⏎ decide` HERE EITHER, for a different reason: an
        // alert has nothing to choose until he has looked. `s` is offered only on the live
        // tab, because silencing something that has already cleared is a key that appears to
        // work on the wrong thing.
        View::Alerts => {
            if !f.dismissed {
                s.push_str(&format!(" · {}s{} silence 1h", KEY, BAR));
            }
            s.push_str(&format!(
                " · {}⏎{} comment · {}h{} {}",
                KEY,
                BAR,
                KEY,
                BAR,
                if f.dismissed { "back" } else { "cleared" }
            ));
        }
        // `A` bulk-archives; `h` shows what has been marked read (same pattern as FYI).
        View::Notifications => s.push_str(&format!(
            " · {}A{} mark all read · {}h{} {}",
            KEY,
            BAR,
            KEY,
            BAR,
            if f.dismissed { "back" } else { "read" }
        )),
    }
    s.push_str(&format!(
        " · {}o{} full · {}⇥{} view · {}r{} sync",
        KEY, BAR, KEY, BAR, KEY, BAR
    ));
    band(BAR, &s, f.w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{operator_actor, Item};

    fn item(title: &str, body: &str) -> Item {
        Item {
            id: "hq-abcd".into(),
            title: title.into(),
            lead: String::new(),
            body: body.into(),
            badge: "question".into(),
            when: "2026-09-05T10:06:00Z".into(),
            enacted: None,
            thread: Vec::new(),
            labels: Vec::new(),
        }
    }

    /// A firing alert, as the ALERTS view builds one: no `lead` ever, and the flap count on
    /// the badge.
    fn alert(title: &str, body: &str) -> Item {
        let mut it = item(title, body);
        it.badge = "alert".into();
        it.labels = vec!["alert".into(), "overseer".into()];
        it
    }

    fn alert_frame<'a>(items: &'a Result<Vec<Item>, String>, dismissed: bool) -> Frame<'a> {
        let mut f = a_frame(items, 107, 19);
        f.view = View::Alerts;
        f.dismissed = dismissed;
        f
    }

    fn insight(title: &str, body: &str) -> Item {
        let mut it = item(title, body);
        it.badge = "insight".into();
        it
    }

    /// An insight that was promoted — the case, and the statute it produced.
    fn enacted_insight(title: &str, body: &str, law: &str) -> Item {
        let mut it = insight(title, body);
        it.enacted = Some(law.into());
        it
    }

    fn fyi_frame<'a>(items: &'a Result<Vec<Item>, String>, dismissed: bool) -> Frame<'a> {
        let mut f = a_frame(items, 107, 19);
        f.view = View::Insights;
        f.dismissed = dismissed;
        f
    }

    /// A frozen instant, ten minutes after the `when` every test item carries. Pinned so an
    /// age is a fixed string rather than whatever the clock said while the suite ran — the
    /// same reason `PANEL_FIXTURE` exists, one layer down.
    const NOW: i64 = 1_788_625_200; // 2026-09-05T16:20:00Z

    fn a_frame<'a>(items: &'a Result<Vec<Item>, String>, w: usize, h: usize) -> Frame<'a> {
        Frame {
            view: View::Decisions,
            dismissed: false,
            items,
            counts: [Some(1), Some(0), Some(0), Some(0)],
            sel: 0,
            mode: None,
            buf: "",
            flash: "",
            age: Some(30),
            refreshing: false,
            detail_scroll: 0,
            now: NOW,
            w,
            h,
        }
    }

    /// Rule 1, as a check rather than a comment. `\x1b[2m` is faint grey on black, and it
    /// was on the BODY OF THE BEAD — the one thing in this pane that exists to be read.
    #[test]
    fn content_is_never_dim() {
        let it = item("a title", "a paragraph

and another");
        for l in detail_lines(&it, 107) {
            assert!(!l.contains("\x1b[2m"), "dim in the detail: {l:?}");
        }
        let (lines, _) = reader(&it, View::Decisions, NOW, 0, 107, 19);
        for l in &lines[1..lines.len() - 1] {
            assert!(!l.contains("\x1b[2m"), "dim in the reader body: {l:?}");
        }
    }

    /// Rule 2: the first and last rows are bands, and no other row is. If a third band
    /// appears, chrome has grown a row at the body's expense.
    #[test]
    fn the_frame_is_exactly_two_bands() {
        let items = Ok(vec![item("t", "one
two
three")]);
        let rows = frame(&a_frame(&items, 107, 19));
        assert!(rows[0].starts_with(BAR), "the header is a band");
        assert!(rows[18].starts_with(BAR), "the footer is a band");
        for (i, r) in rows.iter().enumerate().take(18).skip(1) {
            assert!(!r.starts_with(BAR), "row {i} is a band and should be content");
        }
    }

    /// The pane is exactly h rows and no row overflows w, at every view, in list mode and
    /// comment mode, with a long single-line item, a threaded reply, and (for ALERTS) a
    /// dismissed toggle — one sweep replacing the three that each repeated the same
    /// geometries (cluster 11: `every_row_fits_the_pane`, `a_threaded_item_fits_the_pane`,
    /// `the_alerts_view_fits_every_geometry`).
    #[test]
    fn every_view_fits_every_geometry() {
        let long = "word ".repeat(400);
        let mut long_thread = threaded();
        long_thread.thread[0].2 = "word ".repeat(200);
        let mut long_alert = alert(&long, &long);
        long_alert.labels.push("flaps:12".into());

        for view in View::ALL {
            let long_item = if view == View::Alerts { long_alert.clone() } else { item(&long, &long) };
            let other = if view == View::Alerts { alert("second", "b") } else { item("second", "b") };
            let items = Ok(vec![long_item.clone(), other]);

            for (w, h) in [(107, 19), (100, 16), (60, 8), (40, 5)] {
                for mode in [false, true] {
                    let mut f = a_frame(&items, w, h);
                    f.view = view;
                    f.mode = if mode { Some("comment") } else { None };
                    let rows = frame(&f);
                    assert_eq!(rows.len(), h, "{view:?} {w}x{h} mode={mode} produced {} rows", rows.len());
                    for (i, r) in rows.iter().enumerate() {
                        assert!(strip_len(r) <= w, "{view:?} {w}x{h} row {i} is {} wide", strip_len(r));
                    }
                }
                let (lines, _) = reader(&long_item, view, NOW, 0, w, h);
                assert_eq!(lines.len(), h, "{view:?} reader {w}x{h}");
                for r in &lines {
                    assert!(strip_len(r) <= w);
                }
            }
        }

        // The threaded shape gets its own pass — the content dimension the per-view sweep
        // above does not vary.
        let items = Ok(vec![long_thread.clone(), item("second", "b")]);
        for (w, h) in [(107, 19), (100, 16), (60, 8), (40, 5)] {
            let rows = frame(&a_frame(&items, w, h));
            assert_eq!(rows.len(), h, "threaded {w}x{h} produced {} rows", rows.len());
            for (i, r) in rows.iter().enumerate() {
                assert!(strip_len(r) <= w, "threaded {w}x{h} row {i} is {} wide", strip_len(r));
            }
            let (lines, _) = reader(&long_thread, View::Decisions, NOW, 0, w, h);
            assert_eq!(lines.len(), h, "threaded reader {w}x{h}");
            for r in &lines {
                assert!(strip_len(r) <= w);
            }
        }

        // And ALERTS' dismissed toggle, the one dimension unique to that view.
        let items = Ok(vec![long_alert.clone(), alert("second", "b")]);
        for (w, h) in [(107, 19), (100, 16), (60, 8), (40, 5)] {
            for dismissed in [false, true] {
                let mut f = alert_frame(&items, dismissed);
                f.w = w;
                f.h = h;
                let rows = frame(&f);
                assert_eq!(rows.len(), h, "dismissed={dismissed} {w}x{h} produced {} rows", rows.len());
                for (i, r) in rows.iter().enumerate() {
                    assert!(strip_len(r) <= w, "dismissed={dismissed} {w}x{h} row {i} is {} wide", strip_len(r));
                }
            }
        }
    }

    /// The empty and error frames are the same height as a full one — a short frame leaves
    /// the footer floating in the middle of the pane.
    #[test]
    fn the_empty_and_error_frames_are_full_height() {
        let empty: Result<Vec<Item>, String> = Ok(Vec::new());
        assert_eq!(frame(&a_frame(&empty, 107, 19)).len(), 19);
        let err: Result<Vec<Item>, String> = Err("bd: no such database".into());
        let rows = frame(&a_frame(&err, 107, 19));
        assert_eq!(rows.len(), 19);
        assert!(rows[18].starts_with(BAR));
    }

    /// A paragraph break on the last visible row separates nothing, because what it was
    /// separating is below the fold. It must not cost the body a row.
    #[test]
    fn a_trailing_paragraph_break_never_costs_a_row() {
        // Ten rows of detail; put a blank exactly where the fold falls.
        let body = (0..40)
            .map(|i| if i % 2 == 1 { String::new() } else { format!("para {i}") })
            .collect::<Vec<_>>()
            .join("\n");
        let items = Ok(vec![item("t", &body), item("b", "x"), item("c", "x")]);
        let rows = frame(&a_frame(&items, 107, 19));
        let last = rows.len() - 2; // the row above the footer band
        assert!(!rows[last].trim().is_empty(), "the last content row is blank");
    }


    /// the operator's capture: a four-line body in a ten-row detail area painted six blank rows,
    /// while items they could have seen sat below the fold of a three-row list.
    #[test]
    fn a_short_body_gives_its_surplus_rows_to_the_list() {
        // 19 rows, 12 items waiting, detail needs 4.
        let (list, detail) = split(19, 0, 12, 4);
        assert_eq!(detail, 4, "the detail keeps exactly what it can fill");
        assert_eq!(list + detail, 16, "and no row is lost in the handover");
        assert_eq!(list, 12, "the surplus is visible queue, not blank pane");
    }

    /// The surplus is capped by the queue. Two items cannot fill eleven rows either, so the
    /// blanks stay — but nothing is being hidden behind them.
    #[test]
    fn the_list_never_grows_past_the_items_it_has() {
        let (list, _) = split(19, 0, 2, 1);
        assert_eq!(list, 2);
    }

    /// A body longer than its share is unaffected: the split is unchanged from the 40/60
    /// rule when there is no surplus to hand back.
    #[test]
    fn a_long_body_keeps_the_reading_share() {
        let (list, detail) = split(19, 0, 12, 99);
        assert_eq!((list, detail), (6, 10));
        assert!(detail > list, "reading gets the larger half");
    }

    /// The input row is taken out of the content, not out of the bands.
    #[test]
    fn the_input_row_costs_one_content_row() {
        let (l0, d0) = split(19, 0, 12, 99);
        let (l1, d1) = split(19, 1, 12, 99);
        assert_eq!(l0 + d0, l1 + d1 + 1);
    }

    /// A reply longer than the pane is wide is still visible: it wraps, and it wraps to the
    /// END of the buffer so the cursor is on screen. This is the defect that had the operator
    /// typing blind past one line.
    #[test]
    fn a_long_reply_wraps_and_keeps_the_cursor_visible() {
        let long = "x".repeat(400);
        let rows = input_rows(&long, "verdict", 60);
        assert!(rows.len() > 1, "a 400-char reply must not be one row");
        assert!(rows.len() <= INPUT_MAX_ROWS);
        assert!(rows.last().unwrap().ends_with('█'), "the cursor must be on the last row");
        // Every drawn row fits the pane once the label indent is charged.
        for r in &rows {
            assert!(r.chars().count() <= 60 - ("verdict".len() + 3));
        }
    }

    /// And the rows it needs are the rows the split reserves, or the editor runs off the pane.
    #[test]
    fn the_split_reserves_exactly_the_rows_the_editor_draws() {
        for n in [0usize, 1, 40, 400, 4000] {
            let buf = "y".repeat(n);
            assert_eq!(input_h(&buf, "verdict", 60), input_rows(&buf, "verdict", 60).len());
        }
    }

    /// An empty buffer is one row carrying just the cursor — not zero rows, which would make
    /// the editor invisible the moment it opened.
    #[test]
    fn an_empty_reply_still_shows_a_cursor() {
        let rows = input_rows("", "verdict", 60);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], "█");
    }

    /// And a field that wrapped to three rows costs three. The reservation was a hard-coded
    /// single row, so a wrapping field would have painted over content the layout still
    /// believed it owned.
    #[test]
    fn a_wrapped_input_costs_every_row_it_takes() {
        let (l0, d0) = split(19, 0, 12, 99);
        for input in 1..=INPUT_MAX_ROWS {
            let (l, d) = split(19, input, 12, 99);
            assert_eq!(l0 + d0, l + d + input, "a {input}-row field lost a row somewhere");
        }
    }

    /// Nothing typed is lost and nothing is invented: while the answer still fits, the rows
    /// concatenate back to exactly the buffer plus its cursor. A field whose whole job is
    /// showing what was actually typed cannot swallow a doubled or a trailing space, which is
    /// what `wrap` — the PROSE wrapper, which splits on whitespace and rejoins with single
    /// spaces — would have done had it been reached for here.
    #[test]
    fn the_wrap_is_lossless_while_the_answer_fits() {
        let cases: Vec<String> = vec![
            String::new(),
            "a".into(),
            "hello  world".into(),
            "   leading".into(),
            "trailing   ".into(),
            "word ".repeat(20),
            "x".repeat(120),
        ];
        for buf in &cases {
            for w in [20usize, 40, 60, 107] {
                let rows = input_rows(buf, "verdict", w);
                let drawn = rows.concat();
                let whole = format!("{buf}█");
                if rows.len() < INPUT_MAX_ROWS {
                    assert_eq!(drawn, whole, "w={w} buf={buf:?}");
                }
                // And even once it scrolls, what is shown is a TAIL of what was typed —
                // never a rewrite of it.
                assert!(whole.ends_with(&drawn), "w={w} buf={buf:?} drew {drawn:?}");
            }
        }
    }

    /// THE THRESHOLD CASE, which is the one that regresses silently: one character short of
    /// the edge the field is one row, at the edge the caret still has its cell, and past it
    /// the text wraps and the caret goes with it. Before this, all three drew as one row and
    /// the last two ran off the pane into whatever the terminal felt like doing.
    #[test]
    fn the_wrap_threshold_moves_the_caret_down_not_off_the_edge() {
        let avail = 107 - ("verdict".chars().count() + 3); // 97 cells, cursor included
        let field = |n: usize| input_rows(&"a".repeat(n), "verdict", 107);
        assert_eq!(field(avail - 2).len(), 1, "short of the edge, one row");
        assert_eq!(field(avail - 1).len(), 1, "the last cell of the row is the caret's");
        assert_eq!(field(avail).len(), 2, "past the edge, the caret takes the next row");
        assert!(field(avail).last().unwrap().ends_with('█'));
        // And no row the frame draws runs past the pane, at the threshold or anywhere near it.
        let items = Ok(vec![item("t", "a body")]);
        for n in [0usize, 1, avail - 2, avail - 1, avail, avail + 1, 400] {
            let buf = "a".repeat(n);
            for l in text(&frame(&typed(&items, &buf))) {
                assert!(l.chars().count() <= 107, "n={n} row runs past the pane: {l:?}");
            }
        }
    }

    fn typed<'a>(items: &'a Result<Vec<Item>, String>, buf: &'a str) -> Frame<'a> {
        let mut f = a_frame(items, 107, 19);
        f.mode = Some("decide");
        f.buf = buf;
        f
    }

    /// The caret is on screen for every length there is, and exactly once. THAT is the
    /// invariant — not that all the text is visible — and it is asserted through the drawn
    /// frame, because the defect was never in the wrapping alone but in the field and the
    /// layout disagreeing about how many rows there were.
    #[test]
    fn the_caret_is_always_on_screen() {
        let items = Ok(vec![item("t", "a body")]);
        for n in [0usize, 1, 50, 95, 96, 97, 190, 384, 385, 900] {
            let buf = format!("{}end", "word ".repeat(n / 5));
            let rows = frame(&typed(&items, &buf));
            assert_eq!(rows.len(), 19, "n={n} changed the pane height");
            let t = text(&rows);
            let carets: Vec<usize> = (0..t.len()).filter(|&i| t[i].contains('█')).collect();
            assert_eq!(carets.len(), 1, "n={n} drew {} carets", carets.len());
            let ih = input_h(&buf, "verdict", 107);
            assert!(
                carets[0] >= 18 - ih && carets[0] < 18,
                "n={n} put the caret at row {} — outside the {ih}-row field",
                carets[0]
            );
        }
    }

    /// A long answer keeps its TAIL on screen, because that is where the operator is typing.
    /// What the field shows is the end of what was typed, exactly — a window that slid, not a
    /// paraphrase.
    #[test]
    fn a_long_answer_shows_its_tail() {
        let items = Ok(vec![item("t", "a body")]);
        let buf = format!("{}THE-TAIL-IS-HERE", "filler ".repeat(80));
        let ih = input_h(&buf, "verdict", 107);
        assert_eq!(ih, INPUT_MAX_ROWS, "this answer is meant to be scrolling the window");
        let t = text(&frame(&typed(&items, &buf)));
        // Strip the field's own gutter — the label on its first row, the matching indent on
        // the rest — and what is left is the buffer's last rows, run back together.
        let gutter = "verdict".chars().count() + 3;
        let drawn: String = t[19 - 1 - ih..18]
            .iter()
            .map(|l| l.chars().skip(gutter).collect::<String>())
            .collect();
        assert!(
            format!("{buf}█").ends_with(drawn.trim_end()),
            "the field is not showing a tail of the buffer: {drawn:?}"
        );
        assert!(
            drawn.contains("THE-TAIL-IS-HERE"),
            "the words being typed now are off screen: {drawn:?}"
        );
    }

    /// And chrome has still not grown a band. A four-row field comes out of the content
    /// budget; the header and footer are where they always were.
    #[test]
    fn the_field_does_not_grow_a_third_band() {
        let items = Ok(vec![item("t", "a body")]);
        let buf = "word ".repeat(80);
        let rows = frame(&typed(&items, &buf));
        assert!(rows[0].starts_with(BAR), "the header is a band");
        assert!(rows[18].starts_with(BAR), "the footer is a band");
        for (i, l) in rows.iter().enumerate().take(18).skip(1) {
            assert!(!l.starts_with(BAR), "row {i} became a band: {l:?}");
        }
    }

    /// A pane squeezed to nothing still renders both halves rather than panicking.
    #[test]
    fn a_tiny_pane_still_has_both_halves() {
        for h in 0..8 {
            let (list, detail) = split(h, 0, 3, 3);
            assert!(list >= 1 && detail >= 1, "h={h} gave {list}/{detail}");
        }
    }

    /// Visible text with the escapes taken out, for asserting about position rather than
    /// colour.
    fn text(lines: &[String]) -> Vec<String> {
        lines.iter().map(|l| strip_seq(l)).collect()
    }

    fn strip_seq(s: &str) -> String {
        let (mut out, mut esc) = (String::new(), false);
        for c in s.chars() {
            if esc {
                esc = c != 'm';
            } else if c == '\x1b' {
                esc = true;
            } else {
                out.push(c);
            }
        }
        out
    }

    fn at(lines: &[String], needle: &str) -> usize {
        text(lines)
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} is not on screen: {:#?}", text(lines)))
    }

    fn threaded() -> Item {
        let mut it = item("a title", "THE-ASK first paragraph\n\nTHE-ASK second paragraph");
        it.lead = "do X".into();
        it.thread = vec![
            (operator_actor(), "2026-09-05T15:00:00Z".into(), "OLDEST reply".into()),
            ("claude".into(), "2026-09-05T16:00:00Z".into(), "NEWEST reply".into()),
        ];
        it
    }

    /// (the operator's call) "i want threaded replies at the top." The thread is the newest
    /// content on the item and the ask is what they have already read, so a body long enough to
    /// run past the fold must not be what stands between them and the latest reply.
    #[test]
    fn the_thread_is_above_the_ask_on_both_surfaces() {
        let it = threaded();
        let (lines, _) = reader(&it, View::Decisions, NOW, 0, 107, 40);
        assert!(
            at(&lines, "NEWEST reply") < at(&lines, "THE-ASK first"),
            "the reader still buries the thread"
        );
        let d = detail_lines(&it, 107);
        assert!(
            at(&d, "NEWEST reply") < at(&d, "THE-ASK first"),
            "the detail strip still buries the thread"
        );
    }

    /// The thread reads newest first, so where the conversation got to is the first thing
    /// seen rather than something to scroll for.
    #[test]
    fn the_thread_reads_newest_first() {
        let it = threaded();
        let (lines, _) = reader(&it, View::Decisions, NOW, 0, 107, 40);
        assert!(at(&lines, "NEWEST reply") < at(&lines, "OLDEST reply"));
        let d = detail_lines(&it, 107);
        assert!(at(&d, "NEWEST reply") < at(&d, "OLDEST reply"));
    }

    /// The recommended default keeps the top of the strip. It is the one line that lets an
    /// item be answered without opening it, so the thread goes under it, not over it.
    #[test]
    fn the_default_still_leads_the_detail_strip() {
        let d = detail_lines(&threaded(), 107);
        assert!(at(&d, "do X") < at(&d, "NEWEST reply"));
        assert!(at(&d, "a title") < at(&d, "do X"));
    }

    /// The reader's rule names the ask beneath it, so the ask cannot read as one more reply.
    #[test]
    fn the_reader_names_the_ask_below_the_thread() {
        let it = threaded();
        let (lines, _) = reader(&it, View::Decisions, NOW, 0, 107, 40);
        let r = at(&lines, "the question");
        assert!(at(&lines, "OLDEST reply") < r && r < at(&lines, "THE-ASK first"));
    }

    /// And that rule is never trailing chrome: with nothing beneath it to name, it is not
    /// spent. Rule 3 — every row is earned.
    #[test]
    fn no_rule_when_there_is_nothing_to_separate() {
        let mut it = threaded();
        it.body = String::new();
        let (lines, _) = reader(&it, View::Decisions, NOW, 0, 107, 40);
        assert!(!text(&lines).iter().any(|l| l.contains("the question")));
        let unthreaded = item("a title", "THE-ASK only");
        let (lines, _) = reader(&unthreaded, View::Decisions, NOW, 0, 107, 40);
        assert!(!text(&lines).iter().any(|l| l.contains("───")));
    }

    /// An item with no thread renders exactly as it did before any of this.
    #[test]
    fn an_unthreaded_item_is_untouched() {
        let it = item("a title", "one\n\ntwo");
        let d = detail_lines(&it, 107);
        assert_eq!(text(&d), vec![" a title", " one", "", " two"]);
    }

    /// The thread is content, so rule 1 covers it too: no `\x1b[2m` on anything anyone reads.
    #[test]
    fn the_thread_is_never_dim() {
        let it = threaded();
        for l in detail_lines(&it, 107) {
            assert!(!l.contains("\x1b[2m"), "dim in the thread: {l:?}");
        }
        let (lines, _) = reader(&it, View::Decisions, NOW, 0, 107, 40);
        for l in &lines[1..lines.len() - 1] {
            assert!(!l.contains("\x1b[2m"), "dim in the thread: {l:?}");
        }
    }

    /// The parser is checked against the system clock's own answer, not against itself.
    /// Every value here came from `date -u -d <stamp> +%s` on this box; a date arithmetic
    /// that agrees only with its own tests is how an age is confidently wrong.
    #[test]
    fn epoch_agrees_with_date() {
        assert_eq!(epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch("2026-09-05T16:20:00Z"), Some(1_788_625_200));
        assert_eq!(epoch("1999-12-31T23:59:59Z"), Some(946_684_799));
        // A leap day, which is the whole reason this is days-from-civil and not (y-1970)*365.
        assert_eq!(epoch("2000-02-29T12:00:00Z"), Some(951_825_600));
        // An offset is honoured rather than ignored: 18:20+02:00 IS 16:20Z.
        assert_eq!(epoch("2026-09-05T18:20:00+02:00"), Some(1_788_625_200));
        assert_eq!(epoch("2026-09-05T14:20:00-02:00"), Some(1_788_625_200));
        // Anything short of a full stamp has no age, and says so with None rather than 1970.
        assert_eq!(epoch(""), None);
        assert_eq!(epoch("2026-09-05T16:20"), None);
        assert_eq!(epoch("not a date at all"), None);
    }

    /// (the operator's call) "i want to know how long the item has been open, not the
    /// wallclock time it was opened." Buckets, and only three units — an exact age would
    /// tick every second and repaint the pane forever.
    #[test]
    fn age_is_a_duration_in_coarse_buckets() {
        let ago = |s: i64| age_since("2026-09-05T16:20:00Z", NOW + s);
        assert_eq!(ago(0), "now");
        assert_eq!(ago(59), "now");
        assert_eq!(ago(60), "1m");
        assert_eq!(ago(12 * 60), "12m");
        assert_eq!(ago(3599), "59m");
        assert_eq!(ago(3600), "1h");
        assert_eq!(ago(4 * 3600), "4h");
        assert_eq!(ago(86_399), "23h");
        assert_eq!(ago(86_400), "1d");
        assert_eq!(ago(3 * 86_400), "3d");
        // A clock disagreement is not a negative age.
        assert_eq!(ago(-9_000), "now");
        // No stamp, no age — blank, never a fabricated one.
        assert_eq!(age_since("", NOW), "");
        // Every bucket fits the four columns the list gives it.
        for s in [0, 60, 3599, 3600, 86_400, 900 * 86_400, 40_000 * 86_400] {
            assert!(ago(s).chars().count() <= 4, "{:?} is too wide", ago(s));
        }
    }

    /// The list column is an age. It used to be a wallclock time that changed units at
    /// midnight — "16:04" for today's rows against "Sep02" for the rest — so the two rows
    /// most worth comparing were the two that could not be.
    #[test]
    fn the_list_says_how_long_not_when() {
        let mut old = item("three days old", "b");
        old.when = "2026-09-02T16:20:00Z".into();
        let mut fresh = item("fourteen minutes old", "b");
        fresh.when = "2026-09-05T16:06:00Z".into();
        let items = Ok(vec![old, fresh]);
        let rows = text(&frame(&a_frame(&items, 107, 19)));
        let three = rows.iter().find(|l| l.contains("three days old")).unwrap();
        let fresh = rows.iter().find(|l| l.contains("fourteen minutes")).unwrap();
        assert!(three.contains("3d"), "no age on the older row: {three:?}");
        assert!(fresh.contains("14m"), "no age on the newer row: {fresh:?}");
        // Neither notation the wallclock column used survives, and that is the fix: the two
        // rows are now in the same units and can be compared at a glance.
        assert!(!three.contains("Sep"), "the list still shows a date: {three:?}");
        assert!(!fresh.contains("16:06"), "the list still shows a clock: {fresh:?}");
    }

    /// And the absolute timestamp is still reachable — on the rule and in the reader's
    /// header, one keypress from the list. It is not what the list is for; it is what
    /// correlates a bead with a log line.
    #[test]
    fn the_absolute_timestamp_stays_reachable() {
        let items = Ok(vec![item("t", "b")]);
        let rows = text(&frame(&a_frame(&items, 107, 19)));
        let rule = rows.iter().find(|l| l.starts_with("──")).unwrap();
        assert!(rule.contains("6h old"), "no age on the rule: {rule:?}");
        assert!(rule.contains("2026-09-05T10:06"), "no timestamp on the rule: {rule:?}");
        let (lines, _) = reader(&item("t", "b"), View::Decisions, NOW, 0, 107, 19);
        let head = strip_seq(&lines[0]);
        assert!(head.contains("6h old") && head.contains("2026-09-05T10:06"), "{head:?}");
        // Age FIRST on both, which is also the truncation precedence — see below.
        assert!(rule.find("6h old") < rule.find("2026-09-05"));
        assert!(head.find("6h old") < head.find("2026-09-05"));
    }

    /// Rows are cut from the right, so a pane too narrow for both loses the timestamp and
    /// keeps the age. That is the point of the order, not a side effect of it.
    #[test]
    fn a_narrow_pane_keeps_the_age_and_drops_the_timestamp() {
        let items = Ok(vec![item("t", "b")]);
        let rows = text(&frame(&a_frame(&items, 40, 12)));
        let rule = rows.iter().find(|l| l.starts_with("──")).unwrap();
        assert!(rule.contains("6h old"), "the age was cut first: {rule:?}");
        assert!(
            !rule.contains("2026-09-05T10:06"),
            "40 columns cannot hold both: {rule:?}"
        );
    }

    /// The thread is above the body in the actual frame, not only in detail_lines.
    ///
    /// `frame()` applies the detail_scroll and the split, so a test through `detail_lines`
    /// alone does not prove the full pipeline. At default scroll the thread must be in the
    /// visible window before the ask.
    #[test]
    fn the_thread_precedes_the_body_in_the_frame() {
        let items = Ok(vec![threaded()]);
        let rows = frame(&a_frame(&items, 107, 40));
        assert!(
            at(&rows, "NEWEST reply") < at(&rows, "THE-ASK first"),
            "thread not visible before the body in the frame"
        );
    }

    // ── FYI: an insight is a record, not a request ────────────────────────────────────
    //
    // (the operator, verbatim: *"these insights seem more like bug reports which means they're
    // implicitly asking me for feedback -- really they should be FYI only."*) Every one of
    // these asserts the absence of a decide-shaped affordance, which is the only way to keep
    // one from coming back: nothing about the view's SHAPE announced that adding a `default:`
    // line or a `⏎ decide` hint to it was wrong.

    /// The tab names the view, and the view is FYI. The label the beads carry is `insight`;
    /// what they need to know at a glance is that nothing here is owed.
    #[test]
    fn the_tab_says_fyi_not_insights() {
        let items = Ok(vec![insight("a finding", "a body")]);
        let rows = frame(&fyi_frame(&items, false));
        assert!(strip_seq(&rows[0]).contains("FYI"), "{:?}", strip_seq(&rows[0]));
        assert!(!strip_seq(&rows[0]).contains("INSIGHTS"));
    }

    /// The destructive key on an FYI dismisses, and there is no `⏎ decide` and no
    /// `a accept default` anywhere on the footer. Those two are what made it read as a queue.
    #[test]
    fn the_fyi_footer_offers_no_verdict() {
        let mut it = insight("a finding", "a body");
        it.lead = "do X".into(); // even if one somehow got through, it must not be offered
        let items = Ok(vec![it]);
        let rows = frame(&fyi_frame(&items, false));
        let foot = strip_seq(rows.last().unwrap());
        assert!(foot.contains("dismiss"), "{foot:?}");
        assert!(!foot.contains("decide"), "{foot:?}");
        assert!(!foot.contains("accept default"), "{foot:?}");
        assert!(foot.contains("h dismissed"), "retrieval must be advertised: {foot:?}");
    }

    /// The reader's footer used to advertise `⏎ decide` in every view, including one where
    /// the key was not bound — the loudest "this wants an answer" signal in the pane, on a
    /// record, doing nothing when pressed.
    #[test]
    fn the_fyi_reader_offers_no_verdict_either() {
        let it = insight("a finding", "a body");
        let (lines, _) = reader(&it, View::Insights, NOW, 0, 107, 19);
        let foot = strip_seq(lines.last().unwrap());
        assert!(!foot.contains("decide"), "{foot:?}");
        assert!(foot.contains("comment"), "{foot:?}");
        let (lines, _) = reader(&it, View::Decisions, NOW, 0, 107, 19);
        let foot = strip_seq(lines.last().unwrap());
        assert!(foot.contains("decide"), "a decision still decides: {foot:?}");
        // The reader footer must also offer `c comment`, matching the list footer.
        // This divergence is what let the original defect ship — the list advertised
        // the key; the reader (the surface where you actually form the thought) did not.
        assert!(foot.contains("comment"), "reader must offer c comment too: {foot:?}");
    }

    /// A `●` says the ball is with the operator. On a record that claim is false, and it is the
    /// single most direct way a pane can ask for a reply it does not want.
    ///
    /// THE AUTHOR COMES FROM `operator_actor()`, NOT A LITERAL. The marker fires when the last
    /// author IS the operator, and who that is comes from the environment — so a test that
    /// hardcoded the default asserted the marker's PRESENCE only on a machine with the variable
    /// unset, and failed on the very installation the panel runs on
    /// (law-gates-run-in-a-clean-environment).
    #[test]
    fn an_fyi_row_carries_no_turn_marker() {
        let mut it = insight("a finding", "a body");
        it.thread = vec![(operator_actor(), "2026-09-05T15:00:00Z".into(), "hm".into())];
        let items = Ok(vec![it.clone()]);
        let rows = text(&frame(&fyi_frame(&items, false)));
        assert!(!rows[1].contains('●'), "no ball on an FYI: {:?}", rows[1]);
        // The same item under DECISIONS still says whose turn it is — this is view-aware,
        // not a deletion.
        let mut f = fyi_frame(&items, false);
        f.view = View::Decisions;
        assert!(text(&frame(&f))[1].contains('●'));
    }

    /// The rule beneath the thread names what is under it. "The insight" invites an answer;
    /// the body of a record is the reason it was kept.
    #[test]
    fn the_reader_names_an_insights_body_why_it_matters() {
        let mut it = insight("a finding", "a body");
        it.thread = vec![(operator_actor(), "2026-09-05T15:00:00Z".into(), "hm".into())];
        let (lines, _) = reader(&it, View::Insights, NOW, 0, 107, 19);
        let joined = text(&lines).join("\n");
        assert!(joined.contains("why it matters"), "{joined}");
        assert!(!joined.contains("the insight"), "{joined}");
    }

    /// Browsing the dismissed set renames the tab and renames the key, because the count
    /// beside it now means something else and `d` there does the opposite thing.
    #[test]
    fn the_dismissed_view_says_so_and_restores() {
        let items = Ok(vec![insight("an old finding", "a body")]);
        let rows = frame(&fyi_frame(&items, true));
        assert!(strip_seq(&rows[0]).contains("DISMISSED"), "{:?}", strip_seq(&rows[0]));
        let foot = strip_seq(rows.last().unwrap());
        assert!(foot.contains("restore"), "{foot:?}");
        assert!(foot.contains("h back"), "{foot:?}");
    }

    /// An empty FYI view describes a record already read, not a queue drained.
    #[test]
    fn an_empty_fyi_view_asks_for_nothing() {
        let none: Result<Vec<Item>, String> = Ok(vec![]);
        assert!(text(&frame(&fyi_frame(&none, false)))
            .join("\n")
            .contains("nothing new to know"));
        assert!(text(&frame(&fyi_frame(&none, true)))
            .join("\n")
            .contains("nothing dismissed yet"));
    }
    // ── ALERTS: a condition, not a request and not a verdict ─────────────────────────
    //
    // The FYI checks above assert the absence of decide-shaped affordances because nothing
    // about the view's shape announced that adding one was wrong. The same holds here twice
    // over: an alert must not look answerable, and it must not look CLOSEABLE — the condition
    // is retracted by whatever asserted it.

    /// The tab exists, it is fourth, and it is named ALERTS. Not ESCALATIONS: that word
    /// already means a bead carrying the escalation label, a decision and a default, in
    /// the escalation policy and in several statutes.
    #[test]
    fn the_fourth_tab_is_called_alerts() {
        assert_eq!(View::ALL.len(), 4);
        assert_eq!(View::ALL[3], View::Alerts);
        let items = Ok(vec![alert("the loop is wedged", "no pass in 40m")]);
        let head = strip_seq(&frame(&alert_frame(&items, false))[0]);
        assert!(head.contains("ALERTS"), "{head:?}");
        assert!(!head.contains("ESCALATION"), "{head:?}");
        // And it is reachable by cycling, in both directions, from every other view.
        assert_eq!(View::Notifications.next(), View::Alerts);
        assert_eq!(View::Alerts.next(), View::Decisions);
        assert_eq!(View::Decisions.prev(), View::Alerts);
    }

    /// The footer's verbs are the alert's own. "close" is a decision's — the reason IS the
    /// verdict — and "dismiss" is an insight's; neither fits a condition that will retract
    /// itself. An alert is acknowledged, or silenced for a period.
    #[test]
    fn the_alert_footer_acknowledges_and_silences_and_never_decides() {
        let mut it = alert("the loop is wedged", "no pass in 40m");
        it.lead = "do X".into(); // even if one somehow got through, it must not be offered
        let items = Ok(vec![it]);
        let foot = strip_seq(frame(&alert_frame(&items, false)).last().unwrap());
        assert!(foot.contains("d ack"), "{foot:?}");
        assert!(foot.contains("s silence 1h"), "{foot:?}");
        assert!(foot.contains("h cleared"), "the history must be advertised: {foot:?}");
        assert!(!foot.contains("decide"), "{foot:?}");
        assert!(!foot.contains("accept default"), "{foot:?}");
        assert!(!foot.contains("close"), "{foot:?}");
        assert!(!foot.contains("dismiss"), "{foot:?}");
    }

    /// And the reader's footer agrees with it. The two surfaces advertised different keys
    /// once already — `⏎ decide` on an insight, where the key was not even bound.
    #[test]
    fn the_alert_reader_offers_no_verdict_either() {
        let (lines, _) = reader(&alert("wedged", "no pass in 40m"), View::Alerts, NOW, 0, 107, 19);
        let foot = strip_seq(lines.last().unwrap());
        assert!(!foot.contains("decide"), "{foot:?}");
        assert!(foot.contains("comment"), "{foot:?}");
    }

    /// `s` is not offered over the history: silencing something that has already cleared is a
    /// key that appears to work on the wrong thing, and the history holds both kinds side by
    /// side. There `d` lifts a silence instead.
    #[test]
    fn the_cleared_view_says_so_and_unsilences() {
        let items = Ok(vec![alert("was wedged", "cleared at 04:10")]);
        let rows = frame(&alert_frame(&items, true));
        assert!(strip_seq(&rows[0]).contains("CLEARED"), "{:?}", strip_seq(&rows[0]));
        let foot = strip_seq(rows.last().unwrap());
        assert!(foot.contains("d unsilence"), "{foot:?}");
        assert!(foot.contains("h back"), "{foot:?}");
        assert!(!foot.contains("silence 1h"), "{foot:?}");
    }

    /// The history holds cleared and silenced side by side, and `d` does different things to
    /// the two — it lifts a silence, and refuses a cleared one. So the row says which it is
    /// before the key is pressed. A `!` there would be the one claim this tab must never make
    /// falsely: that a condition which went away by itself is still firing.
    #[test]
    fn the_history_tells_cleared_from_silenced() {
        let mut quiet = alert("still wedged, quietened", "no pass in 40m");
        quiet.labels.push("silent-until:2026-09-05T17:00:00Z".into());
        let items = Ok(vec![quiet, alert("was wedged", "cleared at 04:10")]);
        let rows = text(&frame(&alert_frame(&items, true)));
        assert!(rows[1].contains('z'), "a silenced condition is quiet, not gone: {:?}", rows[1]);
        assert!(rows[2].contains('✓'), "a cleared condition says so: {:?}", rows[2]);
        assert!(!rows[1].contains('!') && !rows[2].contains('!'), "{rows:?}");
    }

    /// The marker column carries the only state a reader can change here. It is NOT a turn
    /// marker: `●` claims somebody owes somebody a reply, which about a condition is false —
    /// the same claim the FYI view had to stop making.
    #[test]
    fn an_alert_row_says_unseen_or_acknowledged_and_never_whose_turn() {
        let mut it = alert("the loop is wedged", "no pass in 40m");
        it.thread = vec![("ryan".into(), "2026-09-05T15:00:00Z".into(), "hm".into())];
        let unseen = Ok(vec![it.clone()]);
        let row = text(&frame(&alert_frame(&unseen, false)))[1].clone();
        assert!(row.contains('!'), "a firing alert is marked unseen: {row:?}");
        assert!(!row.contains('●'), "no ball on an alert: {row:?}");

        it.labels.push("acked".into());
        let seen = Ok(vec![it]);
        let row = text(&frame(&alert_frame(&seen, false)))[1].clone();
        assert!(!row.contains('!'), "an acknowledged alert is not still shouting: {row:?}");
    }

    /// "with the flap count visible where there is one" — on the row, because a condition
    /// that keeps coming back is a property of the row rather than of the selection. And not
    /// on one that has only fired once: a "×1" everywhere makes a real "×7" harder to see.
    #[test]
    fn a_flapping_alert_shows_its_count_on_the_row() {
        let mut it = alert("the loop is wedged", "no pass in 40m");
        it.labels.push("flaps:7".into());
        let items = Ok(vec![it, alert("disk is full", "94%")]);
        let rows = text(&frame(&alert_frame(&items, false)));
        assert!(rows[1].contains("×7 the loop is wedged"), "{:?}", rows[1]);
        assert!(!rows[2].contains('×'), "{:?}", rows[2]);
    }

    /// AN EMPTY ALERTS TAB IS A CLAIM AND SAYS WHAT IT CHECKED. "No alerts" and "the reader
    /// could not see the database" are the same pixels otherwise, and the wrong one reads as
    /// all-clear (law-absence-needs-a-positive-control). The refusal is in `store::alerts`;
    /// this is the half of it a reader actually sees.
    #[test]
    fn an_empty_alerts_tab_is_distinguishable_from_a_broken_reader() {
        let none: Result<Vec<Item>, String> = Ok(vec![]);
        let clear = text(&frame(&alert_frame(&none, false))).join("\n");
        assert!(clear.contains("read the alerts — none firing"), "{clear}");

        let broken: Result<Vec<Item>, String> =
            Err("no beads read at all — cannot say whether anything is firing".into());
        let rows = frame(&alert_frame(&broken, false));
        let shown = text(&rows).join("\n");
        assert!(shown.contains("cannot say whether anything is firing"), "{shown}");
        assert_ne!(clear, shown, "an all-clear and a broken reader render the same");
        // And the broken one is red, where the all-clear is green. The difference has to
        // survive being glanced at, not only being read.
        assert!(rows.iter().any(|l| l.contains(BAD)), "a broken reader must render as a failure");
        assert!(
            frame(&alert_frame(&none, false)).iter().any(|l| l.contains(OK)),
            "an all-clear must render as one"
        );
    }

    /// The rule beneath the thread names what is under it. "The alert" names the message; the
    /// body says what is TRUE, and calling it a question invites an answer there is none of.
    #[test]
    fn the_reader_names_an_alerts_body_the_condition() {
        let mut it = alert("wedged", "no pass in 40m");
        it.thread = vec![("ryan".into(), "2026-09-05T15:00:00Z".into(), "on it".into())];
        let (lines, _) = reader(&it, View::Alerts, NOW, 0, 107, 19);
        let joined = text(&lines).join("\n");
        assert!(joined.contains("the condition"), "{joined}");
        assert!(!joined.contains("the question"), "{joined}");
        // A flapping one keeps it — the badge carries the count, so the match is a prefix.
        it.badge = "alert ×7".into();
        let (lines, _) = reader(&it, View::Alerts, NOW, 0, 107, 19);
        assert!(text(&lines).join("\n").contains("the condition"));
    }

    /// `iso` is the exact inverse of `epoch`, and `epoch` is checked against the system
    /// `date` above. An arithmetic that agrees only with its own inverse can be wrong in both
    /// directions at once and still pass, which is why both halves are here.
    #[test]
    fn iso_round_trips_through_epoch() {
        for stamp in [
            "1970-01-01T00:00:00Z",
            "2000-02-29T12:00:00Z",
            "1999-12-31T23:59:59Z",
            "2026-09-05T16:20:00Z",
            "2026-12-31T23:59:59Z",
            "2027-03-01T00:00:01Z",
        ] {
            assert_eq!(iso(epoch(stamp).unwrap()), stamp);
        }
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(NOW), "2026-09-05T16:20:00Z");
        // The one this exists for: a silence deadline an hour out.
        assert_eq!(iso(NOW + 3600), "2026-09-05T17:20:00Z");
    }

    // ── promotion: an enacted insight is not a dismissed one ──────────────────────────

    /// The acceptance criterion, on the surface it is read from: a promoted insight and a
    /// dismissed one must not render the same. They differed by nothing at all before —
    /// both carried `archived` and were filed side by side under `h`.
    #[test]
    fn a_promoted_row_is_visibly_not_a_dismissed_one() {
        let promoted = vec![enacted_insight("closed over a red PR", "b", "law-closed-is-not-landed")];
        let plain = vec![insight("closed over a red PR", "b")];
        let (a, b) = (Ok(promoted), Ok(plain));
        let one = text(&frame(&fyi_frame(&a, true)));
        let two = text(&frame(&fyi_frame(&b, true)));
        assert!(one[1].contains('§'), "a promoted row carries the mark: {:?}", one[1]);
        assert!(!two[1].contains('§'), "a dismissed row does not: {:?}", two[1]);
    }

    /// And the citation itself is on screen, addressable — `rule.sh show` takes exactly the
    /// string rendered, so the rule can be read from the case in one step.
    #[test]
    fn the_citation_names_the_statute_on_both_surfaces() {
        let it = enacted_insight("t", "body", "law-closed-is-not-landed");
        let items = Ok(vec![it.clone()]);
        let strip = text(&frame(&fyi_frame(&items, true))).join("\n");
        assert!(strip.contains("enacted: law-closed-is-not-landed"), "{strip}");
        let (lines, _) = reader(&it, View::Insights, NOW, 0, 107, 19);
        let read = text(&lines).join("\n");
        assert!(read.contains("enacted: law-closed-is-not-landed"), "{read}");
    }

    /// The mark is FYI's alone. A decision is not a case for a statute, so the column it
    /// would cost is not spent there.
    #[test]
    fn the_mark_never_appears_outside_fyi() {
        let mut it = enacted_insight("t", "b", "law-x");
        it.badge = "question".into();
        let items = Ok(vec![it]);
        for view in [View::Decisions, View::Notifications] {
            let mut f = a_frame(&items, 107, 19);
            f.view = view;
            assert!(!text(&frame(&f))[1].contains('§'));
        }
    }

    /// An advertised key that does nothing is how a footer stops being believed — and an
    /// unadvertised one is never found at all.
    #[test]
    fn both_footers_offer_the_key_in_fyi_and_nowhere_else() {
        let items = Ok(vec![insight("t", "b")]);
        let list = text(&frame(&fyi_frame(&items, false)));
        assert!(list.last().unwrap().contains("L enact"), "{:?}", list.last());
        let (lines, _) = reader(&insight("t", "b"), View::Insights, NOW, 0, 107, 19);
        assert!(text(&lines).last().unwrap().contains("L enact"));

        let d = Ok(vec![item("t", "b")]);
        let plain = text(&frame(&a_frame(&d, 107, 19)));
        assert!(!plain.last().unwrap().contains("L enact"));
        let (lines, _) = reader(&item("t", "b"), View::Decisions, NOW, 0, 107, 19);
        assert!(!text(&lines).last().unwrap().contains("L enact"));
    }

    // ── reject-premise: a fourth exit from a decision row ────────────────────────────────
    //
    // THE BUG: Ryan typed "done" to dismiss four beads he refused to answer. There was no
    // dismiss key for DECISIONS, so "done" was stored as the close_reason and announced as a
    // verdict. `p` is that key, and its prompt names itself as a training signal so the act
    // is legible even when the why is left blank. (sp-sy5xp, 2026-09-09)

    /// The DECISIONS footer must offer `p reject` — a key that is not advertised is one that
    /// is never found. This fails to compile against the unfixed tree, satisfying
    /// law-a-regression-test-must-be-seen-to-fail.
    #[test]
    fn the_decisions_footer_offers_p_reject() {
        let items = Ok(vec![item("should I mute this check?", "it pages every hour")]);
        let foot = strip_seq(frame(&a_frame(&items, 107, 19)).last().unwrap());
        assert!(foot.contains("p reject"), "footer must advertise p: {foot:?}");
    }

    /// `p` must not appear on FYI, NOTIFICATIONS or ALERTS footers — those are record views
    /// whose dismiss semantics are already correct.
    #[test]
    fn p_reject_is_absent_from_non_decision_footers() {
        let insight_items = Ok(vec![insight("a finding", "why it matters")]);
        let fyi_foot = strip_seq(frame(&fyi_frame(&insight_items, false)).last().unwrap());
        assert!(!fyi_foot.contains("p reject"), "FYI must not offer p: {fyi_foot:?}");

        let alert_items = Ok(vec![alert("wedged", "no pass")]);
        let alert_foot = strip_seq(frame(&alert_frame(&alert_items, false)).last().unwrap());
        assert!(!alert_foot.contains("p reject"), "ALERTS must not offer p: {alert_foot:?}");
    }

    /// The reader's footer must match the list's footer for DECISIONS.
    ///
    /// The original FYI defect shipped because the list advertised ⏎ decide and the reader
    /// advertised it too — but on DECISIONS, where both were wrong. The two surfaces must
    /// never diverge for the same view.
    #[test]
    fn the_decisions_reader_footer_also_offers_p_reject() {
        let it = item("should I mute this check?", "it pages every hour");
        let (lines, _) = reader(&it, View::Decisions, NOW, 0, 107, 19);
        let foot = strip_seq(lines.last().unwrap());
        assert!(foot.contains("p reject"), "reader footer must advertise p: {foot:?}");
    }

    /// The input label for "premise" mode distinguishes the training signal from a verdict.
    /// A label that says "reason" over a rejection reads the same as the verdict prompt.
    #[test]
    fn the_input_label_for_premise_says_training_signal() {
        assert_eq!(input_label(Some("premise")), Some("why (training signal)"));
        // And no other mode is affected.
        assert_eq!(input_label(Some("decide")), Some("verdict"));
        assert_eq!(input_label(Some("comment")), Some("comment"));
        assert_eq!(input_label(None), None);
    }

    /// The compose row names what is being typed. "reason" over a statute would be the same
    /// class of lie as `⏎ decide` over an insight.
    #[test]
    fn the_input_row_says_statute() {
        let items = Ok(vec![insight("t", "b")]);
        let mut f = fyi_frame(&items, false);
        f.mode = Some("enact");
        f.buf = "closed-is-not-landed: Close a bead when it landed.";
        let rows = text(&frame(&f));
        let input = rows.iter().find(|l| l.contains('▸')).expect("an input row");
        assert!(input.contains("statute"), "{input}");
        assert!(input.contains("closed-is-not-landed"), "{input}");
    }

    /// A STATUTE IS LONGER THAN THE PANE. The multi-row input keeps the caret on screen by
    /// wrapping; no row may exceed the pane width.
    #[test]
    fn a_long_statute_keeps_its_caret_on_screen() {
        let long = format!("slug: {}", "word ".repeat(60));
        let items = Ok(vec![insight("t", "b")]);
        let mut f = fyi_frame(&items, false);
        f.mode = Some("enact");
        f.buf = &long;
        let rows = frame(&f);
        for l in &rows {
            assert!(strip_len(l) <= 107, "row wider than the pane: {}", strip_len(l));
        }
        // The caret block must appear somewhere in the input rows.
        let has_caret = rows.iter().any(|l| text(&[l.clone()]).first().map(|t| t.contains('█')).unwrap_or(false));
        assert!(has_caret, "the caret must survive in a long buffer");
    }

}
