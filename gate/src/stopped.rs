//! testenv's record of a signal that stopped a run from outside, and the two forms it takes.

/// A signal that stopped a run from outside, as the supervisor read its sender the moment it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub sig: i32,
    pub by_pid: Option<u32>,
    pub cmd: String,
    pub cgroup: String,
}

const STOPPED_PREFIX: &str = "testenv: stopped from outside: sig=";
const VERDICT_PREFIX: &str = "stopped=outside sig=";
const PID_UNRESOLVED: &str = "? (outside the sandbox's namespace)";

impl Stopped {
    pub fn line(&self) -> String {
        let pid = self.by_pid.map_or_else(|| PID_UNRESOLVED.to_string(), |p| p.to_string());
        format!("{STOPPED_PREFIX}{} from pid={pid} cmd={} cgroup={}", self.sig, self.cmd, self.cgroup)
    }

    pub fn parse(line: &str) -> Option<Stopped> {
        let rest = line.strip_prefix(STOPPED_PREFIX)?;
        let (sig, rest) = rest.split_once(" from pid=")?;
        let (pid, rest) = rest.split_once(" cmd=")?;
        let (cmd, cgroup) = rest.rsplit_once(" cgroup=")?;
        Some(Stopped { sig: sig.parse().ok()?, by_pid: pid.parse().ok(), cmd: cmd.into(), cgroup: cgroup.into() })
    }

    /// The sender's program name: the basename of its command's first word.
    pub fn comm(&self) -> &str {
        let first = self.cmd.split_whitespace().next().unwrap_or("?");
        let first = first.strip_prefix('[').and_then(|f| f.strip_suffix(']')).unwrap_or(first);
        first.rsplit('/').next().unwrap_or(first)
    }

    /// The systemd unit or scope the sender ran in: the cgroup path's last component.
    pub fn unit(&self) -> &str {
        self.cgroup.trim_end_matches('/').rsplit('/').next().unwrap_or("?")
    }

    fn by(&self) -> String {
        format!("{}:{}", self.by_pid.map_or_else(|| "?".to_string(), |p| p.to_string()), self.comm())
    }

    /// The `sandbox-metrics` line's field.
    pub fn metrics_field(&self) -> String {
        format!("{VERDICT_PREFIX}{} by={}", self.sig, self.by())
    }

    /// The `VERDICT RED` line's field.
    pub fn verdict_field(&self) -> String {
        format!("{} cgroup={}", self.metrics_field(), self.unit())
    }

    /// A [`Stopped::verdict_field`] read back: the sender is its program name and its unit.
    pub fn parse_verdict(field: &str) -> Option<Stopped> {
        let mut words = field.strip_prefix(VERDICT_PREFIX)?.split_whitespace();
        let sig = words.next()?.parse().ok()?;
        let (pid, cmd) = words.next()?.strip_prefix("by=")?.split_once(':')?;
        let cgroup = words.next()?.strip_prefix("cgroup=")?;
        if words.next().is_some() {
            return None;
        }
        Some(Stopped { sig, by_pid: pid.parse().ok(), cmd: cmd.into(), cgroup: cgroup.into() })
    }

    /// The stop testenv's verdict names: the field ending the last line `output` opens with
    /// `VERDICT `. An earlier line, or one printed indented under a test, names nobody.
    pub fn in_verdict(output: &str) -> Option<Stopped> {
        let verdict = output.lines().rev().find(|l| l.starts_with("VERDICT "))?;
        Stopped::parse_verdict(&verdict[verdict.rfind(VERDICT_PREFIX)?..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sender() -> Stopped {
        Stopped { sig: 15, by_pid: Some(4242), cmd: "kill".into(), cgroup: "x.service".into() }
    }

    #[test_id(te2bk4r)]
    #[test]
    fn a_verdict_field_reads_back_with_an_unresolved_sender_kept() {
        for s in [sender(), Stopped { by_pid: None, ..sender() }] {
            assert_eq!(Stopped::parse_verdict(&s.verdict_field()), Some(s));
        }
    }

    #[test_id(t7kh09c)]
    #[test]
    fn only_a_line_testenv_opens_names_a_sender() {
        let real = format!("VERDICT RED ran=1 red=1 first=unit-tests.sh: exit 143 {}", sender().verdict_field());
        let forged = format!("    VERDICT RED {}", Stopped { sig: 9, ..sender() }.verdict_field());
        assert_eq!(Stopped::in_verdict(&format!("{real}\n{forged}\ngate: x\n")), Some(sender()));
        assert_eq!(Stopped::in_verdict(&forged), None);
        assert_eq!(Stopped::in_verdict(&sender().verdict_field()), None);
        let earlier = format!("{real}\nVERDICT RED ran=1 red=1 first=unit-tests.sh: exit 101");
        assert_eq!(Stopped::in_verdict(&earlier), None, "testenv's last verdict names nobody");
        let quoted = format!("VERDICT RED ran=1 red=1 first={} and more", sender().verdict_field());
        assert_eq!(Stopped::in_verdict(&quoted), None, "testenv ends its line with the field");
    }
}
