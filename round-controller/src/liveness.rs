//! Progress-gated renewal. A runner renews its lease only while the child's last progress mark is
//! inside the current phase's deadline and the pass is inside its maximum wall, so a hung child,
//! a hung runner and an over-long pass all lose the lease within deadline + one TTL.

/// The phases of a pass, each with its own progress deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    Boot,
    Build,
    Suites,
    SubPass,
    Certify,
}

impl Phase {
    pub const ALL: [Phase; 5] = [
        Phase::Boot,
        Phase::Build,
        Phase::Suites,
        Phase::SubPass,
        Phase::Certify,
    ];

    pub fn word(self) -> &'static str {
        match self {
            Phase::Boot => "boot",
            Phase::Build => "build",
            Phase::Suites => "suites",
            Phase::SubPass => "subpass",
            Phase::Certify => "certify",
        }
    }

    pub fn from_word(w: &str) -> Option<Phase> {
        Phase::ALL.into_iter().find(|p| p.word() == w)
    }

    /// `SPIRA_ROUND_PHASE_DEADLINE_SECS_<PHASE>`.
    pub fn deadline_key(self) -> String {
        format!(
            "SPIRA_ROUND_PHASE_DEADLINE_SECS_{}",
            self.word().to_ascii_uppercase()
        )
    }

    fn index(self) -> usize {
        self as usize
    }
}

pub const TTL_KEY: &str = "SPIRA_ROUND_LEASE_TTL_SECS";
pub const PASS_WALL_KEY: &str = "SPIRA_ROUND_PASS_WALL_MAX_SECS";
pub const RETRIES_KEY: &str = "SPIRA_ROUND_REDRIVE_RETRIES";
pub const EC2_LIFETIME_KEY: &str = "SPIRA_ROUND_VM_EC2_MAX_LIFETIME_SECS";

/// The runner's latest progress mark, kept where `pass` can read it: the lease it was written
/// under, the phase it is in, and when the child last showed progress.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerMark {
    pub token: state_store::Rev,
    pub phase: String,
    pub at_ms: u64,
}

pub fn mark_key(repo: &str, batch: &str) -> String {
    format!("batches/{repo}/{batch}/mark")
}

/// The lease keys' values, in milliseconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseSettings {
    pub ttl_ms: u64,
    pub deadlines_ms: [u64; 5],
    pub pass_wall_max_ms: u64,
    pub redrive_retries: u32,
    pub ec2_max_lifetime_ms: u64,
}

impl LeaseSettings {
    /// Every key through `get`, which answers a key's declared value or refuses naming it.
    pub fn from_cfg(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Self, String> {
        let num = |k: &str| -> Result<u64, String> {
            let v = get(k)?;
            v.trim()
                .parse::<u64>()
                .map_err(|e| format!("{k} = {v:?} does not parse: {e}"))
        };
        let secs = |k: &str| num(k).map(|s| s.saturating_mul(1000));
        let mut deadlines_ms = [0; 5];
        for p in Phase::ALL {
            deadlines_ms[p.index()] = secs(&p.deadline_key())?;
        }
        let s = LeaseSettings {
            ttl_ms: secs(TTL_KEY)?,
            deadlines_ms,
            pass_wall_max_ms: secs(PASS_WALL_KEY)?,
            redrive_retries: u32::try_from(num(RETRIES_KEY)?)
                .map_err(|e| format!("{RETRIES_KEY}: {e}"))?,
            ec2_max_lifetime_ms: secs(EC2_LIFETIME_KEY)?,
        };
        if s.ttl_ms < 3 {
            return Err(format!(
                "{TTL_KEY} must leave a renewal interval of TTL/3 above zero"
            ));
        }
        if s.ec2_max_lifetime_ms <= s.pass_wall_max_ms {
            return Err(format!("{EC2_LIFETIME_KEY} must exceed {PASS_WALL_KEY}"));
        }
        Ok(s)
    }

    /// A renewal every third of the TTL, so two renewals can be late before the lease lapses.
    pub fn renew_every_ms(&self) -> u64 {
        self.ttl_ms / 3
    }

    pub fn deadline_ms(&self, p: Phase) -> u64 {
        self.deadlines_ms[p.index()]
    }

    pub fn gate(&self) -> RenewGate {
        RenewGate {
            deadlines_ms: self.deadlines_ms,
            pass_wall_max_ms: self.pass_wall_max_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenewGate {
    deadlines_ms: [u64; 5],
    pass_wall_max_ms: u64,
}

impl RenewGate {
    /// All times are epoch milliseconds on the runner's clock. A mark stamped after `now` is
    /// fresh, not an error.
    pub fn may_renew(&self, now: u64, last_mark: u64, phase: Phase, pass_started: u64) -> bool {
        now.saturating_sub(last_mark) < self.deadlines_ms[phase.index()]
            && now.saturating_sub(pass_started) < self.pass_wall_max_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(k: &str) -> Result<String, String> {
        Ok(match k {
            TTL_KEY => "90",
            "SPIRA_ROUND_PHASE_DEADLINE_SECS_BOOT" => "11",
            "SPIRA_ROUND_PHASE_DEADLINE_SECS_BUILD" => "22",
            "SPIRA_ROUND_PHASE_DEADLINE_SECS_SUITES" => "33",
            "SPIRA_ROUND_PHASE_DEADLINE_SECS_SUBPASS" => "44",
            "SPIRA_ROUND_PHASE_DEADLINE_SECS_CERTIFY" => "55",
            PASS_WALL_KEY => "1000",
            RETRIES_KEY => "4",
            EC2_LIFETIME_KEY => "2000",
            _ => return Err(format!("{k} is not registered")),
        }
        .to_string())
    }

    #[test]
    fn every_key_is_read_in_milliseconds_and_the_interval_is_a_third_of_the_ttl() {
        let s = LeaseSettings::from_cfg(&cfg).unwrap();
        assert_eq!(
            s,
            LeaseSettings {
                ttl_ms: 90_000,
                deadlines_ms: [11_000, 22_000, 33_000, 44_000, 55_000],
                pass_wall_max_ms: 1_000_000,
                redrive_retries: 4,
                ec2_max_lifetime_ms: 2_000_000,
            }
        );
        assert_eq!(s.renew_every_ms(), 30_000);
        assert_eq!(s.deadline_ms(Phase::SubPass), 44_000);
    }

    #[test]
    fn a_missing_or_unparsable_key_is_named() {
        let missing = |k: &str| {
            if k == RETRIES_KEY {
                Err(format!("{k} is not registered"))
            } else {
                cfg(k)
            }
        };
        assert_eq!(
            LeaseSettings::from_cfg(&missing).unwrap_err(),
            "SPIRA_ROUND_REDRIVE_RETRIES is not registered"
        );
        let bad = |k: &str| {
            if k == TTL_KEY {
                Ok("5m".into())
            } else {
                cfg(k)
            }
        };
        assert!(LeaseSettings::from_cfg(&bad)
            .unwrap_err()
            .starts_with("SPIRA_ROUND_LEASE_TTL_SECS = \"5m\" does not parse"));
        let huge = |k: &str| {
            if k == RETRIES_KEY {
                Ok("99999999999".into())
            } else {
                cfg(k)
            }
        };
        assert!(LeaseSettings::from_cfg(&huge)
            .unwrap_err()
            .starts_with(RETRIES_KEY));
        let zero = |k: &str| if k == TTL_KEY { Ok("0".into()) } else { cfg(k) };
        assert!(LeaseSettings::from_cfg(&zero)
            .unwrap_err()
            .contains("renewal interval"));
        let short = |k: &str| {
            if k == EC2_LIFETIME_KEY {
                Ok("1000".into())
            } else {
                cfg(k)
            }
        };
        assert_eq!(
            LeaseSettings::from_cfg(&short).unwrap_err(),
            "SPIRA_ROUND_VM_EC2_MAX_LIFETIME_SECS must exceed SPIRA_ROUND_PASS_WALL_MAX_SECS"
        );
    }

    #[test]
    fn phase_words_round_trip_and_name_their_deadline_keys() {
        for p in Phase::ALL {
            assert_eq!(Phase::from_word(p.word()), Some(p));
        }
        assert_eq!(Phase::from_word("lint"), None);
        assert_eq!(
            Phase::SubPass.deadline_key(),
            "SPIRA_ROUND_PHASE_DEADLINE_SECS_SUBPASS"
        );
    }

    #[test]
    fn may_renew_holds_only_inside_each_phases_deadline_and_the_pass_wall() {
        let g = LeaseSettings::from_cfg(&cfg).unwrap().gate();
        let start = 1_000_000;
        for (p, d) in Phase::ALL
            .into_iter()
            .zip([11_000, 22_000, 33_000, 44_000, 55_000])
        {
            let mark = start + 5_000;
            assert!(g.may_renew(mark + d - 1, mark, p, start), "{p:?} inside");
            assert!(!g.may_renew(mark + d, mark, p, start), "{p:?} at deadline");
        }
        assert!(
            g.may_renew(start + 10, start + 20, Phase::Boot, start),
            "a mark ahead of the runner's clock is fresh"
        );
        let late = start + 1_000_000;
        assert!(g.may_renew(late - 1, late - 2, Phase::Suites, start));
        assert!(
            !g.may_renew(late, late - 1, Phase::Suites, start),
            "fresh marks past the wall do not renew"
        );
    }
}
