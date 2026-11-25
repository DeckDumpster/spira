//! The interactive ops pane over an injected terminal and clock. It reads the one document the
//! scheduler keeps (`lcview::document_path`) on each repaint and draws it; it starts no program,
//! thread or store connection, and writes only the operator's own view state (`ui.json`) on a key
//! or a click.

use crate::lctui::{layout, parent_key, Frame, Mode, Ui};
use crate::lcview::{aged, document_path, read_document, view, View};
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long the loop waits for input before it looks at the clock and the document again.
pub const IDLE_POLL: Duration = Duration::from_secs(1);

pub trait Terminal {
    fn size(&mut self) -> (usize, usize);
    /// The next input within `wait`; `None` when `wait` passed with none.
    fn poll(&mut self, wait: Duration) -> io::Result<Option<Event>>;
    /// Replaces the screen with `lines`.
    fn draw(&mut self, lines: &[String]) -> io::Result<()>;
}

pub trait Clock {
    fn now(&self) -> i64;
}

pub fn load_ui(path: Option<&Path>) -> Ui {
    path.and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_ui(path: Option<&Path>, ui: &Ui) {
    let Some(p) = path else { return };
    let tmp = p.with_extension("json.tmp");
    let _ = std::fs::create_dir_all(p.parent().unwrap_or(Path::new(".")))
        .and_then(|_| std::fs::write(&tmp, serde_json::to_vec(ui).unwrap_or_default()))
        .and_then(|_| std::fs::rename(&tmp, p));
}

/// What the document is, as the loop tells whether a publish replaced it.
fn stamp(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path).ok().map(|m| (m.ino(), m.len()))
}

/// The view a repaint draws at `now`, or the line saying why there is none.
pub fn read_view(view_dir: &Path, now: i64) -> Result<View, String> {
    let (doc, untouched) = read_document(view_dir, now).map_err(|e| format!("lc-view: {e} — is the scheduler running?"))?;
    Ok(view(&aged(doc, untouched, now)))
}

/// Applies one input; `Some(true)` leaves, `Some(false)` asks for a repaint, `None` changes nothing.
fn handle(ev: Event, ui: &mut Ui, frame: &Frame, save: &mut dyn FnMut(&Ui)) -> Option<bool> {
    let pos = frame.order.iter().position(|k| *k == ui.selected).unwrap_or(0);
    let sel = frame.order.get(pos).cloned().unwrap_or_default();
    let (has_kids, showing) = frame.nodes.get(&sel).copied().unwrap_or((false, false));
    let select = |ui: &mut Ui, k: Option<&String>| {
        if let Some(k) = k {
            ui.selected = k.clone();
            ui.scroll = 0;
        }
    };
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
            KeyCode::Char('q') => return Some(true),
            KeyCode::Char('j') | KeyCode::Down => select(ui, frame.order.get(pos + 1)),
            KeyCode::Char('k') | KeyCode::Up => select(ui, pos.checked_sub(1).and_then(|p| frame.order.get(p))),
            KeyCode::Tab => {
                let next = frame.order.iter().skip(pos + 1).find(|k| !k.contains('/'));
                select(ui, next.or_else(|| frame.order.first()))
            }
            KeyCode::BackTab => {
                let prev = frame.order.iter().take(pos).rev().find(|k| !k.contains('/'));
                select(ui, prev)
            }
            KeyCode::Char('l') | KeyCode::Right if has_kids => {
                if showing {
                    select(ui, frame.order.get(pos + 1));
                } else {
                    ui.set(&sel, Mode::Open);
                    save(ui);
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if has_kids && showing {
                    ui.set(&sel, Mode::Collapsed);
                    save(ui);
                } else {
                    let up = parent_key(&sel).map(String::from);
                    select(ui, up.as_ref());
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') if has_kids => {
                ui.cycle(&sel);
                save(ui);
            }
            KeyCode::Char('a') => {
                ui.reset(&sel);
                save(ui);
            }
            KeyCode::Char('A') => {
                ui.modes.clear();
                ui.scroll = 0;
                save(ui);
            }
            KeyCode::PageDown => ui.scroll += 10,
            KeyCode::PageUp => ui.scroll = ui.scroll.saturating_sub(10),
            _ => return None,
        },
        Event::Mouse(m) => match m.kind {
            MouseEventKind::Down(MouseButton::Left) => match frame.rows.iter().find(|(r, _)| *r == m.row as usize) {
                Some((_, key)) => {
                    ui.selected = key.clone();
                    if frame.nodes.get(key).is_some_and(|n| n.0) {
                        ui.cycle(key);
                    }
                    save(ui);
                }
                None => return None,
            },
            MouseEventKind::ScrollDown => ui.scroll += 3,
            MouseEventKind::ScrollUp => ui.scroll = ui.scroll.saturating_sub(3),
            _ => return None,
        },
        Event::Resize(..) => {}
        _ => return None,
    }
    Some(false)
}

/// Draws the document in `view_dir` every `secs`, and at once on a key, a click, a resize or
/// a replaced document, until `q`. `ui_path` holds the operator's folds and selection.
pub fn run(term: &mut dyn Terminal, clock: &dyn Clock, view_dir: &Path, ui_path: Option<PathBuf>, secs: u64) -> io::Result<()> {
    let path = document_path(view_dir);
    let mut ui = load_ui(ui_path.as_deref());
    let mut frame = Frame::default();
    let mut seen: Option<Option<(u64, u64)>> = None;
    let mut painted_at = i64::MIN;
    let mut dirty = true;
    loop {
        let now = clock.now();
        let at = stamp(&path);
        if seen != Some(at) || now - painted_at >= secs as i64 {
            dirty = true;
        }
        if dirty {
            let (w, h) = term.size();
            match read_view(view_dir, now) {
                Ok(v) => {
                    if seen != Some(at) && v.errors.is_empty() && !frame.all_keys.is_empty() {
                        let fresh = layout(&v, &ui, 200, 1000).all_keys;
                        ui.modes.retain(|k, _| !k.contains("sp-") || fresh.contains(k));
                    }
                    frame = layout(&v, &ui, w, h);
                    term.draw(&frame.lines)?;
                }
                Err(line) => term.draw(&[line])?,
            }
            seen = Some(at);
            painted_at = now;
            dirty = false;
        }
        if let Some(ev) = term.poll(IDLE_POLL)? {
            let mut save = |ui: &Ui| save_ui(ui_path.as_deref(), ui);
            match handle(ev, &mut ui, &frame, &mut save) {
                Some(true) => return Ok(()),
                Some(false) => dirty = true,
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lcview::{render, Snapshot};

    #[test_id(tn3v7hx)]
    #[test]
    fn ages_advance_between_renders_of_one_document() {
        let dir = testkit::TempDir::new("lcpane-ages");
        let path = document_path(dir.path());
        std::fs::write(&path, serde_json::to_vec(&Snapshot { now: 1_000, good_at: 1_000, ..Default::default() }).unwrap()).unwrap();
        std::fs::File::open(&path).unwrap().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_000)).unwrap();
        let at = |now| render(&read_view(dir.path(), now).unwrap(), 120).join("\n");
        assert_ne!(at(1_000 + 400), at(1_000 + 460), "the age moves with the clock, not with a gather");
        assert!(at(1_000 + 400).contains("last good read"), "a document untouched for over the warning age says so");
        assert!(!at(1_000 + 10).contains("last good read"), "a recently touched one does not");
    }


    #[test_id(tkscbjn)]
    #[test]
    fn a_missing_document_is_a_line_naming_its_path() {
        let dir = testkit::TempDir::new("lcpane-missing");
        let line = read_view(&dir.join("nowhere"), 1).err().unwrap();
        assert!(line.contains("nowhere/snapshot.json") && line.contains("scheduler"), "{line}");
    }
}
