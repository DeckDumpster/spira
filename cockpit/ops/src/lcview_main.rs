//! `lc-view tui|once|fit|loop [secs]` — the lifecycle-lens ops pane (sp-lpw5ol; interactive since sp-5j35g5). Gathers from the built
//! tools only: `spira-lc list` (state), `work list --json` (titles, priority), the landing ref's
//! commits (drift), `world status`, and the aeon ceiling from config. Never runs `bd`.

use cockpit_ops::lctui::{layout, parent_key, Frame, Mode, Ui};
use cockpit_ops::lcview::{own_ids, render, tail_lines, view, View, BatchRow, DwellRow, EdgeRow, GraphEdge, Meta, Row, Snapshot, Tail, TAIL_BYTES};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new("timeout")
        .arg("5")
        .arg(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{cmd} {}: exit {} {}", args.join(" "), out.status.code().unwrap_or(-1), err.lines().last().unwrap_or("")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The "plane work" line of `world status`, which it prints first (within a second); the rest of
/// its report takes it tens of seconds, so read until the line and stop. The 5 s deadline bounds
/// the whole call, so nothing it spawned outlives it.
fn world_plane() -> Result<String, String> {
    use std::io::BufRead;
    let mut child = Command::new("timeout")
        .args(["5", "world", "status"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("world status: {e}"))?;
    let found = child
        .stdout
        .take()
        .and_then(|o| std::io::BufReader::new(o).lines().map_while(Result::ok).find(|l| l.contains("plane work")));
    let _ = child.kill();
    let _ = child.wait();
    found.ok_or_else(|| "world status: no 'plane work' line".into())
}

fn num(v: &serde_json::Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn truthy(v: &serde_json::Value) -> bool {
    v.as_bool().unwrap_or_else(|| num(v).is_some_and(|n| n != 0))
}

fn read_tail(path: &std::path::Path) -> Option<Tail> {
    let mut f = std::fs::File::open(path).ok()?;
    let meta = f.metadata().ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    let start = meta.len().saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(TAIL_BYTES).read_to_end(&mut buf).ok()?;
    Some(Tail { lines: tail_lines(&buf, start > 0, 2), mtime })
}

fn lc_json(args: &[&str]) -> Result<Vec<serde_json::Value>, String> {
    let name = format!("spira-lc {}", args.join(" "));
    run("spira-lc", args)
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| format!("{name}: {e}")))
        .map(|v| v.as_array().cloned().unwrap_or_default())
}

fn text(v: &serde_json::Value) -> String {
    v.as_str().unwrap_or("").into()
}

fn gather_state_machine(s: &mut Snapshot) {
    match lc_json(&["ops-graph"]) {
        Ok(rows) => s.graph = rows.iter().map(|g| GraphEdge { from: text(&g["from"]), event: text(&g["event"]), to: text(&g["to"]) }).collect(),
        Err(e) => s.errors.push(e),
    }
    match lc_json(&["ops-view", "ops_edges"]) {
        Ok(rows) => {
            s.edges = rows
                .iter()
                .map(|e| EdgeRow {
                    kind: text(&e["kind"]),
                    from_state: text(&e["from_state"]),
                    to_state: e["to_state"].as_str().map(String::from),
                    event: e["event"].as_str().map(String::from),
                    refusal: e["refusal"].as_str().map(String::from),
                    n_1h: num(&e["n_1h"]).unwrap_or(0),
                })
                .collect()
        }
        Err(e) => s.errors.push(e),
    }
    match lc_json(&["ops-view", "ops_dwell"]) {
        Ok(rows) => {
            s.dwell = rows
                .iter()
                .map(|d| DwellRow { bead_id: text(&d["bead_id"]), state: text(&d["state"]), entered_at: num(&d["entered_at"]).unwrap_or(0), p95_s: num(&d["p95_s"]) })
                .collect()
        }
        Err(e) => s.errors.push(e),
    }
    match lc_json(&["list", "--batches"]) {
        Ok(rows) => {
            s.batches = rows
                .iter()
                .map(|b| BatchRow {
                    id: text(&b["batch_id"]),
                    state: text(&b["state"]),
                    last_at: num(&b["last_at"]).or_else(|| num(&b["opened_at"])).unwrap_or(0),
                    members: b["members"].as_array().map(|arr| arr.iter().map(|x| text(&x["bead_id"])).collect()).unwrap_or_default(),
                    opened_at: num(&b["opened_at"]).unwrap_or(0),
                    eject_at: Vec::new(),
                    ejected: b["ejected"]
                        .as_array()
                        .map(|arr| arr.iter().map(|x| x["bead_id"].as_str().map(String::from).unwrap_or_else(|| text(x))).collect())
                        .unwrap_or_default(),
                })
                .collect()
        }
        Err(e) => s.errors.push(e),
    }
    // Eject times for the newest round only: that is the one the pane names.
    if let Some(b) = s.batches.iter_mut().max_by_key(|b| b.opened_at.max(b.last_at)).filter(|b| !b.ejected.is_empty()) {
        match lc_json(&["history", &b.id, "--machine", "batch"]) {
            Ok(evs) => {
                b.eject_at = evs
                    .iter()
                    .filter(|e| e["evidence"].as_str().is_some_and(|x| x.starts_with("{\"Eject\"")))
                    .filter_map(|e| num(&e["at"]))
                    .collect()
            }
            Err(e) => s.errors.push(e),
        }
    }
}

fn gather() -> Snapshot {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let mut s = Snapshot { now, ..Default::default() };
    s.release = std::env::var("SPIRA_RELEASE")
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "?".into());

    match run("spira-lc", &["list"]).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| format!("spira-lc list: {e}"))) {
        Ok(v) => {
            for r in v.as_array().cloned().unwrap_or_default() {
                let holds: Vec<String> = r["holds"].as_str().and_then(|h| serde_json::from_str(h).ok()).unwrap_or_default();
                s.rows.push(Row {
                    id: r["bead_id"].as_str().unwrap_or("").into(),
                    state: r["state"].as_str().unwrap_or("").into(),
                    holder: r["holder"].as_str().map(String::from),
                    holds,
                    reason: r["reason"].as_str().map(String::from),
                    updated_at: num(&r["updated_at"]).unwrap_or(0),
                    since: num(&r["since"]).or_else(|| num(&r["updated_at"])).unwrap_or(0),
                    lease_until: num(&r["lease_until"]),
                    persona: r["persona"].as_str().map(String::from),
                    rework: truthy(&r["rework"]),
                    claimable: (!r["claimable"].is_null()).then(|| truthy(&r["claimable"])),
                    blocker: r["blocker"].as_str().map(String::from),
                });
            }
        }
        Err(e) => s.errors.push(e),
    }
    match run("work", &["list", "--json"]).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| format!("work list: {e}"))) {
        Ok(v) => {
            for b in v.as_array().cloned().unwrap_or_default() {
                if let Some(id) = b["id"].as_str() {
                    s.meta.insert(id.into(), Meta {
                            title: b["title"].as_str().unwrap_or("").into(),
                            priority: num(&b["priority"]),
                            labels: b["labels"].as_array().map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect()).unwrap_or_default(),
                        });
                }
            }
        }
        Err(e) => s.errors.push(e),
    }
    let (root, base) = (run("spira-config", &["repo", "root", "spira"]), run("spira-config", &["repo", "base", "spira"]));
    match (root, base) {
        (Ok(root), Ok(base)) => {
            s.base = base.trim().into();
            match run("git", &["-C", root.trim(), "log", "--format=%H %s", "-n", "4000", &format!("refs/heads/{}", s.base)]) {
                Ok(log) => {
                    let mut m = HashMap::new();
                    for l in log.lines() {
                        if let Some((h, subj)) = l.split_once(' ') {
                            for id in own_ids(subj) {
                                m.entry(id).or_insert_with(|| h.to_string());
                            }
                        }
                    }
                    s.on_base = m;
                }
                Err(e) => s.errors.push(e),
            }
        }
        (Err(e), _) | (_, Err(e)) => s.errors.push(e),
    }
    s.world = world_plane().unwrap_or_else(|e| {
        s.errors.push(e);
        "?".into()
    });
    if let Ok(run) = spira_config::process::cfg("SPIRA_RUN") {
        for r in s.rows.iter().filter(|r| r.state == "WORKING" && r.holder.is_some()) {
            if let Some(t) = read_tail(&std::path::Path::new(run.trim()).join(format!("{}.log", r.id))) {
                s.tails.insert(r.id.clone(), t);
            }
        }
    }
    gather_state_machine(&mut s);
    s.ask_label = spira_config::process::cfg("SPIRA_ASK_LABEL").map(|v| v.trim().to_string()).unwrap_or_default();
    s.ceiling = spira_config::process::cfg("SPIRA_MAX_LIVE_AEONS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    s
}

/// Write the snapshot loom serves (`/lifecycle`), atomically, so the phone page shows exactly
/// what this pane just drew. A failure is printed in the frame's place, never swallowed.
fn publish(s: &Snapshot) {
    let Ok(run) = spira_config::process::cfg("SPIRA_RUN") else {
        eprintln!("lc-view: SPIRA_RUN does not resolve — the phone page will go stale");
        return;
    };
    let dir = std::path::Path::new(run.trim()).join("lcview");
    let tmp = dir.join(".snapshot.json.tmp");
    let res = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(&tmp, serde_json::to_vec(s).unwrap_or_default()))
        .and_then(|_| std::fs::rename(&tmp, dir.join("snapshot.json")));
    if let Err(e) = res {
        eprintln!("lc-view: cannot publish the snapshot under {}: {e}", dir.display());
    }
}

fn width() -> usize {
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(120)
}

fn ui_path() -> Option<std::path::PathBuf> {
    let run = spira_config::process::cfg("SPIRA_RUN").ok()?;
    Some(std::path::Path::new(run.trim()).join("lcview").join("ui.json"))
}

fn load_ui() -> Ui {
    ui_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_ui(ui: &Ui) {
    let Some(p) = ui_path() else { return };
    let tmp = p.with_extension("json.tmp");
    let _ = std::fs::create_dir_all(p.parent().unwrap_or(std::path::Path::new(".")))
        .and_then(|_| std::fs::write(&tmp, serde_json::to_vec(ui).unwrap_or_default()))
        .and_then(|_| std::fs::rename(&tmp, &p));
}

/// The interactive pane: draws exactly the terminal's size, redraws at once on a key, a click or a
/// resize, and gathers fresh data every `secs` on a worker thread so input never waits on a slow
/// store. q leaves.
fn interactive(secs: u64) -> std::io::Result<()> {
    use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
    use crossterm::{cursor, execute, terminal};
    use std::io::Write;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<Snapshot>();
    std::thread::spawn(move || loop {
        let snap = gather();
        publish(&snap);
        if tx.send(snap).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_secs(secs));
    });

    let mut out = std::io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, EnableMouseCapture, cursor::Hide, terminal::DisableLineWrap)?;
    let restore = |out: &mut std::io::Stdout| {
        let _ = execute!(out, terminal::EnableLineWrap, cursor::Show, DisableMouseCapture, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    };

    let mut ui = load_ui();
    let mut v: Option<View> = None;
    let mut frame = Frame::default();
    let mut said_waiting = false;
    let res = (|| -> std::io::Result<()> {
        loop {
            let mut dirty = v.is_none() && !std::mem::replace(&mut said_waiting, true);
            while let Ok(snap) = rx.try_recv() {
                let nv = view(&snap);
                // Forget choices for beads that have left the tree, once a whole gather says so.
                if nv.errors.is_empty() && !frame.all_keys.is_empty() {
                    let fresh = layout(&nv, &ui, 200, 1000).all_keys;
                    let before = ui.modes.len();
                    ui.modes.retain(|k, _| !k.contains("sp-") || fresh.contains(k));
                    if ui.modes.len() != before {
                        save_ui(&ui);
                    }
                }
                v = Some(nv);
                dirty = true;
            }
            if event::poll(Duration::from_millis(if v.is_some() { 250 } else { 50 }))? {
                dirty = true;
                let pos = frame.order.iter().position(|k| *k == ui.selected).unwrap_or(0);
                let sel = frame.order.get(pos).cloned().unwrap_or_default();
                let (has_kids, showing) = frame.nodes.get(&sel).copied().unwrap_or((false, false));
                let select = |ui: &mut Ui, k: Option<&String>| {
                    if let Some(k) = k {
                        ui.selected = k.clone();
                        ui.scroll = 0;
                    }
                };
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('j') | KeyCode::Down => select(&mut ui, frame.order.get(pos + 1)),
                        KeyCode::Char('k') | KeyCode::Up => select(&mut ui, pos.checked_sub(1).and_then(|p| frame.order.get(p))),
                        KeyCode::Tab => {
                            // Next top-level section.
                            let next = frame.order.iter().skip(pos + 1).find(|k| !k.contains('/'));
                            select(&mut ui, next.or_else(|| frame.order.first()))
                        }
                        KeyCode::BackTab => {
                            let prev = frame.order.iter().take(pos).rev().find(|k| !k.contains('/'));
                            select(&mut ui, prev)
                        }
                        KeyCode::Char('l') | KeyCode::Right if has_kids => {
                            if showing {
                                select(&mut ui, frame.order.get(pos + 1));
                            } else {
                                ui.set(&sel, Mode::Open);
                                save_ui(&ui);
                            }
                        }
                        KeyCode::Char('h') | KeyCode::Left => {
                            if has_kids && showing {
                                ui.set(&sel, Mode::Collapsed);
                                save_ui(&ui);
                            } else {
                                let up = parent_key(&sel).map(String::from);
                                select(&mut ui, up.as_ref());
                            }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') if has_kids => {
                            ui.cycle(&sel);
                            save_ui(&ui);
                        }
                        KeyCode::Char('a') => {
                            ui.reset(&sel);
                            save_ui(&ui);
                        }
                        KeyCode::Char('A') => {
                            ui.modes.clear();
                            ui.scroll = 0;
                            save_ui(&ui);
                        }
                        KeyCode::PageDown => ui.scroll += 10,
                        KeyCode::PageUp => ui.scroll = ui.scroll.saturating_sub(10),
                        _ => dirty = false,
                    },
                    Event::Mouse(m) => match m.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if let Some((_, key)) = frame.rows.iter().find(|(r, _)| *r == m.row as usize) {
                                ui.selected = key.clone();
                                if frame.nodes.get(key).is_some_and(|n| n.0) {
                                    ui.cycle(key);
                                }
                                save_ui(&ui);
                            } else {
                                dirty = false;
                            }
                        }
                        MouseEventKind::ScrollDown => ui.scroll += 3,
                        MouseEventKind::ScrollUp => ui.scroll = ui.scroll.saturating_sub(3),
                        _ => dirty = false,
                    },
                    Event::Resize(..) => {}
                    _ => dirty = false,
                }
            }
            if !dirty {
                continue;
            }
            let (w, h) = terminal::size().map(|(w, h)| (w as usize, h as usize)).unwrap_or((width(), 40));
            let Some(view) = v.as_ref() else {
                execute!(out, cursor::MoveTo(0, 0), terminal::Clear(terminal::ClearType::All))?;
                write!(out, "lc-view: gathering…")?;
                out.flush()?;
                continue;
            };
            frame = layout(view, &ui, w, h);
            let mut buf = String::from("\x1b[H");
            for (i, l) in frame.lines.iter().enumerate() {
                buf.push_str(l);
                buf.push_str("\x1b[K");
                if i + 1 < frame.lines.len() {
                    buf.push_str("\r\n");
                }
            }
            buf.push_str("\x1b[J");
            write!(out, "{buf}")?;
            out.flush()?;
        }
    })();
    restore(&mut out);
    res
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("tui") => {
            let secs: u64 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(15);
            if let Err(e) = interactive(secs) {
                eprintln!("lc-view tui: {e}");
                std::process::exit(1);
            }
        }
        Some("loop") => {
            let secs: u64 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(10);
            loop {
                let snap = gather();
                publish(&snap);
                let frame = render(&view(&snap), width());
                print!("\x1b[H\x1b[2J{}\n", frame.join("\n"));
                std::thread::sleep(Duration::from_secs(secs));
            }
        }
        Some("once") | None => println!("{}", render(&view(&gather()), width()).join("\n")),
        Some("fit") => {
            // One frame of the TUI layout at COLUMNS x LINES, for a non-interactive look.
            let h = std::env::var("LINES").ok().and_then(|c| c.parse().ok()).unwrap_or(50);
            println!("{}", layout(&view(&gather()), &load_ui(), width(), h).lines.join("\n"));
        }
        Some(other) => {
            eprintln!("usage: lc-view tui [secs] | once | fit | loop [secs] (unknown: {other})");
            std::process::exit(2);
        }
    }
}
