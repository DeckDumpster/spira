//! CHECK 4's decision (`check4_decide`, lib.sh), as a pure function. DESIGN.md §2 `decide`.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Thresholds {
    pub poison_at: u32,
    pub requeue_at: u32,
    pub reclaim_at: u32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds { poison_at: 3, requeue_at: 5, reclaim_at: 5 }
    }
}

/// `<requeue-asked>:<reclaim-asked>:<poison-asked-at-n>[:<poison-lifted-at-or-above-n>]`.
/// Each field is "1" for true; anything else (missing, empty, 0) is false — bash's
/// `[ "${x:-0}" != 1 ]` reading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct AskedStamp {
    pub requeue: bool,
    pub reclaim: bool,
    pub poison_at_n: bool,
    pub lifted_at_or_above_n: bool,
}

impl AskedStamp {
    pub fn parse(s: &str) -> AskedStamp {
        let f: Vec<&str> = s.split(':').collect();
        let one = |i: usize| f.get(i).map(|x| *x == "1").unwrap_or(false);
        AskedStamp { requeue: one(0), reclaim: one(1), poison_at_n: one(2), lifted_at_or_above_n: one(3) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Token {
    RequeueMail,
    ReclaimMail,
    Poison,
    Clear,
    Ask,
}

impl Token {
    pub fn as_str(self) -> &'static str {
        match self {
            Token::RequeueMail => "requeue-mail",
            Token::ReclaimMail => "reclaim-mail",
            Token::Poison => "poison",
            Token::Clear => "clear",
            Token::Ask => "ask",
        }
    }
}

pub struct Inputs<'a> {
    pub attempts: u32,
    pub requeues: u32,
    pub reclaims: u32,
    /// Comma-separated labels; only `delivers:action` is read (substring, as bash's case).
    pub labels: &'a str,
    pub stamp: AskedStamp,
    /// The caller's read of the spira-lc poison hold.
    pub poisoned: bool,
}

/// Each token is decided independently (a bead over one cap is never exempt from another).
pub fn decide(i: &Inputs, t: Thresholds) -> Vec<Token> {
    let mut out = Vec::new();
    let n = i.attempts;
    if !i.labels.contains("delivers:action") && i.requeues >= t.requeue_at && !i.stamp.requeue {
        out.push(Token::RequeueMail);
    }
    if i.reclaims >= t.reclaim_at && !i.stamp.reclaim {
        out.push(Token::ReclaimMail);
    }
    if i.poisoned {
        if n < t.poison_at {
            out.push(Token::Clear);
        }
    } else if n >= t.poison_at && n > 0 && !i.stamp.lifted_at_or_above_n {
        out.push(Token::Poison);
    }
    if n >= t.poison_at && n > 0 && !i.stamp.poison_at_n {
        out.push(Token::Ask);
    }
    out
}

pub fn render(tokens: &[Token]) -> String {
    if tokens.is_empty() {
        "none".to_string()
    } else {
        tokens.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(n: u32, rq: u32, rc: u32, labels: &str, stamp: &str, poisoned: bool) -> String {
        render(&decide(
            &Inputs { attempts: n, requeues: rq, reclaims: rc, labels, stamp: AskedStamp::parse(stamp), poisoned },
            Thresholds::default(),
        ))
    }

    #[test]
    fn decide_below_threshold_none() {
        assert_eq!(run(2, 0, 0, "", "0:0:0:0", false), "none");
        assert_eq!(run(0, 0, 0, "", "", false), "none");
    }

    #[test]
    fn decide_poison_at_threshold() {
        assert_eq!(run(3, 0, 0, "", "0:0:0:0", false), "poison ask");
        assert_eq!(run(7, 0, 0, "", "0:0:0", false), "poison ask");
    }

    #[test]
    fn decide_already_poisoned_no_poison_token() {
        // Held, still over: no poison, no clear; the ask is its own dedup.
        assert_eq!(run(3, 0, 0, "", "0:0:1:0", true), "none");
        assert_eq!(run(3, 0, 0, "", "0:0:0:0", true), "ask");
    }

    #[test]
    fn decide_clear_below_threshold_when_poisoned() {
        assert_eq!(run(1, 0, 0, "", "1:1:1", true), "clear");
        assert_eq!(run(0, 0, 0, "", "1:1:1", true), "clear");
    }

    #[test]
    fn poison_at_zero_needs_a_charged_attempt() {
        let t = Thresholds { poison_at: 0, ..Thresholds::default() };
        let d = |n| render(&decide(&Inputs { attempts: n, requeues: 0, reclaims: 0, labels: "", stamp: AskedStamp::default(), poisoned: false }, t));
        assert_eq!(d(0), "none");
        assert_eq!(d(1), "poison ask");
    }

    #[test]
    fn ask_suppressed_by_stamp() {
        assert_eq!(run(3, 0, 0, "", "0:0:1:0", false), "poison");
    }

    #[test]
    fn ask_again_at_new_count() {
        // The stamp is computed per (bead, n): asked at 3 does not suppress 4.
        assert_eq!(run(4, 0, 0, "", "0:0:0:0", false), "poison ask");
    }

    #[test]
    fn lift_at_or_above_n_suppresses_poison() {
        assert_eq!(run(3, 0, 0, "", "0:0:1:1", false), "none");
    }

    #[test]
    fn new_failure_after_lift_poisons() {
        // Lifted at 3; n is now 4, so the caller's stamp has lifted=0.
        assert_eq!(run(4, 0, 0, "", "0:0:0:0", false), "poison ask");
    }

    #[test]
    fn decide_requeue_mail_at_cap() {
        assert_eq!(run(0, 5, 0, "", "0:0:0", false), "requeue-mail");
        assert_eq!(run(0, 4, 0, "", "0:0:0", false), "none");
        assert_eq!(run(0, 5, 0, "", "1:0:0", false), "none");
        assert_eq!(run(0, 9, 0, "repo:spira,delivers:action", "0:0:0", false), "none");
    }

    #[test]
    fn reclaim_mail_at_cap() {
        assert_eq!(run(0, 0, 5, "", "0:0:0", false), "reclaim-mail");
        assert_eq!(run(0, 0, 5, "", "0:1:0", false), "none");
    }

    #[test]
    fn supplement_call_shape_only_ever_requeue_mail() {
        // sentinel's closed-bead supplement: check4_decide 0 <rq> 0 <labels> "<rq>:1:1"
        assert_eq!(run(0, 6, 0, "plan", "0:1:1", false), "requeue-mail");
    }

    #[test]
    fn all_tokens_in_fixed_order() {
        assert_eq!(run(3, 5, 5, "", "0:0:0:0", false), "requeue-mail reclaim-mail poison ask");
    }

    #[test]
    fn stamp_parse_is_bash_lenient() {
        assert_eq!(AskedStamp::parse(""), AskedStamp::default());
        assert_eq!(AskedStamp::parse("1:1:1"), AskedStamp { requeue: true, reclaim: true, poison_at_n: true, lifted_at_or_above_n: false });
        assert_eq!(AskedStamp::parse("yes:1"), AskedStamp { reclaim: true, ..AskedStamp::default() });
    }
}
