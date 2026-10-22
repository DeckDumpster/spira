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
            "SP_INFLIGHT_SEG='\u{2699} 1 aeon \u{b7} 1 round (5/10) \u{b7} 1 check'\n\
             SP_INFLIGHT_ROW0='aeon|sp-1|guardian|100||'\nSP_INFLIGHT_ROW1='round|spira/b1|certifying|100|5|10'\n\
             SP_INFLIGHT_ROW2='check|spira-checks-forge-prod.service|pre-check|?||'\n",
        );
        assert_eq!(out.len(), 4);
        assert!(out[0].contains("1 aeon") && out[0].contains("1 check"));
        assert!(out[1].contains("sp-1") && out[1].contains("guardian") && out[1].contains("10m"));
        assert!(out[2].contains("\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}") && out[2].contains("5/10"));
        assert!(out[3].contains("pre-check") && out[3].contains("?"));
    }

    #[test]
    fn idle_is_one_line_and_an_absent_key_is_not_idle() {
        assert_eq!(render("SP_INFLIGHT_SEG='\u{2699} idle'\n"), vec!["\u{2699} idle"]);
        let missing = render("SP_OPEN='1'\n");
        assert!(missing[0].contains('?') && !missing[0].contains("idle"));
    }

    #[test]
    fn an_unreadable_kind_shows_its_question_mark() {
        let out = render("SP_INFLIGHT_SEG='\u{2699} ?rounds'\n");
        assert!(out[0].contains("?rounds"));
    }
}
