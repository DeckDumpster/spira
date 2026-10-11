//! A bead's declared target and the verdict a closing submission must carry for it.
//! Pure: the callers read the store and pass the facts in.

pub const TARGET_PREFIX: &str = "Target: ";
pub const VERDICT_PREFIX: &str = "TARGET VERDICT: ";

const OPS: &[&str] = &["<", "<=", ">", ">=", "=", "=="];

/// `<metric> <op> <value>`: an operator token with a metric before it and a value after it.
pub fn valid_target(t: &str) -> Result<(), String> {
    let words: Vec<&str> = t.split_whitespace().collect();
    match words.iter().position(|w| OPS.contains(w)) {
        Some(i) if i > 0 && i + 1 < words.len() => Ok(()),
        _ => Err(format!("--target {t:?} is not \"<metric> <op> <value>\" (op one of {})", OPS.join(" "))),
    }
}

pub fn parse_target(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.trim().strip_prefix(TARGET_PREFIX)).map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

pub fn parse_verdict(text: &str) -> Option<String> {
    text.lines().rev().find_map(|l| l.trim().strip_prefix(VERDICT_PREFIX)).map(|t| t.trim().to_string())
}

/// What the store knows about a bead named as a follow-on or an escalation.
pub struct Named {
    pub target: Option<String>,
    pub labels: Vec<String>,
}

const EXITS: &str = "the bead declares a target, so the submission must carry one of: \
--met \"<measured value and where measured>\"; \
--contradicted \"<the evidence>\"; \
--short \"<measured value>\" with --follow-on <bead id> (a bead you filed carrying the same target) or --escalation <id of an operator escalation ask>";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned().filter(|v| !v.trim().is_empty())
}

/// Err is the refusal text. Ok(None): the bead has no target. Ok(Some(line)): the verdict
/// line to record on the bead.
pub fn judge(target: Option<&str>, args: &[String], lookup: &dyn Fn(&str) -> Result<Option<Named>, String>, self_id: &str, ask_label: &str) -> Result<Option<String>, String> {
    let Some(target) = target else { return Ok(None) };
    let (met, contra, short) = (flag(args, "--met"), flag(args, "--contradicted"), flag(args, "--short"));
    let given = [&met, &contra, &short].iter().filter(|v| v.is_some()).count();
    if given == 0 {
        return Err(format!("refused: {EXITS} (target: {target})"));
    }
    if given > 1 {
        return Err(format!("refused: give exactly one of --met, --contradicted, --short ({EXITS})"));
    }
    if let Some(m) = met {
        return Ok(Some(format!("{VERDICT_PREFIX}MET [{target}] {m}")));
    }
    if let Some(c) = contra {
        return Ok(Some(format!("{VERDICT_PREFIX}CONTRADICTED [{target}] {c}")));
    }
    let measured = short.unwrap_or_default();
    let (follow, escalation) = (flag(args, "--follow-on"), flag(args, "--escalation"));
    match (follow, escalation) {
        (Some(id), None) => {
            if id == self_id {
                return Err(format!("refused: --follow-on {id} is this bead; file a new bead for the remainder ({EXITS})"));
            }
            match lookup(&id)? {
                None => Err(format!("refused: --follow-on {id} does not exist; file it first (work file-followup) ({EXITS})")),
                Some(n) if n.target.as_deref() != Some(target) => {
                    Err(format!("refused: --follow-on {id} does not carry this bead's target ({target}); file it with the same --target ({EXITS})"))
                }
                Some(_) => Ok(Some(format!("{VERDICT_PREFIX}SHORT [{target}] {measured} follow-on: {id}"))),
            }
        }
        (None, Some(id)) => match lookup(&id)? {
            None => Err(format!("refused: --escalation {id} does not exist ({EXITS})")),
            Some(n) if !n.labels.iter().any(|l| l == ask_label) => Err(format!("refused: --escalation {id} is not labelled {ask_label} ({EXITS})")),
            Some(_) => Ok(Some(format!("{VERDICT_PREFIX}SHORT [{target}] {measured} escalation: {id}"))),
        },
        _ => Err(format!("refused: --short needs exactly one of --follow-on or --escalation ({EXITS})")),
    }
}

/// Sift's reading of a recorded verdict: why a bead with a target may not proceed, if it may not.
pub fn gap(description_and_notes: &str) -> Option<String> {
    let target = parse_target(description_and_notes)?;
    match parse_verdict(description_and_notes) {
        None => Some(format!("declares target [{target}] but its submission carries no verdict (--met, --contradicted, or --short with --follow-on/--escalation)")),
        Some(v) if v.starts_with("SHORT ") && !v.contains(" follow-on: ") && !v.contains(" escalation: ") => {
            Some(format!("target [{target}] verdict is SHORT with no follow-on bead or escalation"))
        }
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASK: &str = "ask-fixture";
    const T: &str = "sim world wall < 10 s";

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn lookup(id: &str) -> Result<Option<Named>, String> {
        Ok(match id {
            "sp-next" => Some(Named { target: Some(T.into()), labels: vec![] }),
            "sp-other" => Some(Named { target: Some("x < 1".into()), labels: vec![] }),
            "sp-esc" => Some(Named { target: None, labels: vec![ASK.into()] }),
            "sp-plain" => Some(Named { target: None, labels: vec![] }),
            _ => None,
        })
    }

    fn judged(args: &[&str]) -> Result<Option<String>, String> {
        judge(Some(T), &a(args), &lookup, "sp-me", ASK)
    }

    #[test]
    fn a_bead_without_a_target_is_unaffected() {
        assert_eq!(judge(None, &a(&[]), &lookup, "sp-me", ASK), Ok(None));
    }

    #[test]
    fn no_verdict_is_refused_naming_all_three_exits() {
        let e = judged(&[]).unwrap_err();
        for exit in ["--met", "--contradicted", "--short", "--follow-on", "--escalation"] {
            assert!(e.contains(exit), "{e}");
        }
    }

    #[test]
    fn met_contradicted_and_short_with_a_follow_on_pass() {
        assert!(judged(&["--met", "9.2 s on the box"]).unwrap().unwrap().contains("MET"));
        assert!(judged(&["--contradicted", "floor is 12 s"]).unwrap().unwrap().contains("CONTRADICTED"));
        assert!(judged(&["--short", "37 s", "--follow-on", "sp-next"]).unwrap().unwrap().contains("follow-on: sp-next"));
        assert!(judged(&["--short", "37 s", "--escalation", "sp-esc"]).unwrap().unwrap().contains("escalation: sp-esc"));
    }

    #[test]
    fn short_is_refused_without_a_real_matching_exit() {
        assert!(judged(&["--short", "37 s"]).is_err());
        assert!(judged(&["--short", "37 s", "--follow-on", "sp-ghost"]).unwrap_err().contains("does not exist"));
        assert!(judged(&["--short", "37 s", "--follow-on", "sp-other"]).unwrap_err().contains("same --target"));
        assert!(judged(&["--short", "37 s", "--follow-on", "sp-me"]).is_err());
        assert!(judged(&["--short", "37 s", "--escalation", "sp-plain"]).unwrap_err().contains(ASK));
    }

    #[test]
    fn target_syntax() {
        assert!(valid_target(T).is_ok());
        assert!(valid_target("faster").is_err());
        assert!(valid_target("< 10").is_err());
    }

    #[test]
    fn sift_gap_reads_the_recorded_verdict() {
        let body = format!("{TARGET_PREFIX}{T}\n");
        assert!(gap("no target here").is_none());
        assert!(gap(&body).is_some());
        assert!(gap(&format!("{body}{VERDICT_PREFIX}MET [{T}] 9 s")).is_none());
        assert!(gap(&format!("{body}{VERDICT_PREFIX}SHORT [{T}] 37 s")).is_some());
        assert!(gap(&format!("{body}{VERDICT_PREFIX}SHORT [{T}] 37 s follow-on: sp-n")).is_none());
    }
}
