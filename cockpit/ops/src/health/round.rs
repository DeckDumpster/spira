//! ROUND — the round in flight (or the last verdict and the pool when none is open), from the
//! `SP_ROUND_*` keys the collector reads off the lifecycle batch machine. Elapsed time is
//! computed here from the open time, not carried in the snapshot, so it never freezes between
//! collector passes.

use super::colors::*;
use super::fmt::{fit, mail_dur};
use super::model::Snapshot;

pub const PHONE_COLS: i64 = 60;
const AMBER_PCT: i64 = 80;

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

fn rows(snap: &Snapshot, prefix: &str) -> Vec<String> {
    (0..).map_while(|i| snap.get(&format!("{prefix}{i}")).filter(|s| !s.is_empty())).map(str::to_string).collect()
}

pub fn round_section(snap: &Snapshot, cols: i64, now: i64) -> Vec<String> {
    let phone = cols < PHONE_COLS;
    let pool = snap.q("SP_ROUND_POOL");
    match snap.get("SP_ROUND_STATE") {
        Some("open") => open_rows(snap, cols, now, phone),
        Some("idle") => vec![idle_row(snap, now, phone, pool)],
        _ => vec![format!(" {DIM}ROUND{RST}  {BAD}?{RST} {DIM}cannot read the round{RST}")],
    }
}

fn open_rows(snap: &Snapshot, cols: i64, now: i64, phone: bool) -> Vec<String> {
    let name = snap.q("SP_ROUND_NAME");
    let phase = snap.q("SP_ROUND_PHASE");
    let cap = num(snap, "SP_ROUND_CAP").unwrap_or(0);
    let wall = match num(snap, "SP_ROUND_OPENED") {
        Some(opened) => {
            let e = (now - opened).max(0);
            format!("{}{e}s/{cap}s{RST}", wall_colour(e, cap))
        }
        None => format!("{BAD}?/{cap}s{RST}"),
    };
    let n = snap.q("SP_ROUND_N");
    let ejected = rows(snap, "SP_ROUND_EJECT");
    if phone {
        let out = if ejected.is_empty() { String::new() } else { format!(" {WARN}-{}{RST}", ejected.len()) };
        return vec![format!(" {DIM}ROUND{RST} {B}{name}{RST} {phase} {wall} {n}m{out}")];
    }
    let mut out = vec![format!(" {DIM}ROUND{RST}  {B}{name}{RST}  {ACC}{phase}{RST}  {wall}  {n} member(s)")];
    for raw in rows(snap, "SP_ROUND_MEMBER") {
        let (id, state) = pair(&raw);
        out.push(format!("        {id}  {DIM}{state}{RST}"));
    }
    for raw in &ejected {
        let (id, why) = pair(raw);
        out.push(format!("        {WARN}ejected{RST} {id}  {DIM}{}{RST}", fit(why, cols - 20)));
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
    format!(" {DIM}ROUND{RST}  {DIM}idle{RST}  {last}  {DIM}pool{RST} {B}{pool}{RST}{DIM}{waiting}{RST}")
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
        round_section(&Snapshot::parse(snap), cols, now)
    }

    fn open(phase: &str) -> String {
        format!(
            "SP_ROUND_STATE='open'\nSP_ROUND_NAME='r-9'\nSP_ROUND_PHASE='{phase}'\nSP_ROUND_OPENED='1000'\nSP_ROUND_CAP='900'\nSP_ROUND_N='2'\n\
             SP_ROUND_MEMBER0='sp-a|in'\nSP_ROUND_MEMBER1='sp-b|in'\nSP_ROUND_EJECT_N='1'\nSP_ROUND_EJECT0='sp-c|red: test-x.sh'\nSP_ROUND_POOL='3'\n"
        )
    }

    #[test]
    fn every_phase_renders_its_name_members_and_ejection() {
        for phase in ["merging", "certifying", "landing", "deploying"] {
            let out = render(&open(phase), 100, 1100);
            let text = strip(&out.join("\n"));
            assert!(text.contains(&format!("r-9  {phase}  100s/900s  2 member(s)")), "{text}");
            assert!(text.contains("sp-a  in") && text.contains("sp-b  in"), "{text}");
            assert!(text.contains("ejected sp-c  red: test-x.sh"), "{text}");
        }
    }

    #[test]
    fn wall_is_green_then_amber_past_720_then_red_past_the_cap() {
        let at = |now: i64| render(&open("certifying"), 100, now)[0].clone();
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
        assert!(strip(&render(&open("merging"), 40, 1100)[0]).contains("r-9 merging 100s/900s 2m -1"));
    }

    #[test]
    fn an_unreadable_round_is_a_question_mark_never_idle() {
        for snap in ["", "SP_ROUND_STATE='?'\n"] {
            let t = strip(&render(snap, 100, 1000)[0]);
            assert!(t.contains("?") && !t.contains("idle"), "{t}");
        }
    }
}
