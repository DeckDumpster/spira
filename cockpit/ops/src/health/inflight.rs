//! WORK IN FLIGHT — every running unit of work, from the `SP_INFLIGHT_*` keys the collector's
//! `inflight` probe composes. The headline is the same text the status line shows; a snapshot
//! without it is `?`, never an idle pane. Elapsed time is computed here so it never freezes.

use super::colors::*;
use super::fmt::{fit, mail_dur};
use super::model::Snapshot;

const MAX_SHOWN: usize = 6;
const BAR_CELLS: usize = 10;

fn bar(done: u64, total: u64) -> String {
    let filled = if total == 0 { 0 } else { ((done.min(total) * BAR_CELLS as u64) / total) as usize };
    format!("{}{}", "\u{2588}".repeat(filled), "\u{2591}".repeat(BAR_CELLS - filled))
}

pub fn row_text(raw: &str, now: i64) -> String {
    let f: Vec<&str> = raw.split('|').collect();
    let at = |i: usize| f.get(i).copied().unwrap_or("");
    let elapsed = at(3).parse::<i64>().map_or("?".to_string(), |t| mail_dur(&(now - t).max(0).to_string()));
    let progress = match (at(4).parse::<u64>(), at(5).parse::<u64>()) {
        (Ok(d), Ok(t)) => format!("  {ACC}{}{RST} {d}/{t}", bar(d, t)),
        (Ok(d), Err(_)) => format!("  {DIM}{d}/?{RST}"),
        _ => String::new(),
    };
    format!("{B}{:<6}{RST} {}  {DIM}{} {elapsed}{RST}{progress}", at(0), at(1), at(2))
}

fn row_line(raw: &str, cols: i64, now: i64) -> String {
    fit(&format!("  {}", row_text(raw, now)), cols)
}

pub fn inflight_section(snap: &Snapshot, cols: i64, now: i64) -> Vec<String> {
    let Some(seg) = snap.get("SP_INFLIGHT_SEG").filter(|s| !s.is_empty()) else {
        return vec![format!("{B}{BAD}\u{2699} ?{RST} {DIM}work in flight unread{RST}")];
    };
    let unread = seg.contains('?');
    let mut out = vec![format!("{}{seg}{RST}", if unread { format!("{B}{BAD}") } else { B.to_string() })];
    let rows: Vec<&str> = (0..).map_while(|i| snap.get(&format!("SP_INFLIGHT_ROW{i}")).filter(|s| !s.is_empty())).collect();
    out.extend(rows.iter().take(MAX_SHOWN).map(|r| row_line(r, cols, now)));
    if rows.len() > MAX_SHOWN {
        out.push(format!("  {DIM}+{} more{RST}", rows.len() - MAX_SHOWN));
    }
    out
}

/// One row of the work-in-flight view, parsed from the collector's `SP_INFLIGHT_ROW<n>` keys.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FlightRow {
    pub kind: String,
    pub subject: String,
    pub detail: String,
    pub started: Option<i64>,
    pub done: Option<u64>,
    pub total: Option<u64>,
}

/// The same facts as the pane section, for a page and its JSON. `readable` is false when the
/// snapshot has no segment or is older than `stale_s`: the view then says `?`, never idle.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Flight {
    pub readable: bool,
    pub segment: String,
    pub world: String,
    pub rows: Vec<FlightRow>,
}

impl Flight {
    pub fn from_snapshot(snap: &Snapshot, now: i64, stale_s: i64) -> Flight {
        let fresh = snap.get("SP_AT").and_then(|v| v.parse::<i64>().ok()).is_some_and(|at| now - at <= stale_s);
        let seg = snap.get("SP_INFLIGHT_SEG").filter(|s| !s.is_empty());
        let (readable, segment) = match seg {
            Some(s) if fresh => (true, s.to_string()),
            Some(s) => (false, format!("{s} (stale)")),
            None => (false, "\u{2699} ?inflight".to_string()),
        };
        let rows = if readable {
            (0..)
                .map_while(|i| snap.get(&format!("SP_INFLIGHT_ROW{i}")).filter(|s| !s.is_empty()))
                .map(|raw| {
                    let f: Vec<&str> = raw.split('|').collect();
                    let at = |i: usize| f.get(i).copied().unwrap_or("");
                    FlightRow {
                        kind: at(0).to_string(),
                        subject: at(1).to_string(),
                        detail: at(2).to_string(),
                        started: at(3).parse().ok(),
                        done: at(4).parse().ok(),
                        total: at(5).parse().ok(),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        Flight { readable, segment, world: snap.get("SP_INFLIGHT_WORLD").unwrap_or("?").to_string(), rows }
    }

    pub fn unread(&self) -> bool {
        !self.readable || self.segment.contains('?') || self.world == "?"
    }

    pub fn html(&self, now: i64) -> String {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let class = if self.unread() { "line bad" } else { "line" };
        let mut h = format!("<section id=inflight><div class='{class}'>{} <span class=world>[{}]</span></div>", esc(&self.segment), esc(&self.world));
        for r in &self.rows {
            let age = r.started.map_or("?".to_string(), |t| mail_dur(&(now - t).max(0).to_string()));
            let progress = match (r.done, r.total) {
                (Some(d), Some(t)) => format!(" <progress max={t} value={d}></progress> {d}/{t}"),
                (Some(d), None) => format!(" {d}/?"),
                _ => String::new(),
            };
            h.push_str(&format!("<div class=line>{} {} {} {age}{progress}</div>", esc(&r.kind), esc(&r.subject), esc(&r.detail)));
        }
        h.push_str("</section>");
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        let mut o = String::new();
        let mut esc = false;
        for c in s.chars() {
            if esc { if c.is_ascii_alphabetic() { esc = false } } else if c == '\x1b' { esc = true } else { o.push(c) }
        }
        o
    }

    fn render(snap: &str) -> Vec<String> {
        inflight_section(&Snapshot::parse(snap), 100, 700).iter().map(|l| strip(l)).collect()
    }

    #[test]
    fn running_work_lists_every_kind_with_a_bar_for_rounds() {
        let out = render(
            "SP_INFLIGHT_SEG='\u{2699} 1 dwarf \u{b7} 1 round (5/10) \u{b7} 1 gate'\n\
             SP_INFLIGHT_ROW0='dwarf|sp-1|warden|100||'\nSP_INFLIGHT_ROW1='round|spira/b1|certifying|100|5|10'\n\
             SP_INFLIGHT_ROW2='gate|spira-x|pre-merge|?||'\n",
        );
        assert_eq!(out.len(), 4);
        assert!(out[0].contains("1 dwarf") && out[0].contains("1 gate"));
        assert!(out[1].contains("sp-1") && out[1].contains("warden") && out[1].contains("10m"));
        assert!(out[2].contains("\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}") && out[2].contains("5/10"));
        assert!(out[3].contains("pre-merge") && out[3].contains("?"));
    }

    #[test]
    fn idle_is_one_line_and_an_absent_key_is_not_idle() {
        assert_eq!(render("SP_INFLIGHT_SEG='\u{2699} idle'\n"), vec!["\u{2699} idle"]);
        let missing = render("SP_OPEN='1'\n");
        assert!(missing[0].contains('?') && !missing[0].contains("idle"));
    }

    #[test]
    fn the_page_view_lists_each_kind_and_never_shows_unreadable_as_idle() {
        let snap = Snapshot::parse(
            "SP_AT='690'\nSP_INFLIGHT_WORLD='draining'\nSP_INFLIGHT_SEG='\u{2699} 1 round (5/10) \u{b7} draining'\nSP_INFLIGHT_ROW0='round|spira/b1|certifying|100|5|10'\n",
        );
        let f = Flight::from_snapshot(&snap, 700, 60);
        assert!(f.readable && f.rows.len() == 1 && f.rows[0].total == Some(10));
        let h = f.html(700);
        assert!(h.contains("spira/b1") && h.contains("5/10") && h.contains("draining") && !h.contains("line bad"));
        let stale = Flight::from_snapshot(&snap, 7000, 60);
        assert!(!stale.readable && stale.rows.is_empty() && stale.html(7000).contains("line bad"));
        let none = Flight::from_snapshot(&Snapshot::parse("SP_AT='690'\n"), 700, 60);
        assert!(none.segment.contains("?inflight") && !none.segment.contains("idle"));
        let dark = Snapshot::parse("SP_AT='690'\nSP_INFLIGHT_WORLD='running'\nSP_INFLIGHT_SEG='\u{2699} ?rounds'\n");
        assert!(Flight::from_snapshot(&dark, 700, 60).unread());
    }

    #[test]
    fn an_unreadable_kind_shows_its_question_mark() {
        let out = render("SP_INFLIGHT_SEG='\u{2699} ?rounds'\n");
        assert!(out[0].contains("?rounds"));
    }
}
