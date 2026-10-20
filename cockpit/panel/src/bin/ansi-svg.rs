//! ANSI text on stdin -> an SVG screenshot on stdout.
//!
//!     tmux capture-pane -p -e -t <pane> | ansi-svg > shot.svg
//!
//! A TUI on a box with no image tooling still owes before/after images
//! (law-ui-changes-need-screenshots); an SVG is an image that costs no dependency. Reads SGR
//! escapes only, which is all the panel emits; anything else is passed over.

use std::io::{Read, Write};

const PAL: [&str; 16] = [
    "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
    "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
];
const DEF_FG: &str = "#d0d0d0";
const DEF_BG: &str = "#101010";
const CW: f64 = 8.4;
const CH: f64 = 17.0;
const PAD: f64 = 10.0;
const FS: f64 = 14.0;

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    fg: &'static str,
    bg: &'static str,
    bold: bool,
}

#[derive(Default)]
struct Pen {
    fg: Option<&'static str>,
    bg: Option<&'static str>,
    bold: bool,
    rev: bool,
}

impl Pen {
    fn push(&self, out: &mut Vec<Cell>, ch: char) {
        let f = if self.bold && self.fg.is_none() { PAL[15] } else { self.fg.unwrap_or(DEF_FG) };
        let b = self.bg.unwrap_or(DEF_BG);
        let (fg, bg) = if self.rev { (b, f) } else { (f, b) };
        out.push(Cell { ch, fg, bg, bold: self.bold });
    }

    fn apply(&mut self, c: i64) {
        match c {
            0 => *self = Pen::default(),
            1 => self.bold = true,
            21 | 22 => self.bold = false,
            7 => self.rev = true,
            27 => self.rev = false,
            30..=37 => self.fg = Some(PAL[(c - 30) as usize]),
            39 => self.fg = None,
            40..=47 => self.bg = Some(PAL[(c - 40) as usize]),
            49 => self.bg = None,
            90..=97 => self.fg = Some(PAL[(c - 90 + 8) as usize]),
            100..=107 => self.bg = Some(PAL[(c - 100 + 8) as usize]),
            _ => {}
        }
    }
}

fn cells(line: &str) -> Vec<Cell> {
    let mut pen = Pen::default();
    let mut out = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let Some(esc) = rest.find("\x1b[") else { break };
        let body = &rest[esc + 2..];
        let n = body.bytes().take_while(|b| b.is_ascii_digit() || *b == b';').count();
        if body.as_bytes().get(n) != Some(&b'm') {
            for ch in rest[..esc + 2].chars() {
                pen.push(&mut out, ch);
            }
            rest = body;
            continue;
        }
        for ch in rest[..esc].chars() {
            pen.push(&mut out, ch);
        }
        for code in body[..n].split(';') {
            pen.apply(code.parse().unwrap_or(0));
        }
        rest = &body[n + 1..];
    }
    for ch in rest.chars() {
        pen.push(&mut out, ch);
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

fn render(input: &str) -> String {
    let rows: Vec<Vec<Cell>> = input.trim_end_matches('\n').split('\n').map(cells).collect();
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let w = PAD * 2.0 + cols as f64 * CW;
    let h = PAD * 2.0 + rows.len() as f64 * CH;
    let mut o = vec![
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.0}\" height=\"{h:.0}\" viewBox=\"0 0 {w:.1} {h:.1}\" font-family=\"DejaVu Sans Mono,Menlo,monospace\" font-size=\"{FS:.1}\">"
        ),
        format!("<rect width=\"100%\" height=\"100%\" fill=\"{DEF_BG}\"/>"),
    ];
    for (y, row) in rows.iter().enumerate() {
        let mut x = 0;
        while x < row.len() {
            let bg = row[x].bg;
            let n = 1 + row[x + 1..].iter().take_while(|c| c.bg == bg).count();
            if bg != DEF_BG {
                o.push(format!(
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{CH:.1}\" fill=\"{bg}\"/>",
                    PAD + x as f64 * CW,
                    PAD + y as f64 * CH,
                    n as f64 * CW + 0.5
                ));
            }
            x += n;
        }
    }
    for (y, row) in rows.iter().enumerate() {
        let mut x = 0;
        while x < row.len() {
            let (fg, bold) = (row[x].fg, row[x].bold);
            let n = 1 + row[x + 1..].iter().take_while(|c| (c.fg, c.bold) == (fg, bold)).count();
            let text: String = row[x..x + n].iter().map(|c| c.ch).collect();
            if !text.trim().is_empty() {
                let wt = if bold { " font-weight=\"bold\"" } else { "" };
                o.push(format!(
                    "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{fg}\"{wt} xml:space=\"preserve\">{}</text>",
                    PAD + x as f64 * CW,
                    PAD + y as f64 * CH + FS * 0.8,
                    escape(&text)
                ));
            }
            x += n;
        }
    }
    o.push("</svg>".to_string());
    o.join("\n") + "\n"
}

fn main() {
    let mut input = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut input);
    let _ = std::io::stdout().write_all(render(&String::from_utf8_lossy(&input)).as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_bold_and_reverse_render() {
        let svg = render("\x1b[31mred\x1b[0m \x1b[1mB\x1b[0m\x1b[7mR\x1b[0m<&>");
        assert!(svg.contains("fill=\"#cd0000\" xml:space=\"preserve\">red</text>"), "{svg}");
        assert!(svg.contains("fill=\"#ffffff\" font-weight=\"bold\""), "{svg}");
        assert!(svg.contains("<rect x=\"") && svg.contains("fill=\"#d0d0d0\"/>"), "reverse draws a bar: {svg}");
        assert!(svg.contains("&lt;&amp;&gt;"));
    }
}
