//! The watchdog's timer, from one description: a systemd service and timer on Linux, a launchd
//! plist on macOS, a crontab line anywhere else.

use std::path::PathBuf;

pub const LABEL: &str = "spira-watchdog";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimerSpec {
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub every_s: u64,
}

impl TimerSpec {
    /// The watchdog's own timer: one pass every `every_s`.
    pub fn watchdog(program: PathBuf, every_s: u64, env: Vec<(String, String)>) -> TimerSpec {
        TimerSpec { label: LABEL.into(), program, args: vec!["pass".into()], env, every_s }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Systemd,
    Launchd,
    Cron,
}

impl Platform {
    /// From `std::env::consts::OS`.
    pub fn for_os(os: &str) -> Platform {
        match os {
            "linux" => Platform::Systemd,
            "macos" => Platform::Launchd,
            _ => Platform::Cron,
        }
    }
}

/// `(file name, contents)` for each file the platform needs.
pub fn render(p: Platform, t: &TimerSpec) -> Result<Vec<(String, String)>, String> {
    if t.every_s == 0 {
        return Err("a timer every 0 s never settles — refusing".into());
    }
    let prog = t.program.to_str().ok_or_else(|| format!("{} is not UTF-8", t.program.display()))?;
    Ok(match p {
        Platform::Systemd => systemd(t, prog),
        Platform::Launchd => vec![(format!("{}.plist", t.label), launchd(t, prog))],
        Platform::Cron => vec![(format!("{}.crontab", t.label), cron(t, prog)?)],
    })
}

fn sd_quote(s: &str) -> String {
    let esc = s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$");
    if esc.is_empty() || esc.contains(|c: char| c.is_whitespace() || c == '\'' || c == ';') || esc != s {
        format!("\"{esc}\"")
    } else {
        esc
    }
}

fn systemd(t: &TimerSpec, prog: &str) -> Vec<(String, String)> {
    let exec: Vec<String> = std::iter::once(prog).chain(t.args.iter().map(String::as_str)).map(sd_quote).collect();
    let env: String = t.env.iter().map(|(k, v)| format!("Environment={}\n", sd_quote(&format!("{k}={v}")))).collect();
    let service = format!(
        "[Unit]\nDescription={label} — one pass: restart and page when the scheduler is wedged or absent\n\n[Service]\nType=oneshot\n{env}ExecStart={exec}\nTimeoutStartSec={every}\n",
        label = t.label,
        exec = exec.join(" "),
        every = t.every_s,
    );
    let timer = format!(
        "[Unit]\nDescription={label} timer\n\n[Timer]\nOnBootSec={every}s\nOnUnitActiveSec={every}s\nAccuracySec=1s\nUnit={label}.service\n\n[Install]\nWantedBy=timers.target\n",
        label = t.label,
        every = t.every_s,
    );
    vec![(format!("{}.service", t.label), service), (format!("{}.timer", t.label), timer)]
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn launchd(t: &TimerSpec, prog: &str) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n");
    out += &format!("  <key>Label</key>\n  <string>{}</string>\n  <key>ProgramArguments</key>\n  <array>\n", xml(&t.label));
    for a in std::iter::once(prog).chain(t.args.iter().map(String::as_str)) {
        out += &format!("    <string>{}</string>\n", xml(a));
    }
    out += "  </array>\n";
    if !t.env.is_empty() {
        out += "  <key>EnvironmentVariables</key>\n  <dict>\n";
        for (k, v) in &t.env {
            out += &format!("    <key>{}</key>\n    <string>{}</string>\n", xml(k), xml(v));
        }
        out += "  </dict>\n";
    }
    out += &format!("  <key>StartInterval</key>\n  <integer>{}</integer>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n", t.every_s);
    out
}

fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-=:,@+".contains(&b)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// cron's resolution is a minute, and `*/N` keeps an even cadence only when N divides 60.
fn cron(t: &TimerSpec, prog: &str) -> Result<String, String> {
    let n = t.every_s / 60;
    if !t.every_s.is_multiple_of(60) || n == 0 || !60u64.is_multiple_of(n) {
        return Err(format!("cron cannot run every {} s: the interval must be a whole number of minutes that divides an hour", t.every_s));
    }
    let minute = if n == 1 { "*".to_string() } else { format!("*/{n}") };
    let env: String = t.env.iter().map(|(k, v)| format!("{}=", k) + &sh_quote(v) + " ").collect();
    let cmd: Vec<String> = std::iter::once(prog).chain(t.args.iter().map(String::as_str)).map(sh_quote).collect();
    let run = if env.is_empty() { cmd.join(" ") } else { format!("/usr/bin/env {env}{}", cmd.join(" ")) };
    // cron turns a bare `%` into a newline.
    Ok(format!("{minute} * * * * {} # {}\n", run.replace('%', "\\%"), t.label))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> TimerSpec {
        TimerSpec::watchdog(PathBuf::from("/opt/spira/bin/spira-watchdog"), 120, vec![("SPIRA_SITE".into(), "/etc/spira dir/site.conf".into())])
    }

    fn file(files: &[(String, String)], name: &str) -> String {
        files.iter().find(|(n, _)| n == name).map(|(_, t)| t.clone()).unwrap_or_else(|| panic!("no {name}"))
    }

    #[test]
    fn linux_gets_a_oneshot_service_and_its_timer() {
        let files = render(Platform::for_os("linux"), &spec()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(
            file(&files, "spira-watchdog.service"),
            "[Unit]\nDescription=spira-watchdog — one pass: restart and page when the scheduler is wedged or absent\n\n[Service]\nType=oneshot\nEnvironment=\"SPIRA_SITE=/etc/spira dir/site.conf\"\nExecStart=/opt/spira/bin/spira-watchdog pass\nTimeoutStartSec=120\n"
        );
        assert_eq!(
            file(&files, "spira-watchdog.timer"),
            "[Unit]\nDescription=spira-watchdog timer\n\n[Timer]\nOnBootSec=120s\nOnUnitActiveSec=120s\nAccuracySec=1s\nUnit=spira-watchdog.service\n\n[Install]\nWantedBy=timers.target\n"
        );
    }

    #[test]
    fn macos_gets_a_launchd_plist_on_a_start_interval() {
        let files = render(Platform::for_os("macos"), &spec()).unwrap();
        assert_eq!(
            file(&files, "spira-watchdog.plist"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>spira-watchdog</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>/opt/spira/bin/spira-watchdog</string>\n    <string>pass</string>\n  </array>\n  <key>EnvironmentVariables</key>\n  <dict>\n    <key>SPIRA_SITE</key>\n    <string>/etc/spira dir/site.conf</string>\n  </dict>\n  <key>StartInterval</key>\n  <integer>120</integer>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n"
        );
    }

    #[test]
    fn anywhere_else_gets_one_crontab_line() {
        let files = render(Platform::for_os("freebsd"), &spec()).unwrap();
        assert_eq!(file(&files, "spira-watchdog.crontab"), "*/2 * * * * /usr/bin/env SPIRA_SITE='/etc/spira dir/site.conf' /opt/spira/bin/spira-watchdog pass # spira-watchdog\n");
        let mut every_minute = spec();
        every_minute.every_s = 60;
        every_minute.env.clear();
        assert_eq!(file(&render(Platform::Cron, &every_minute).unwrap(), "spira-watchdog.crontab"), "* * * * * /opt/spira/bin/spira-watchdog pass # spira-watchdog\n");
    }

    #[test]
    fn an_interval_a_platform_cannot_keep_is_refused() {
        for every in [0, 90, 420] {
            let mut t = spec();
            t.every_s = every;
            assert!(render(Platform::Cron, &t).is_err(), "cron every {every}");
        }
        let mut t = spec();
        t.every_s = 0;
        assert!(render(Platform::Systemd, &t).is_err());
    }

    #[test]
    fn values_are_quoted_for_each_format() {
        let t = TimerSpec { label: "w".into(), program: PathBuf::from("/b/w"), args: vec!["50%".into(), "a&b".into(), "it's".into()], env: vec![], every_s: 60 };
        assert!(file(&render(Platform::Systemd, &t).unwrap(), "w.service").contains("ExecStart=/b/w \"50%%\" a&b \"it's\"\n"));
        assert!(file(&render(Platform::Launchd, &t).unwrap(), "w.plist").contains("<string>a&amp;b</string>\n    <string>it&apos;s</string>"));
        assert!(file(&render(Platform::Cron, &t).unwrap(), "w.crontab").contains("/b/w '50\\%' 'a&b' 'it'\\''s' # w"));
    }
}
