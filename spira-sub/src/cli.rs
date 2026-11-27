//! The `spira-sub` command line.

pub const HELP: &str = "\
spira-sub — one subscriber's connection to the harness's MQTT broker.

usage: spira-sub run --as <name>
       spira-sub add <filter> [--note <text>] [--once | --batch <interval>] [--as <name>]
       spira-sub add --at <instant> --note <text> [--as <name>]
       spira-sub set <id> (--now | --batch <interval>) [--as <name>]
       spira-sub drop <id> [--as <name>]
       spira-sub list [--as <name>]
       spira-sub follow [--as <name>]

verbs
  run      holds the broker connection and serves the control socket, until killed. The unit
           spira-sub-<name>-<instance>.service runs it; subscriber/<name> reads gone once it ends.
  add      subscribes, and prints the new subscription's id. The current value of every
           matching state topic is delivered at once, or in the first batch.
  set      changes whether one subscription is delivered at once or batched; its id stays.
  drop     removes one subscription.
  list     each subscription, when it last fired, and whether each publisher is connected.
  follow   streams delivery lines until killed, values not yet delivered first. It waits out
           a `run` that is down or restarting.

arguments
  <filter>, --filter <filter>
                     an MQTT topic filter (`+` one level, a final `#` the rest) that matches a
                     family declared in spira/topics.toml
  <id>, --id <id>    the id `add` printed, such as s12
  --as <name>        the subscriber's name: one topic segment (default: concierge; run
                     requires it)
  --note <text>      said after each line this subscription delivers
  --once             drop the subscription after its first delivery
  --batch <interval> deliver in batches, one per interval (10m, 90s, 2h); not with --once
                     or --at
  --now              (set) deliver at once, starting with what the pending batch holds
  --at <instant>     fire once at this RFC 3339 instant, such as 2026-11-26T09:45:00Z;
                     requires --note
  -h, --help         this text

A delivery is one line: [sub:<id>] <topic> <fields> — <note>
A batched subscription's changes arrive as one delivery at the next multiple of its interval:
a header, [batch] n=<k> since <instant>, then <k> such lines, each changed topic's latest value
once. Subscriptions due together share one batch. A publisher going is still said at once.
With no `follow` attached nothing counts as delivered: a --once subscription and an --at timer
wait for one.

The broker is the one SPIRA_BROKER_SOCKET names; the control socket and the subscription set
are in the directory SPIRA_SUB_CONTROL_DIR names.

exit codes:
  0  done
  1  failed: `run` is not up, or could not keep running
  2  refused, nothing changed: usage, a filter matching no family, an unknown id or
     configuration
";

pub const DEFAULT_NAME: &str = "concierge";

#[derive(Debug, PartialEq)]
pub enum Command {
    Run,
    Add { filter: String, note: String, once: bool, batch_s: Option<u64> },
    At { at_ms: u64, note: String },
    Drop { id: String },
    Set { id: String, batch_s: Option<u64> },
    List,
    Follow,
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Help,
    Command { name: String, command: Command },
}

const VERBS: [&str; 6] = ["run", "add", "set", "drop", "list", "follow"];

fn instant(text: &str) -> Result<u64, String> {
    let zoned = text.ends_with(['Z', 'z']) || text.len() > 6 && text.get(text.len() - 6..).is_some_and(|t| t.starts_with(['+', '-']));
    zoned
        .then(|| strand::timefmt::parse_rfc3339(text))
        .flatten()
        .and_then(|secs| u64::try_from(secs).ok())
        .map(|secs| secs * 1000)
        .ok_or_else(|| format!("--at {text:?} is not an RFC 3339 instant with a zone, such as 2026-11-26T09:45:00Z"))
}

fn interval(text: &str) -> Result<u64, String> {
    spira_config::amendment::parse_duration(text)
        .and_then(|secs| u64::try_from(secs).ok())
        .ok_or_else(|| format!("--batch {text:?} is not an interval such as 10m, 90s or 2h"))
}

fn segment(name: &str) -> Result<String, String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '+', '#', '\0']) {
        return Err(format!("--as {name:?} is not one topic segment"));
    }
    Ok(name.to_string())
}

pub fn parse(argv: &[String]) -> Result<Parsed, String> {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(Parsed::Help);
    }
    let (verb, rest) = argv.split_first().ok_or_else(|| format!("a verb is required: {}", VERBS.join(", ")))?;
    if !VERBS.contains(&verb.as_str()) {
        return Err(format!("unknown verb {verb:?}: it is one of {}", VERBS.join(", ")));
    }
    // The one operand a verb takes, and the flag that also names it.
    let operand_flag = match verb.as_str() {
        "add" => Some("--filter"),
        "set" | "drop" => Some("--id"),
        _ => None,
    };
    let (mut name, mut note, mut at, mut operand, mut once) = (None, None, None, None, false);
    let (mut batch, mut now) = (None, false);
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        let slot = match word.as_str() {
            "--once" if verb == "add" => {
                once = true;
                continue;
            }
            "--now" if verb == "set" => {
                now = true;
                continue;
            }
            "--as" => &mut name,
            "--batch" if verb == "add" || verb == "set" => &mut batch,
            "--note" if verb == "add" => &mut note,
            "--at" if verb == "add" => &mut at,
            flag if Some(flag) == operand_flag => &mut operand,
            other if other.starts_with('-') => return Err(format!("{verb} takes no {other:?}; see -h")),
            bare => match operand_flag {
                Some(_) if operand.is_none() => {
                    operand = Some(bare.to_string());
                    continue;
                }
                _ => {
                    let meant = if verb == "add" { "should it follow --note?" } else { "should it follow --as?" };
                    return Err(format!("unexpected bare argument {bare:?}: {meant}"));
                }
            },
        };
        let value = words.next().ok_or_else(|| format!("{word} needs a value"))?;
        if slot.replace(value.clone()).is_some() {
            return Err(format!("{word} was given twice"));
        }
    }
    let batch_s = batch.as_deref().map(interval).transpose()?;
    let command = match verb.as_str() {
        "run" if name.is_none() => return Err("run requires --as <name>".to_string()),
        "run" => Command::Run,
        "add" => match (operand, at) {
            (Some(_), Some(_)) => return Err("give a filter or --at, not both".to_string()),
            (Some(_), None) if once && batch_s.is_some() => return Err("--batch conflicts with --once: give one".to_string()),
            (Some(filter), None) => Command::Add { filter, note: note.unwrap_or_default(), once, batch_s },
            (None, Some(_)) if batch_s.is_some() => return Err("--batch conflicts with --at: a timer fires at once".to_string()),
            (None, Some(_)) if once => return Err("--at already fires once; drop --once".to_string()),
            (None, Some(at)) => Command::At { at_ms: instant(&at)?, note: note.ok_or("--at requires --note <text>")? },
            (None, None) => return Err("add needs a <filter> or --at <instant>".to_string()),
        },
        "set" => {
            let id = operand.ok_or("set needs an <id>")?;
            if now == batch_s.is_some() {
                return Err("set needs exactly one of --now and --batch <interval>".to_string());
            }
            Command::Set { id, batch_s }
        }
        "drop" => Command::Drop { id: operand.ok_or("drop needs an <id>")? },
        "list" => Command::List,
        _ => Command::Follow,
    };
    let name = segment(name.as_deref().unwrap_or(DEFAULT_NAME))?;
    Ok(Parsed::Command { name, command })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(words: &[&str]) -> Result<Parsed, String> {
        parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test_id(tj5ow00)]
    #[test]
    fn the_commands_of_the_user_path_parse_with_the_concierge_as_the_default_name() {
        let got = of(&["add", "session/+/age/+", "--note", "read this builder's approach"]).unwrap();
        let command = Command::Add { filter: "session/+/age/+".into(), note: "read this builder's approach".into(), once: false, batch_s: None };
        assert_eq!(got, Parsed::Command { name: "concierge".into(), command });
        assert_eq!(of(&["drop", "s12", "--as", "pane"]).unwrap(), Parsed::Command { name: "pane".into(), command: Command::Drop { id: "s12".into() } });
        assert_eq!(of(&["drop", "--id", "s1"]), of(&["drop", "s1"]));
        assert_eq!(of(&["add", "--filter", "round/#"]), of(&["add", "round/#"]));
        let at = of(&["add", "--at", "1970-01-01T01:00:00+00:00", "--note", "n"]).unwrap();
        assert_eq!(at, Parsed::Command { name: "concierge".into(), command: Command::At { at_ms: 3_600_000, note: "n".into() } });
        assert_eq!(of(&["list", "-h"]).unwrap(), Parsed::Help);
    }

    #[test_id(tcxh23f)]
    #[test]
    fn set_names_one_subscription_and_the_class_it_takes() {
        let set = |id: &str, batch_s| Ok(Parsed::Command { name: "concierge".into(), command: Command::Set { id: id.into(), batch_s } });
        assert_eq!(of(&["set", "s1", "--now"]), set("s1", None));
        assert_eq!(of(&["set", "--id", "s2", "--batch", "10m"]), set("s2", Some(600)));
        assert_eq!(of(&["set", "s3", "--batch", "90s"]), set("s3", Some(90)));
        let added = Command::Add { filter: "round/#".into(), note: String::new(), once: false, batch_s: Some(7200) };
        assert_eq!(of(&["add", "round/#", "--batch", "2h"]), Ok(Parsed::Command { name: "concierge".into(), command: added }));
        for want in ["spira-sub set <id>", "--batch <interval>", "--now"] {
            assert!(HELP.contains(want), "{want}");
        }
    }

    #[test_id(t66dl6n)]
    #[test]
    fn a_set_or_batch_line_that_names_no_one_class_is_refused() {
        for (words, want) in [
            (&["set", "s1"][..], "exactly one"),
            (&["set", "s1", "--now", "--batch", "10m"][..], "exactly one"),
            (&["set", "--now"][..], "<id>"),
            (&["set", "s1", "--batch", "soon"][..], "interval"),
            (&["set", "s1", "--batch", "0m"][..], "interval"),
            (&["set", "s1", "--batch"][..], "needs a value"),
            (&["set", "s1", "--once"][..], "--once"),
            (&["add", "round/#", "--now"][..], "--now"),
            (&["add", "round/#", "--batch", "10m", "--once"][..], "--once"),
            (&["add", "--at", "2026-11-26T09:45:00Z", "--note", "n", "--batch", "10m"][..], "--at"),
        ] {
            let why = of(words).unwrap_err();
            assert!(why.contains(want), "{words:?}: {why}");
        }
    }

    #[test_id(tze2yvs)]
    #[test]
    fn a_wrong_line_is_refused_naming_what_was_wrong() {
        for (words, want) in [
            (&["add", "round/#", "look"][..], "--note"),
            (&["list", "pane"][..], "--as"),
            (&["run"][..], "--as"),
            (&["add"][..], "--at"),
            (&["add", "--at", "2026-11-26T09:45:00", "--note", "n"][..], "RFC 3339"),
            (&["add", "--at", "2026-11-26T09:45:00Z"][..], "--note"),
            (&["add", "x", "--at", "2026-11-26T09:45:00Z"][..], "not both"),
            (&["follow", "--once"][..], "--once"),
            (&["list", "--as", "a/b"][..], "segment"),
            (&["watch"][..], "unknown verb"),
        ] {
            let why = of(words).unwrap_err();
            assert!(why.contains(want), "{words:?}: {why}");
        }
        assert!(HELP.contains("exit codes") && VERBS.iter().all(|v| HELP.contains(v)));
    }
}
