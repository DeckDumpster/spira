//! `lc-view tui|once|fit|loop [secs]` — the lifecycle-lens ops pane. It draws the one document the
//! scheduler writes under the user runtime directory (`cockpit_ops::lcview::document_path`, under `spira_config::cockpit_view_dir`) and
//! gathers nothing itself.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
use cockpit_ops::lcpane::{self, Clock, Terminal};
use cockpit_ops::lctui::layout;
use cockpit_ops::lcview::render;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::{cursor, execute, terminal};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn width() -> usize {
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(120)
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn view_dir() -> Result<PathBuf, String> {
    std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()).map(|d| spira_config::cockpit_view_dir(std::path::Path::new(&d))).ok_or_else(|| "lc-view: XDG_RUNTIME_DIR is unset — there is no runtime directory to read the document from".to_string())
}

fn ui_path() -> Result<PathBuf, String> {
    let run = spira_config::process::cfg("SPIRA_RUN").map_err(|e| format!("lc-view: {e}"))?;
    Ok(std::path::Path::new(run.trim()).join("lcview").join("ui.json"))
}

struct Clock0;

impl Clock for Clock0 {
    fn now(&self) -> i64 {
        unix_now()
    }
}

struct Screen(std::io::Stdout);

impl Terminal for Screen {
    fn size(&mut self) -> (usize, usize) {
        terminal::size().map(|(w, h)| (w as usize, h as usize)).unwrap_or((width(), 40))
    }

    fn poll(&mut self, wait: Duration) -> std::io::Result<Option<Event>> {
        if event::poll(wait)? {
            return event::read().map(Some);
        }
        Ok(None)
    }

    fn draw(&mut self, lines: &[String]) -> std::io::Result<()> {
        let mut buf = String::from("\x1b[H");
        for (i, l) in lines.iter().enumerate() {
            buf.push_str(l);
            buf.push_str("\x1b[K");
            if i + 1 < lines.len() {
                buf.push_str("\r\n");
            }
        }
        buf.push_str("\x1b[J");
        write!(self.0, "{buf}")?;
        self.0.flush()
    }
}

fn interactive(dir: &std::path::Path, ui: PathBuf, secs: u64) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, EnableMouseCapture, cursor::Hide, terminal::DisableLineWrap)?;
    let res = lcpane::run(&mut Screen(std::io::stdout()), &Clock0, dir, Some(ui), secs);
    let _ = execute!(out, terminal::EnableLineWrap, cursor::Show, DisableMouseCapture, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    res
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let usage = |other: &str| {
        eprintln!("usage: lc-view tui [secs] | once | fit | loop [secs] (unknown: {other})");
        std::process::exit(2);
    };
    let mode = args.get(1).map(String::as_str);
    if !matches!(mode, Some("tui" | "loop" | "once" | "fit") | None) {
        usage(mode.unwrap_or(""));
    }
    let dir = view_dir().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let secs = |default: u64| args.get(2).and_then(|a| a.parse().ok()).unwrap_or(default);
    let drawn = || lcpane::read_view(&dir, unix_now());
    let ui = || ui_path().map_err(|e| e.to_string());
    match mode {
        Some("tui") => {
            if let Err(e) = ui().and_then(|ui| interactive(&dir, ui, secs(15)).map_err(|e| format!("lc-view tui: {e}"))) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("loop") => loop {
            let frame = drawn().map(|v| render(&v, width())).unwrap_or_else(|e| vec![e]);
            println!("\x1b[H\x1b[2J{}", frame.join("\n"));
            std::thread::sleep(Duration::from_secs(secs(10)));
        },
        Some("fit") => {
            let h = std::env::var("LINES").ok().and_then(|c| c.parse().ok()).unwrap_or(50);
            match (ui(), drawn()) {
                (Ok(ui), Ok(v)) => println!("{}", layout(&v, &lcpane::load_ui(Some(&ui)), width(), h).lines.join("\n")),
                (Err(e), _) | (_, Err(e)) => println!("{e}"),
            }
        }
        _ => println!("{}", drawn().map(|v| render(&v, width())).unwrap_or_else(|e| vec![e]).join("\n")),
    }
}

#[cfg(test)]
mod tests {
    fn removed_calls(src: &str) -> Vec<&'static str> {
        let code: String = src.split("#[cfg(test)]").next().unwrap_or("").lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        let mut found = Vec::new();
        if code.contains("run(\"work\"") {
            found.push("work");
        }
        if code.contains("lc_json(&[\"list\"])") || code.contains("run(\"spira-lc\", &[\"list\"])") {
            found.push("unfiltered spira-lc list");
        }
        if code.contains("lc_json(&[\"list\", \"--delivery\"])") {
            found.push("list --delivery");
        }
        if code.contains("spira-claim") {
            found.push("spira-claim");
        }
        for gathering in ["Command::new", "thread::spawn", "store_view::"] {
            if code.contains(gathering) {
                found.push(gathering);
            }
        }
        found
    }

    #[test_id(tq16fci)]
    #[test]
    fn the_render_issues_none_of_the_removed_calls() {
        assert!(removed_calls(include_str!("lcview_main.rs")).is_empty());
        assert!(removed_calls(include_str!("lcpane.rs")).is_empty(), "the loop it runs gathers nothing either");
    }

    #[test_id(txi08zu)]
    #[test]
    fn the_scan_sees_a_planted_spawn_a_planted_thread_and_a_planted_store_read() {
        for planted in ["fn f() { Command::new(\"git\"); }", "fn f() { std::thread::spawn(|| ()); }", "fn f() { store_view::held_read(&[]); }"] {
            assert_eq!(removed_calls(planted).len(), 1, "{planted}");
        }
        assert!(removed_calls("fn f() {}\n#[cfg(test)]\nmod t { fn g() { Command::new(\"x\"); } }").is_empty());
    }
}
