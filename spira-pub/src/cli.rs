//! The `spira-pub` command line. Arguments are named; the payload is on stdin.

use crate::Class;

pub const HELP: &str = "\
spira-pub — publish one fact to the harness's MQTT broker.

usage: spira-pub --as <name> --topic <topic> [--class <state|chatter>] < payload.json

  --as <name>       the publisher's name: one topic segment. publisher/<name> reads up while
                    this command runs and gone once it exits.
  --topic <topic>   a topic of a family declared in spira/topics.toml
  --class <class>   optional: the class you mean, state or chatter; refused unless the topic's
                    family declares it. The family's class decides whether it is retained.
  -h, --help        this text

The payload is one JSON value on stdin. The broker is the one SPIRA_BROKER_SOCKET names.

exit codes:
  0  the broker took the message
  1  not delivered: the broker is down, slow or dropped the connection
  2  refused, nothing sent: usage, payload, configuration, undeclared family or wrong class
";

#[derive(Debug, PartialEq)]
pub struct Args {
    pub name: String,
    pub topic: String,
    pub class: Option<Class>,
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Help,
    Publish(Args),
}

pub fn parse(argv: &[String]) -> Result<Parsed, String> {
    let (mut name, mut topic, mut class) = (None, None, None);
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        let slot = match word.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--as" => &mut name,
            "--topic" => &mut topic,
            "--class" => &mut class,
            "--json" => return Err("there is no --json: the payload goes on stdin".to_string()),
            other if other.starts_with('-') => return Err(format!("unknown argument {other:?}; see -h")),
            bare => {
                let meant = match (&name, &topic) {
                    (None, _) => "should it follow --as?",
                    (Some(_), None) => "should it follow --topic?",
                    _ => "the payload goes on stdin",
                };
                return Err(format!("unexpected bare argument {bare:?}: arguments are named; {meant}"));
            }
        };
        let value = words.next().ok_or_else(|| format!("{word} needs a value"))?;
        if slot.replace(value.clone()).is_some() {
            return Err(format!("{word} was given twice"));
        }
    }
    Ok(Parsed::Publish(Args {
        name: name.ok_or("--as <name> is required")?,
        topic: topic.ok_or("--topic <topic> is required")?,
        class: class.map(|c| c.parse()).transpose()?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(words: &[&str]) -> Result<Parsed, String> {
        parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test_id(t6ueejh)]
    #[test]
    fn a_bare_positional_is_refused_naming_the_flag_it_should_have_been() {
        let first = of(&["queue"]).unwrap_err();
        assert!(first.contains("\"queue\"") && first.contains("--as"), "{first}");
        let second = of(&["--as", "queue", "release/active"]).unwrap_err();
        assert!(second.contains("\"release/active\"") && second.contains("--topic"), "{second}");
        let third = of(&["--as", "q", "--topic", "release/active", "{}"]).unwrap_err();
        assert!(third.contains("stdin"), "{third}");
    }

    #[test_id(tc7sp5r)]
    #[test]
    fn the_required_arguments_are_named_when_missing_and_parsed_when_given() {
        assert!(of(&["--topic", "t"]).unwrap_err().contains("--as"));
        assert!(of(&["--as", "q"]).unwrap_err().contains("--topic"));
        assert!(of(&["--as"]).unwrap_err().contains("--as needs a value"));
        assert!(of(&["--as", "a", "--as", "b"]).unwrap_err().contains("twice"));
        let got = of(&["--as", "q", "--topic", "release/active", "--class", "state"]).unwrap();
        assert_eq!(got, Parsed::Publish(Args { name: "q".into(), topic: "release/active".into(), class: Some(Class::State) }));
        assert_eq!(of(&["-h"]).unwrap(), Parsed::Help);
        assert!(HELP.contains("--as") && HELP.contains("--topic") && HELP.contains("--class") && HELP.contains("exit codes"));
    }
}
