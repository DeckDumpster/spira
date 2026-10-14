//! `concierge-guard check|health --state FILE ...` — see systemd/spira-concierge-guard.service.

use concierge_guard::{check, health, Proc, Settings, State, World};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const USAGE: &str = "usage: concierge-guard check --state FILE --session NAME --waiter \"WORDS\"... --inbox-text TEXT --mail-after SECS [--inbox FILE] --mail BIN --from ADDR [--proc-root DIR]\n       concierge-guard health --state FILE --max-age SECS";

struct Real {
    proc_root: PathBuf,
    mail: String,
    from: String,
    inbox: PathBuf,
}

fn read_argv(dir: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read(dir.join("cmdline")).ok()?;
    let argv: Vec<String> = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    (!argv.is_empty()).then_some(argv)
}

fn read_ppid(dir: &Path) -> Option<u32> {
    let stat = std::fs::read_to_string(dir.join("stat")).ok()?;
    stat[stat.rfind(')')? + 1..].split_whitespace().nth(1)?.parse().ok()
}

impl World for Real {
    fn procs(&self) -> Vec<Proc> {
        let Ok(rd) = std::fs::read_dir(&self.proc_root) else { return vec![] };
        rd.flatten()
            .filter_map(|e| {
                let pid: u32 = e.file_name().to_str()?.parse().ok()?;
                Some(Proc { pid, ppid: read_ppid(&e.path())?, argv: read_argv(&e.path())? })
            })
            .collect()
    }

    fn append_inbox(&mut self, text: &str) -> Result<(), String> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.inbox).map_err(|e| format!("{}: {e}", self.inbox.display()))?;
        writeln!(f, "{} {text}", iso(now())).map_err(|e| e.to_string())
    }

    fn send_mail(&mut self, subject: &str, body: &str) -> Result<(), String> {
        let mut child = Command::new("timeout")
            .args(["5", &self.mail, "send", "operator", "--from", &self.from, "--subject", subject, "--kind", "note", "--urgent"])
            .env("SPIRA_MAIL_OPERATOR_CONSIDERED", "concierge-guard: the Concierge is the party that is not answering")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", self.mail))?;
        child.stdin.take().ok_or("no stdin")?.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        let st = child.wait().map_err(|e| e.to_string())?;
        st.success().then_some(()).ok_or_else(|| format!("mail send exited {st}"))
    }

    fn inbox_tail(&self) -> String {
        let text = std::fs::read_to_string(&self.inbox).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(10)..].join("\n")
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn run() -> Result<i32, String> {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().ok_or(USAGE)?;
    let mut opts: Vec<(String, String)> = Vec::new();
    while let Some(k) = args.next() {
        opts.push((k, args.next().ok_or(USAGE)?));
    }
    let get = |k: &str| opts.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).ok_or_else(|| format!("missing {k}\n{USAGE}"));
    let num = |k: &str| get(k)?.parse::<u64>().map_err(|e| format!("{k}: {e}"));
    let state_path = get("--state")?;
    let prior = State::parse(&std::fs::read_to_string(&state_path).unwrap_or_default());

    if cmd == "health" {
        return match health(&prior, now(), num("--max-age")?) {
            Ok(m) => {
                println!("{m}");
                Ok(0)
            }
            Err(m) => {
                println!("{m}");
                Ok(1)
            }
        };
    }
    if cmd != "check" {
        return Err(USAGE.to_string());
    }

    let cfg = Settings {
        session_name: get("--session")?,
        waiters: opts.iter().filter(|(k, _)| k == "--waiter").map(|(_, v)| v.split_whitespace().map(str::to_string).collect()).collect(),
        mail_after: num("--mail-after")?,
        inbox_text: get("--inbox-text")?,
    };
    if cfg.waiters.is_empty() {
        return Err(format!("missing --waiter\n{USAGE}"));
    }
    let mut world = Real {
        proc_root: PathBuf::from(get("--proc-root").unwrap_or_else(|_| "/proc".to_string())),
        mail: get("--mail")?,
        from: get("--from")?,
        inbox: PathBuf::from(get("--inbox").or_else(|_| spira_config::process::cfg("SPIRA_CONCIERGE_INBOX"))?),
    };
    let t = now();
    let (state, events) = check(&cfg, &mut world, &prior, t);
    for e in &events {
        println!("{} concierge-guard: {e}", iso(t));
    }
    std::fs::write(&state_path, state.render()).map_err(|e| format!("{state_path}: {e}"))?;
    Ok(0)
}

fn iso(t: u64) -> String {
    let (days, rem) = ((t / 86_400) as i64, t % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("concierge-guard: {e}");
            std::process::exit(2);
        }
    }
}
