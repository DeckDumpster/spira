//! ROUNDS — each live round as one collapsed row, the one on the VM expanded to its members when
//! the pane has room; with none live, the last verdict and the pool. Read from the `SP_ROUNDS*`
//! keys the collector takes off the lifecycle batch machine. Elapsed time is computed here from
//! the open time, so it never freezes between collector passes.

use super::colors::*;
use super::fmt::{fit, mail_dur};
use super::model::Snapshot;

pub const PHONE_COLS: i64 = 60;
const AMBER_PCT: i64 = 80;
const MAX_ROUNDS: usize = 4;
const EXPAND_MIN_ROWS: i64 = 30;

fn num(snap: &Snapshot, key: &str) -> Option<i64> {
    snap.get(key)?.parse().ok()
}

fn wall_colour(elapsed: i64, cap: i64) -> &'static str {
    if cap > 0 && elapsed > cap {
        BAD
    } else if cap > 0 && elapsed * 100 > cap * AMBER_PCT {
        WARN
    } else {
        OK
    }
}

fn pair(raw: &str) -> (&str, &str) {
    raw.split_once('|').unwrap_or((raw, ""))
}

fn rows_of(snap: &Snapshot, prefix: &str) -> Vec<String> {
    (0..).map_while(|i| snap.get(&format!("{prefix}{i}")).filter(|s| !s.is_empty())).map(str::to_string).collect()
}

pub fn round_section(snap: &Snapshot, cols: i64, now: i64, rows: i64) -> Vec<String> {
    let phone = cols < PHONE_COLS;
    let pool = snap.q("SP_ROUND_POOL");
    match snap.get("SP_ROUND_STATE") {
        Some("open") => open_rows(snap, cols, now, phone, rows),
        Some("idle") => vec![idle_row(snap, now, phone, pool)],
        _ => vec![format!(" {DIM}ROUNDS{RST}  {BAD}?{RST} {DIM}cannot read the rounds{RST}")],
    }
}

fn open_rows(snap: &Snapshot, cols: i64, now: i64, phone: bool, rows: i64) -> Vec<String> {
    let cap = num(snap, "SP_ROUND_CAP").unwrap_or(0);
    let count = num(snap, "SP_ROUNDS_N").unwrap_or(0).max(0) as usize;
    let shown = count.min(MAX_ROUNDS);
    let room = rows <= 0 || rows >= EXPAND_MIN_ROWS;
    let on_vm = (0..shown).find(|i| snap.q(&format!("SP_ROUNDS{i}_PHASE")) == "certifying");
    let mut out = Vec::new();
    for i in 0..shown {
        let k = format!("SP_ROUNDS{i}");
        let name = snap.q(&format!("{k}_NAME"));
        let phase = snap.q(&format!("{k}_PHASE"));
        let n = snap.q(&format!("{k}_N"));
        let ejected = rows_of(snap, &format!("{k}_EJECT"));
        let label = if i == 0 { format!("{DIM}ROUNDS{RST}") } else { "      ".to_string() };
        let wall = match (phase == "certifying", num(snap, &format!("{k}_OPENED"))) {
            (false, _) => String::new(),
            (true, Some(opened)) => {
                let e = (now - opened).max(0);
                format!("  {}{e}s/{cap}s{RST}", wall_colour(e, cap))
            }
            (true, None) => format!("  {BAD}?/{cap}s{RST}"),
        };
        if phone {
            let out_n = if ejected.is_empty() { String::new() } else { format!(" {WARN}-{}{RST}", ejected.len()) };
            out.push(format!(" {label} {B}{name}{RST} {phase}{}{} {n}m{out_n}", wall.trim_end(), RST));
            continue;
        }
        let open = on_vm == Some(i) && room;
        let mark = if open { "\u{25be}" } else { "\u{25b8}" };
        out.push(format!(" {label}  {mark} {B}{name}{RST}  {ACC}{phase}{RST}{wall}  {n} member(s){}", if ejected.is_empty() { String::new() } else { format!("  {WARN}-{}{RST}", ejected.len()) }));
        if !open {
            continue;
        }
        for raw in rows_of(snap, &format!("{k}_MEMBER")) {
            let (id, state) = pair(&raw);
            out.push(format!("          {id}  {DIM}{}{RST}", fit(state, cols - 20)));
        }
        for raw in &ejected {
            let (id, why) = pair(raw);
            out.push(format!("          {WARN}ejected{RST} {id}  {DIM}{}{RST}", fit(why, cols - 28)));
        }
    }
    if count > shown {
        out.push(format!("        {DIM}+{} more{RST}", count - shown));
    }
    out
}

fn idle_row(snap: &Snapshot, now: i64, phone: bool, pool: &str) -> String {
    let last = match snap.get("SP_ROUND_LAST_VERDICT") {
        Some(v) => {
            let name = snap.q("SP_ROUND_LAST_NAME");
            let ago = num(snap, "SP_ROUND_LAST_AT").map_or("?".to_string(), |t| mail_dur(&(now - t).max(0).to_string()));
            let (col, word) = if v == "GREEN" { (OK, "GREEN landed") } else { (BAD, "RED") };
            let why = snap.get("SP_ROUND_LAST_WHY").filter(|s| !s.is_empty() && !phone).map(|w| format!(" {DIM}{w}{RST}")).unwrap_or_default();
            format!("last {name} {col}{word}{RST}{why} {DIM}{ago} ago{RST}")
        }
        None => format!("{DIM}no round yet{RST}"),
    };
    let waiting = if phone { "" } else { " waiting" };
    format!(" {DIM}ROUNDS{RST}  {DIM}idle{RST}  {last}  {DIM}pool{RST} {B}{pool}{RST}{DIM}{waiting}{RST}")
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

    fn render(snap: &str, cols: i64, now: i64) -> Vec<String> {
        round_section(&Snapshot::parse(snap), cols, now, 0)
    }

    fn round(k: usize, name: &str, phase: &str) -> String {
        format!(
            "SP_ROUNDS{k}_NAME='{name}'\nSP_ROUNDS{k}_PHASE='{phase}'\nSP_ROUNDS{k}_OPENED='1000'\nSP_ROUNDS{k}_N='2'\n\
             SP_ROUNDS{k}_MEMBER0='sp-a|in'\nSP_ROUNDS{k}_MEMBER1='sp-b|skipped: Conflict'\nSP_ROUNDS{k}_EJECT_N='1'\nSP_ROUNDS{k}_EJECT0='sp-c|red: test-x.sh'\n"
        )
    }

    fn open(phase: &str) -> String {
        format!("SP_ROUND_STATE='open'\nSP_ROUND_CAP='900'\nSP_ROUND_POOL='3'\nSP_ROUNDS_N='1'\n{}", round(0, "r-9", phase))
    }

    fn two() -> String {
        format!("SP_ROUND_STATE='open'\nSP_ROUND_CAP='900'\nSP_ROUNDS_N='2'\n{}{}", round(0, "r-11", "staged"), round(1, "r-10", "certifying"))
    }

    #[test]
    fn the_round_on_the_vm_expands_to_members_and_ejections() {
        let text = strip(&render(&open("certifying"), 100, 1100).join("\n"));
        assert!(text.contains("ROUNDS") && text.contains("r-9  certifying  100s/900s  2 member(s)"), "{text}");
        assert!(text.contains("sp-a  in") && text.contains("sp-b  skipped: Conflict"), "{text}");
        assert!(text.contains("ejected sp-c  red: test-x.sh"), "{text}");
    }

    #[test]
    fn every_other_phase_is_one_collapsed_row() {
        for phase in ["staged", "merging", "landing", "deploying"] {
            let out = render(&open(phase), 100, 1100);
            assert_eq!(out.len(), 1, "{phase}");
            assert!(strip(&out[0]).contains(&format!("r-9  {phase}  2 member(s)")), "{}", out[0]);
        }
    }

    #[test]
    fn two_rounds_are_two_named_rows_and_only_the_vm_one_expands() {
        let out = render(&two(), 100, 1100);
        let text = strip(&out.join("\n"));
        assert!(strip(&out[0]).contains("ROUNDS") && strip(&out[0]).contains("r-11  staged"), "{text}");
        assert!(text.contains("r-10  certifying"), "{text}");
        assert_eq!(text.matches("sp-a  in").count(), 1, "{text}");
    }

    #[test]
    fn a_short_pane_keeps_every_round_collapsed() {
        let snap = Snapshot::parse(&two());
        assert_eq!(round_section(&snap, 100, 1100, 20).len(), 2);
        assert!(round_section(&snap, 100, 1100, 40).len() > 2);
    }

    #[test]
    fn rounds_beyond_the_cap_are_counted_not_listed() {
        let mut snap = String::from("SP_ROUND_STATE='open'\nSP_ROUND_CAP='900'\nSP_ROUNDS_N='6'\n");
        for k in 0..4 { snap.push_str(&round(k, &format!("r-{k}"), "staged")); }
        let text = strip(&render(&snap, 100, 1100).join("\n"));
        assert!(text.contains("+2 more") && !text.contains("r-4"), "{text}");
    }

    #[test]
    fn wall_is_green_then_amber_past_720_then_red_past_the_cap() {
        let quiet = open("certifying").replace("EJECT0=", "GONE0=");
        let at = |now: i64| render(&quiet, 100, now)[0].clone();
        assert!(at(1000 + 720).contains(OK) && !at(1000 + 720).contains(WARN));
        assert!(at(1000 + 721).contains(WARN) && !at(1000 + 721).contains(BAD));
        assert!(at(1000 + 900).contains(WARN));
        assert!(at(1000 + 901).contains(BAD), "{}", at(1000 + 901));
    }

    #[test]
    fn a_planted_over_cap_round_renders_red() {
        let out = render(&open("certifying"), 100, 1000 + 2000);
        assert!(out[0].contains(&format!("{BAD}2000s/900s")), "{}", out[0]);
    }

    #[test]
    fn no_open_round_shows_the_last_verdict_and_the_pool() {
        let green = strip(&render("SP_ROUND_STATE='idle'\nSP_ROUND_LAST_NAME='r-8'\nSP_ROUND_LAST_VERDICT='GREEN'\nSP_ROUND_LAST_AT='700'\nSP_ROUND_POOL='4'\n", 100, 1000)[0]);
        assert!(green.contains("idle") && green.contains("last r-8 GREEN landed 5m ago") && green.contains("pool 4 waiting"), "{green}");
        let red = strip(&render("SP_ROUND_STATE='idle'\nSP_ROUND_LAST_NAME='r-7'\nSP_ROUND_LAST_VERDICT='RED'\nSP_ROUND_LAST_WHY='red: test-a.sh'\nSP_ROUND_LAST_AT='0'\nSP_ROUND_POOL='0'\n", 100, 600)[0]);
        assert!(red.contains("r-7 RED red: test-a.sh 10m ago") && red.contains("pool 0 waiting"), "{red}");
    }

    #[test]
    fn phone_width_is_one_line() {
        assert_eq!(render(&open("merging"), 40, 1100).len(), 1);
        assert_eq!(render("SP_ROUND_STATE='idle'\nSP_ROUND_POOL='1'\n", 40, 1100).len(), 1);
        assert!(strip(&render(&open("merging"), 40, 1100)[0]).contains("r-9 merging 2m -1"));
    }

    #[test]
    fn an_unreadable_round_is_a_question_mark_never_idle() {
        for snap in ["", "SP_ROUND_STATE='?'\n"] {
            let t = strip(&render(snap, 100, 1000)[0]);
            assert!(t.contains("?") && !t.contains("idle"), "{t}");
        }
    }
}
