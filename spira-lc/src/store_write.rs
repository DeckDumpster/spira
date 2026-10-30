//! `bd-supersede <id> <succ>` and `bd-note <id> <text>` — the store-only writes the
//! supersede-duty handler makes after the lifecycle row has moved. Neither touches
//! `spira_lifecycle`: the row is the machine's verbs' to move, bd's side follows it.

use crate::callers::Answer;

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

fn refuse(verb: &str, why: &str) -> Answer {
    Answer { code: REFUSED, stderr: format!("spira-lc {verb}: {why}\n"), ..Default::default() }
}

use crate::cutover::is_row_key as bead_id;

fn call(verb: &str, args: &[&str], bd: &dyn Fn(&[&str]) -> Result<String, String>) -> Answer {
    match bd(args) {
        Ok(out) => Answer { stdout: out, ..Default::default() },
        Err(e) => Answer { code: 1, stderr: format!("spira-lc {verb}: {}\n", e.trim_end()), ..Default::default() },
    }
}

pub fn supersede(args: &[String], bd: &dyn Fn(&[&str]) -> Result<String, String>) -> Answer {
    let [id, succ] = args else {
        return Answer { code: CANNOT_TELL, stderr: "usage: spira-lc bd-supersede <bead-id> <successor-id>\n".into(), ..Default::default() };
    };
    if !bead_id(id) || !bead_id(succ) {
        return refuse("bd-supersede", "both arguments must be bead ids");
    }
    if id == succ {
        return refuse("bd-supersede", "a bead cannot supersede itself");
    }
    call("bd-supersede", &["supersede", id, "--with", succ], bd)
}

pub fn note(args: &[String], bd: &dyn Fn(&[&str]) -> Result<String, String>) -> Answer {
    let [id, text] = args else {
        return Answer { code: CANNOT_TELL, stderr: "usage: spira-lc bd-note <bead-id> <text>\n".into(), ..Default::default() };
    };
    if !bead_id(id) {
        return refuse("bd-note", "the first argument must be a bead id");
    }
    if text.trim().is_empty() {
        return refuse("bd-note", "empty note");
    }
    call("bd-note", &["note", id, text], bd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    fn recording(calls: &RefCell<Vec<String>>) -> impl Fn(&[&str]) -> Result<String, String> + '_ {
        move |a| {
            calls.borrow_mut().push(a.join(" "));
            Ok(String::new())
        }
    }

    #[test]
    fn supersede_calls_bd_with_the_successor() {
        let calls = RefCell::new(vec![]);
        let ans = supersede(&v(&["sp-a", "sp-b"]), &recording(&calls));
        assert_eq!(ans.code, 0);
        assert_eq!(*calls.borrow(), ["supersede sp-a --with sp-b"]);
    }

    #[test]
    fn supersede_refuses_bad_ids_and_self_before_calling_bd() {
        let never = |_: &[&str]| -> Result<String, String> { panic!("bd must not run") };
        for bad in [&["sp-a", "sp-a"][..], &["-x", "sp-b"], &["sp-a", ""], &["sp-a", "--force"], &["sp a", "sp-b"], &["/etc/passwd", "sp-b"]] {
            assert_eq!(supersede(&v(bad), &never).code, REFUSED, "{bad:?}");
        }
        assert_eq!(supersede(&v(&["sp-a"]), &never).code, CANNOT_TELL);
    }

    #[test]
    fn note_passes_the_text_as_one_argument() {
        let calls = RefCell::new(vec![]);
        let ans = note(&v(&["sp-a", "supersede-request confirmed: sp-b is LANDED"]), &recording(&calls));
        assert_eq!(ans.code, 0);
        assert_eq!(*calls.borrow(), ["note sp-a supersede-request confirmed: sp-b is LANDED"]);
    }

    #[test]
    fn note_refuses_empty_text_and_option_ids_before_calling_bd() {
        let never = |_: &[&str]| -> Result<String, String> { panic!("bd must not run") };
        assert_eq!(note(&v(&["sp-a", "  "]), &never).code, REFUSED);
        assert_eq!(note(&v(&["--x", "t"]), &never).code, REFUSED);
        assert_eq!(note(&v(&["sp-a"]), &never).code, CANNOT_TELL);
    }

    #[test]
    fn a_failing_bd_is_exit_1_with_its_message() {
        let down = |_: &[&str]| -> Result<String, String> { Err("bd down\n".into()) };
        let ans = supersede(&v(&["sp-a", "sp-b"]), &down);
        assert_eq!((ans.code, ans.stderr.as_str()), (1, "spira-lc bd-supersede: bd down\n"));
        assert_eq!(note(&v(&["sp-a", "t"]), &down).code, 1);
    }
}
