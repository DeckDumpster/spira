//! The Goodput page: the ops pane's top line and section rows, from the same functions.

use crate::shell::shell;
use crate::stuck::esc;
use cockpit_ops::health::goodput::{view, PHONE_COLS};
use cockpit_ops::health::model::Snapshot;
use serde_json::{json, Value};

pub fn render_json(snap: &Snapshot, deadman_secs: i64) -> Value {
    let v = view(snap, PHONE_COLS, deadman_secs);
    json!({ "stale": v.stale, "top": v.top, "sections": v.rows })
}

pub fn render_page(snap: &Snapshot, deadman_secs: i64) -> String {
    let v = view(snap, PHONE_COLS, deadman_secs);
    let class = if v.stale { "dim" } else { "" };
    let block = |lines: &[String]| format!("<pre class=\"{class}\">{}</pre>", esc(&lines.join("\n")));
    shell("goodput", "Goodput", 15, &format!("<header><h1>Goodput</h1></header><main>{}{}</main>", block(&v.top), block(&v.rows)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cockpit_ops::health::goodput::{sections_section, top_lines};

    const FIXTURE: &str = "SP_GP_RATE='3.2'\nSP_GP_TARGET='5'\nSP_GP_PASS_AGE_S='14'\nSP_GP_BINDING='round-controller/pokebot'\n\
        SP_GP_DISTANCE='1 repeat identity open'\nSP_GP_STEP='hold-member b-17@3f2a'\nSP_GP_STEP_ACK='14:02'\nSP_GP_STEP_HORIZON='10m'\n\
        SP_GP_SEC_N='3'\nSP_GP_SEC0_REF='round-controller/pokebot'\nSP_GP_SEC0_DIST='1'\nSP_GP_SEC0_AGE='12'\nSP_GP_SEC0_FRESH='fresh'\n\
        SP_GP_SEC1_REF='lifecycle/all'\nSP_GP_SEC1_DIST='0'\nSP_GP_SEC1_AGE='40'\nSP_GP_SEC1_FRESH='fresh'\n\
        SP_GP_SEC2_REF='alarms/all'\nSP_GP_SEC2_DIST='2'\nSP_GP_SEC2_AGE='7'\nSP_GP_SEC2_FRESH='fresh'\n";

    fn strip_ansi(s: &str) -> String {
        let mut it = s.chars();
        let mut out = String::new();
        while let Some(c) = it.next() {
            match c {
                '\x1b' => it.by_ref().take_while(|n| !n.is_ascii_alphabetic()).for_each(drop),
                c => out.push(c),
            }
        }
        out
    }

    fn lines(v: &Value, key: &str) -> Vec<String> {
        v[key].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn the_page_serves_the_lines_the_pane_prints() {
        let snap = Snapshot::parse(FIXTURE);
        let v = render_json(&snap, 300);
        let pane = |f: fn(&Snapshot, i64, i64) -> Vec<String>| f(&snap, PHONE_COLS, 300).iter().map(|l| strip_ansi(l)).collect::<Vec<_>>();
        assert_eq!(lines(&v, "top"), pane(top_lines));
        assert_eq!(lines(&v, "sections"), pane(sections_section));
        let joined = lines(&v, "top").join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(joined.contains("step `hold-member b-17@3f2a` acked 14:02, horizon 10m"), "{v}");
    }

    #[test]
    fn a_stale_pass_is_marked_dim_with_its_age_on_the_page() {
        let stale = Snapshot::parse(&FIXTURE.replace("AGE_S='14'", "AGE_S='900'"));
        assert_eq!(render_json(&stale, 300)["stale"], true);
        assert!(lines(&render_json(&stale, 300), "top").join(" ").contains("stale pass, 900s old"));
        assert!(render_page(&stale, 300).contains("<pre class=\"dim\">"));
        assert!(render_page(&Snapshot::parse(FIXTURE), 300).contains("<pre class=\"\">"));
    }

    #[test]
    fn without_a_pass_record_the_page_reads_question_marks_never_zero() {
        let none = render_json(&Snapshot::parse(""), 300);
        let text = lines(&none, "top").join(" ") + &lines(&none, "sections").join(" ");
        assert!(text.contains("GOODPUT ?/day of ?") && text.contains("pass ?") && !text.contains('0'), "{text}");
    }

    #[test]
    fn no_line_on_the_page_is_wider_than_a_phone() {
        let v = render_json(&Snapshot::parse(&FIXTURE.replace("AGE_S='14'", "AGE_S='900'")), 300);
        let all: Vec<String> = lines(&v, "top").into_iter().chain(lines(&v, "sections")).collect();
        assert!(all.len() > 6, "{all:?}");
        assert!(all.iter().all(|l| l.chars().count() as i64 <= PHONE_COLS), "{all:?}");
    }
}
