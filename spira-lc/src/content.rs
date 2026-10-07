//! `content <bd-subcommand> …` — the one door for the cockpit's bead-content reads and its
//! label/comment writes, so nothing under `cockpit/` or `cockpit-collect/` invokes `bd`.
//! State is never served here: the machine's own verbs answer that. Anything outside the
//! allowlist is refused.

use crate::callers::Answer;

const REFUSED: i32 = 3;
const CANNOT_TELL: i32 = 2;

pub fn check(args: &[String]) -> Result<(), String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["list" | "show" | "memories" | "state", ..] => Ok(()),
        ["gate", "list", ..] => Ok(()),
        ["close", id, rest @ ..] if !id.starts_with('-') && close_flags(rest) => Ok(()),
        ["comments", "add", id, _text] if !id.starts_with('-') => Ok(()),
        ["comments", sub, ..] if *sub != "add" => Ok(()),
        ["update", id, flags @ ..] if !id.starts_with('-') && !flags.is_empty() => labels_only(flags),
        _ => Err(format!("`{}` is not a content verb", a.join(" "))),
    }
}

fn close_flags(rest: &[&str]) -> bool {
    match rest {
        ["--reason", r, "--force"] | ["--force", "--reason", r] => !r.starts_with("--"),
        _ => false,
    }
}

fn labels_only(flags: &[&str]) -> Result<(), String> {
    let mut it = flags.iter();
    while let Some(f) = it.next() {
        match (*f, it.next()) {
            ("--add-label" | "--remove-label", Some(l)) if !l.is_empty() => {}
            _ => return Err(format!("update takes only --add-label/--remove-label <label>, not `{f}`")),
        }
    }
    Ok(())
}

pub fn run(args: &[String], bd: &dyn Fn(&[String]) -> Result<String, String>) -> Answer {
    if args.is_empty() {
        return Answer { code: CANNOT_TELL, stderr: "usage: spira-lc content <list|show|comments|gate list|memories|state|update|comments add|close --reason R --force> …\n".into(), ..Default::default() };
    }
    if let Err(e) = check(args) {
        return Answer { code: REFUSED, stderr: format!("spira-lc content: {e}\n"), ..Default::default() };
    }
    match bd(args) {
        Ok(out) => Answer { stdout: out, ..Default::default() },
        Err(e) => Answer { code: 1, stderr: format!("{}\n", e.trim_end()), ..Default::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn reads_and_label_comment_writes_pass() {
        for ok in [
            &["list", "--all", "--json"][..],
            &["show", "sp-a", "--json"],
            &["comments", "sp-a", "--json"],
            &["comments", "add", "sp-a", "text"],
            &["close", "sp-a", "--reason", "r", "--force"],
            &["close", "sp-a", "--force", "--reason", "r"],
            &["gate", "list", "--json"],
            &["update", "sp-a", "--add-label", "x", "--remove-label", "y"],
            &["state", "sp-a", "fayth"],
            &["memories", "--json"],
        ] {
            assert!(check(&v(ok)).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn every_other_write_is_refused() {
        for bad in [
            &["close", "sp-a"][..],
            &["close", "sp-a", "--reason", "r"],
            &["update", "sp-a", "--status", "open"],
            &["update", "sp-a", "--add-label"],
            &["update", "sp-a"],
            &["create", "t"],
            &["comments", "add", "sp-a"],
            &["gate", "resolve", "x"],
            &[][..],
        ] {
            assert!(check(&v(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn run_refuses_before_calling_bd() {
        let ans = run(&v(&["close", "sp-a"]), &|_| panic!("bd must not run"));
        assert_eq!(ans.code, REFUSED);
        let ans = run(&v(&["show", "sp-a"]), &|a| Ok(format!("ran {}", a.join(" "))));
        assert_eq!((ans.code, ans.stdout.as_str()), (0, "ran show sp-a"));
    }
}
