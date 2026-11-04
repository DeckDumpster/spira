//! The in-process doors a pass uses instead of a child process (scheduler consolidation §2):
//! the lifecycle store over its socket ([`LcClient`]), spira-claim and hel as libraries, the
//! unit port (`runtime::units`), the metrics store (`tsd::store`) and the mail library.
//!
//! [`Services`] is the seam; [`RealServices`] is production. Each method answers in the shape
//! the CLI it replaces printed — an [`Out`] with its exit code, stdout and stderr — so every
//! caller's parsing is unchanged; only the transport moved.

use std::cell::OnceCell;
use std::time::{Duration, Instant};

use runtime::units::{UnitHandle, UnitState, Units};
use spira_config::lc_call::{LcClient, LcError};

use crate::host::Out;

/// One `mail send concierge --kind question` ask.
pub struct Ask<'a> {
    pub from: &'a str,
    pub subject: &'a str,
    pub default: &'a str,
    pub body: &'a str,
    pub bead: &'a str,
    /// `SPIRA_MAIL_REPEAT_CONSIDERED=sentinel-own-dedup`: the pass deduped this ask itself.
    pub own_dedup: bool,
}

pub trait Services {
    /// One `spira-lc` verb. `bin` is the configured program (a fixture's stand-in routes
    /// through it, see [`LcClient::for_bin`]).
    fn lc(&self, bin: &str, args: &[String]) -> Out;
    /// One bead-content read through the store's `content` verb (`bd <args>` before).
    fn content(&self, bin: &str, args: &[String]) -> Out;
    /// One spira-claim verb in-process. `env` is what the CLI child would have been handed
    /// on top of the pass's environment (the ready cache path).
    fn claim(&self, args: &[String], env: &[(String, String)]) -> Out;
    /// hel `holder-alive <id>`.
    fn holder_alive(&self, id: &str) -> bool;
    /// hel `stop-holder <id>`: stop the unit working in the bead's worktree.
    fn stop_holder(&self, id: &str) -> bool;
    /// hel `destroy-worktree <id> <path> <repo> <why>`.
    fn destroy_worktree(&self, id: &str, path: &str, repo: &str, why: &str) -> bool;
    /// `systemctl --user is-active <unit>` read as "active".
    fn unit_active(&self, unit: &str) -> bool;
    /// `systemctl --user stop <unit>`: true once the unit has left active.
    fn unit_stop(&self, unit: &str) -> bool;
    /// One row of a metrics-store family (`tsd-write --family` before), best-effort.
    fn tsd_row(&self, family: &str, fields: &[(String, serde_json::Value)]);
    /// `mail send concierge --kind question …`: true only when the ask was accepted.
    fn mail(&self, ask: &Ask) -> bool;
}

fn out_of(r: Result<spira_config::lc_call::Reply, LcError>) -> Out {
    match r {
        Ok(r) => Out { rc: r.code, stdout: r.out, stderr: r.err },
        // No answer at all is "cannot tell" — the CLI's own exit 2.
        Err(e) => Out { rc: spira_config::lc_call::CANNOT_TELL, stdout: String::new(), stderr: e.to_string() },
    }
}

fn first_line(s: &str) -> String {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_string()
}

/// Production: every door in this process; the store client and the unit backend are opened
/// on first use and kept for the pass.
pub struct RealServices {
    /// The environment the pass's children are handed (conf.sh's, from the probe): what a
    /// spira-claim child would have read for the keys it takes from its environment.
    pub base_env: Vec<(String, String)>,
    lc: OnceCell<Result<LcClient, String>>,
    units: OnceCell<Result<Box<dyn Units>, String>>,
    tsd: OnceCell<tsd::store::Pusher>,
}

impl RealServices {
    pub fn new(base_env: Vec<(String, String)>) -> Self {
        RealServices { base_env, lc: OnceCell::new(), units: OnceCell::new(), tsd: OnceCell::new() }
    }

    fn client(&self, bin: &str) -> Result<&LcClient, Out> {
        self.lc
            .get_or_init(|| LcClient::for_bin(bin))
            .as_ref()
            .map_err(|e| Out { rc: spira_config::lc_call::CANNOT_TELL, stdout: String::new(), stderr: e.clone() })
    }

    fn units(&self) -> Result<&dyn Units, String> {
        self.units.get_or_init(hel::verbs::default_units).as_ref().map(|u| &**u).map_err(Clone::clone)
    }
}

/// How long a stop waits for the unit to leave active: systemd's own DefaultTimeoutStopSec,
/// the bound `systemctl stop` waited under.
const STOP_WAIT: Duration = Duration::from_secs(90);

impl Services for RealServices {
    fn lc(&self, bin: &str, args: &[String]) -> Out {
        match self.client(bin) {
            Ok(c) => out_of(c.call(args)),
            Err(o) => o,
        }
    }

    fn content(&self, bin: &str, args: &[String]) -> Out {
        let argv: Vec<String> = std::iter::once("content".to_string()).chain(args.iter().cloned()).collect();
        self.lc(bin, &argv)
    }

    fn claim(&self, args: &[String], env: &[(String, String)]) -> Out {
        let lookup = |k: &str| -> Option<String> {
            env.iter()
                .rev()
                .chain(self.base_env.iter().rev())
                .find(|(x, _)| x == k)
                .map(|(_, v)| v.clone())
                .or_else(|| std::env::var(k).ok())
        };
        let o = spira_claim::cli::dispatch_in(args, &mut std::io::empty(), &lookup);
        Out { rc: o.code, stdout: o.out, stderr: o.err }
    }

    fn holder_alive(&self, id: &str) -> bool {
        hel::verbs::holder_alive(id).unwrap_or(false)
    }

    fn stop_holder(&self, id: &str) -> bool {
        match self.units() {
            Ok(u) => hel::verbs::stop_holder(id, u).is_ok(),
            Err(_) => false,
        }
    }

    fn destroy_worktree(&self, id: &str, path: &str, repo: &str, why: &str) -> bool {
        hel::verbs::destroy_worktree(None, id, path, repo, why).unwrap_or(false)
    }

    fn unit_active(&self, unit: &str) -> bool {
        let Ok(u) = self.units() else { return false };
        u.facts(&UnitHandle::named(unit)).is_ok_and(|f| f.state == UnitState::Active)
    }

    fn unit_stop(&self, unit: &str) -> bool {
        let Ok(u) = self.units() else { return false };
        let h = UnitHandle::named(unit);
        if u.stop(&h).is_err() {
            return false;
        }
        let start = Instant::now();
        loop {
            match u.facts(&h).map(|f| f.state) {
                Ok(UnitState::Active | UnitState::Activating | UnitState::Deactivating) => {}
                Ok(_) => return true,
                Err(_) => return false,
            }
            if start.elapsed() >= STOP_WAIT {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200)); // waits-on: the stopped unit leaving active; cap STOP_WAIT
        }
    }

    fn tsd_row(&self, family: &str, fields: &[(String, serde_json::Value)]) {
        let pusher = self.tsd.get_or_init(|| {
            tsd::store::Pusher::new(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().filter(|e| !e.trim().is_empty()))
        });
        let _ = pusher.push(family, &tsd::now_iso(), fields);
    }

    fn mail(&self, ask: &Ask) -> bool {
        let Ok(mut env) = mail::env::Env::load() else { return false };
        if ask.own_dedup {
            env.repeat_considered = Some("sentinel-own-dedup".into());
        }
        if mail::maildir::mailbox_valid("concierge").is_err() {
            return false;
        }
        let Ok(bdq) = spira_config::release_env::tool("bdq") else { return false };
        let bd = mail::bead::BdCli { bdq, conn_retries: env.bd_conn_retries };
        let args = mail::cmds::SendArgs {
            mailbox: "concierge",
            from: Some(ask.from),
            subject: ask.subject,
            kind: "question",
            default: ask.default,
            class: "",
            bead: ask.bead,
            urgent: false,
            digest: false,
            dry_run: false,
            requested_by: "",
        };
        let body = ask.body.to_string();
        let result = mail::cmds::send(&bd, &env, &args, body.clone());
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        if let Some(a) = mail::cmds::attempt_of_send(&args, &body, &result, now) {
            if let Err(e) = a.write(&env.run_dir, &tsd::default_host()) {
                eprintln!("sentinel: mail send attempt not recorded: {}", first_line(&e));
            }
        }
        result.is_ok()
    }
}
