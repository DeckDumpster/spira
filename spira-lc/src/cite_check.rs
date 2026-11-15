//! `cite-check --repo <path> --base <ref> --branch <ref>`: the landing gate's bead-citation
//! check. The rule is `spira_config::cite`, the same one `work submit` applies.

use crate::{flag, CANNOT_TELL};

/// `(sha, message)` for each record of `git log -z --format=%H%n%B`.
fn parse_log(out: &str) -> Vec<(String, String)> {
    out.split('\0')
        .filter(|r| !r.trim().is_empty())
        .map(|r| {
            let r = r.trim_start_matches('\n');
            let (sha, msg) = r.split_once('\n').unwrap_or((r, ""));
            (sha.to_string(), msg.to_string())
        })
        .collect()
}

/// The sha a lone commit message is shown under: it has none yet.
const THIS_MESSAGE: &str = "this message";

/// Judges one commit message that is not yet a commit.
pub fn message_verdict(message: &str, prefix: &str, known: &mut spira_config::cite::Lookup<'_>) -> (i32, String) {
    crate::work::phantom_refusal(&[(THIS_MESSAGE.to_string(), message.to_string())], prefix, known).unwrap_or((0, String::new()))
}

pub fn run(args: &[String]) -> (i32, String) {
    if flag(args, "--message").as_deref() == Some("-") {
        let mut message = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut message) {
            return (CANNOT_TELL, format!("cannot tell: reading the message on stdin: {e}"));
        }
        let prefix = match spira_config::process::cfg("SPIRA_ID_PREFIX") {
            Ok(p) => p,
            Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
        };
        return message_verdict(&message, &prefix, &mut |ids| crate::callers::Bd::known(&mut crate::bd::LiveBd, ids));
    }
    let (Some(repo), Some(base), Some(branch)) = (flag(args, "--repo"), flag(args, "--base"), flag(args, "--branch")) else {
        return (CANNOT_TELL, "cite-check: usage: cite-check --repo <path> --base <ref> --branch <ref> | cite-check --message -".into());
    };
    let prefix = match spira_config::process::cfg("SPIRA_ID_PREFIX") {
        Ok(p) => p,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let out = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(&repo)
        .args(["log", "-z", "--format=%H%n%B", &format!("{base}..{branch}")])
        .output();
    let commits = match out {
        Ok(o) if o.status.success() => parse_log(&String::from_utf8_lossy(&o.stdout)),
        Ok(o) => return (CANNOT_TELL, format!("cannot tell: {base} or {branch} does not resolve in {repo}: {}", String::from_utf8_lossy(&o.stderr).trim_end())),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: git: {e}")),
    };
    crate::work::phantom_refusal(&commits, &prefix, &mut |ids| crate::callers::Bd::known(&mut crate::bd::LiveBd, ids))
        .unwrap_or((0, String::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verb_exits_0_for_clean_and_fixture_ids_3_for_a_phantom_and_2_for_a_silent_store() {
        let held = ["sp-real1".to_string()];
        let mut store = |ids: &[String]| Ok(ids.iter().filter(|i| held.contains(i)).cloned().collect());
        let run = |msg: &str, known: &mut spira_config::cite::Lookup<'_>| {
            crate::work::phantom_refusal(&[("0123456789abcdef".into(), msg.into())], "sp", known).map_or(0, |(c, _)| c)
        };
        assert_eq!(run("sp-real1: x\n\nanswers sp-a and sp-b", &mut store), 0);
        assert_eq!(run("sp-real1: x\n\nsee sp-zzzzzz", &mut store), crate::REFUSED);
        assert_eq!(run("sp-real1: x", &mut |_| Err("down".into())), crate::CANNOT_TELL);
    }

    #[test]
    fn the_log_splits_into_sha_and_whole_message() {
        let got = parse_log("aaa\nsubject\n\nbody sp-abc\n\0bbb\nother\n\0");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "aaa");
        assert!(got[0].1.contains("body sp-abc"));
        assert_eq!(got[1], ("bbb".to_string(), "other\n".to_string()));
    }
}
