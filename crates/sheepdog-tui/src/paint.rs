use crate::app::{App, Modal, RenameTarget, filtered};
use crate::input::Field;
use crate::layout::{self, Rect};
use blit_remote::FrameState;
use sheepdog_client::ViewSnapshot;
use sheepdog_core::{EndReason, TerminalState};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn clean(text: &str, width: usize) -> String {
    let safe: String = text
        .chars()
        .filter(|c| {
            !c.is_control()
                && !('\u{202a}'..='\u{202e}').contains(c)
                && !('\u{2066}'..='\u{2069}').contains(c)
        })
        .collect();
    let mut out = String::new();
    let mut used = 0;
    for g in safe.graphemes(true) {
        let n = g.width();
        if used + n > width {
            break;
        }
        used += n;
        out.push_str(g);
    }
    out
}
fn field_window(f: &Field, width: u16) -> (String, usize) {
    let mut start = f.cursor;
    let mut x = 0;
    for (i, g) in f.text[..f.cursor].grapheme_indices(true).rev() {
        let n = clean(g, usize::MAX).width();
        if x + n > width.saturating_sub(1) as usize {
            break;
        }
        start = i;
        x += n;
    }
    (clean(&f.text[start..], width as usize), x)
}
fn line(out: &mut Vec<u8>, rect: Rect, text: &str, active: bool) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    out.extend_from_slice(
        format!(
            "\x1b[{};{}H\x1b[0m{}",
            rect.y + 1,
            rect.x + 1,
            if active { "\x1b[7m" } else { "" }
        )
        .as_bytes(),
    );
    let text = clean(text, rect.width as usize);
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(
        " ".repeat((rect.width as usize).saturating_sub(text.width()))
            .as_bytes(),
    );
    out.extend_from_slice(b"\x1b[0m");
}
pub fn crop(frame: &FrameState, rows: u16, cols: u16) -> FrameState {
    let rows = rows.min(frame.rows());
    let cols = cols.min(frame.cols());
    assert!(
        rows > 0 && cols > 0,
        "terminal crops require positive dimensions"
    );
    let mut cells = Vec::with_capacity(rows as usize * cols as usize * 12);
    for row in 0..rows {
        let start = row as usize * frame.cols() as usize * 12;
        cells.extend_from_slice(&frame.cells()[start..start + cols as usize * 12]);
        if cols < frame.cols() && frame.cells()[start + cols as usize * 12 + 1] & 2 != 0 {
            let last = cells.len() - 12;
            cells[last + 1] = (cells[last + 1] & !0x3a) | 8;
            cells[last + 8..last + 12].copy_from_slice(b" \0\0\0");
        }
    }
    let mut clipped = FrameState::from_parts(
        rows,
        cols,
        frame.cursor_row().min(rows - 1),
        frame.cursor_col().min(cols - 1),
        frame.mode(),
        frame.title(),
        cells,
    );
    for (&idx, text) in frame.overflow() {
        let row = idx / frame.cols() as usize;
        let col = idx % frame.cols() as usize;
        if row < rows as usize
            && col < cols as usize
            && clipped.cells()[(row * cols as usize + col) * 12 + 1] >> 3 & 7 == 7
        {
            clipped
                .overflow_mut()
                .insert(row * cols as usize + col, text.clone());
        }
    }
    clipped
}
fn terminal(out: &mut Vec<u8>, frame: &FrameState, body: Rect, live: bool) {
    let cursor_visible = live
        && frame.mode() & quosh_proto::MODE_CURSOR_VISIBLE != 0
        && frame.cursor_row() < body.height.min(300)
        && frame.cursor_col() < body.width.min(500);
    let frame = crop(frame, body.height.min(300), body.width.min(500));
    // frame_ansi's CRLF resets the physical column to 1. Render independent
    // rows at the body origin so terminal output can never overwrite sidebar.
    for row in 0..frame.rows() {
        let start = row as usize * frame.cols() as usize * 12;
        let mut one = FrameState::from_parts(
            1,
            frame.cols(),
            0,
            0,
            frame.mode(),
            "",
            frame.cells()[start..start + frame.cols() as usize * 12].to_vec(),
        );
        for (&idx, text) in frame.overflow() {
            if idx / frame.cols() as usize == row as usize {
                one.overflow_mut()
                    .insert(idx % frame.cols() as usize, text.clone());
            }
        }
        out.extend_from_slice(format!("\x1b[{};{}H", body.y + row + 1, body.x + 1).as_bytes());
        out.extend_from_slice(&quosh_proto::frame_ansi(&one));
    }
    if cursor_visible {
        out.extend_from_slice(
            format!(
                "\x1b[{};{}H\x1b[?25h",
                body.y + frame.cursor_row() + 1,
                body.x + frame.cursor_col() + 1
            )
            .as_bytes(),
        );
    }
}
pub fn render(app: &App, host: &str, view: Option<&ViewSnapshot>) -> Vec<u8> {
    let mut out = b"\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[0m\x1b[H\x1b[2J".to_vec();
    if !app.usable() {
        line(
            &mut out,
            Rect {
                x: 0,
                y: 0,
                width: app.width,
                height: 1,
            },
            "Resize terminal to at least 12×5",
            false,
        );
        out.extend_from_slice(b"\x1b[?7h\x1b[?2026l");
        return out;
    }
    let layout = layout::compute(app);
    let body = layout.body;
    for e in &layout.entries {
        line(&mut out, e.rect, &e.text, e.active);
    }
    if layout.sidebar > 0 {
        for y in 0..app.height {
            line(
                &mut out,
                Rect {
                    x: layout.sidebar - 1,
                    y,
                    width: 1,
                    height: 1,
                },
                "│",
                false,
            );
        }
    }
    if let Some(modal) = &app.modal {
        let lines: Vec<String> =
            match modal {
                Modal::Help => [
                    "Ctrl+B prefix; mouse selects Spaces/terminals/Tabs",
                    "c new Tab   N new Space (shell and name automatic)",
                    "n/p next/previous Tab   1–9 jump",
                    "w Spaces picker   g Goto (/ search)",
                    "r rename Space   , rename Tab (or right-click)",
                    "X close Tab   D close Space (confirmed)",
                    "Enter explicitly reopens ended Tab   q detach",
                    "P arm ONE literal paste; File Ingest is pending",
                    "Esc closes dialogs; Ctrl+B Ctrl+B sends Ctrl+B",
                    "Agent recognition/Attention and history/copy pending",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
                Modal::Rename(target, f) => vec![
                    format!(
                        "Rename {} — Enter saves; Esc cancels",
                        match target {
                            RenameTarget::Space(_) => "Space",
                            RenameTarget::Tab(_) => "Tab",
                        }
                    ),
                    field_window(f, body.width).0,
                ],
                Modal::ConfirmTab(_, name) => vec![
                    format!("Close Tab {name}?"),
                    "Terminates its work. y confirms; n/Esc cancels.".into(),
                ],
                Modal::ConfirmSpace(_, name) => vec![
                    format!("Close Space {name}?"),
                    "Terminates ALL its Tabs. y confirms; n/Esc cancels.".into(),
                ],
                Modal::Picker {
                    spaces,
                    choices,
                    cursor,
                    filter,
                    ..
                } => {
                    let choices = filtered(choices, &filter.text);
                    let count = body.height.saturating_sub(2) as usize;
                    let start = cursor.saturating_sub(count.saturating_sub(1));
                    let mut lines = vec![
                        if *spaces {
                            "Spaces — Enter selects; Esc cancels".into()
                        } else {
                            "Goto — Enter selects; Left/Right changes group".into()
                        },
                        format!("Search: {}", filter.text),
                    ];
                    lines.extend(choices.iter().enumerate().skip(start).take(count).map(
                        |(i, c)| format!("{} {}", if i == *cursor { ">" } else { " " }, c.label),
                    ));
                    lines
                }
            };
        for (i, text) in lines.iter().take(body.height as usize).enumerate() {
            line(
                &mut out,
                Rect {
                    y: body.y + i as u16,
                    height: 1,
                    ..body
                },
                text,
                false,
            );
        }
        if let Modal::Rename(_, f) = modal {
            out.extend_from_slice(
                format!(
                    "\x1b[{};{}H\x1b[?25h",
                    body.y + 2,
                    body.x + field_window(f, body.width).1 as u16 + 1
                )
                .as_bytes(),
            );
        }
    } else {
        if let Some(frame) = view.and_then(|v| v.frame.as_ref()) {
            terminal(&mut out, frame, body, app.live());
        }
        if let Some(tab) = app.tab()
            && let TerminalState::Ended(reason) = &tab.terminal
            && view.is_none_or(|v| v.frame.is_none())
        {
            let reason = match reason {
                EndReason::ServerRestart => {
                    "Server restarted; previous processes and contents were not restored".into()
                }
                EndReason::Exited(code) => format!("Shell ended with status {code}"),
                EndReason::LaunchFailed(error) => format!("Shell launch failed: {error}"),
            };
            line(&mut out, Rect { height: 1, ..body }, &reason, false);
            line(
                &mut out,
                Rect {
                    y: body.y + 1,
                    height: 1,
                    ..body
                },
                "Ctrl+B Enter explicitly opens a NEW shell",
                false,
            );
        } else if view.is_none() {
            line(
                &mut out,
                Rect { height: 1, ..body },
                if app.selected.is_none() {
                    "Click + to open a shell"
                } else {
                    "Connecting to selected terminal…"
                },
                false,
            );
        }
    }
    let status = if app.prefix {
        "Prefix: ? help, q detach".into()
    } else if app.busy {
        "Control pending".into()
    } else {
        view.map_or(format!("Sheepdog {host}"), |v| format!("{:?}", v.status))
    };
    out.extend_from_slice(b"\x1b7");
    if layout.sidebar > 0 {
        line(
            &mut out,
            Rect {
                x: 0,
                y: app.height - 1,
                width: layout.sidebar - 1,
                height: 1,
            },
            &status,
            false,
        );
    }
    // Notices are explicit temporary overlays, not a permanent terminal footer.
    if !app.notice.is_empty() {
        line(
            &mut out,
            Rect {
                x: body.x,
                y: app.height - 1,
                width: body.width,
                height: 1,
            },
            &app.notice,
            false,
        );
    }
    out.extend_from_slice(b"\x1b8\x1b[?7h\x1b[?2026l");
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn header_text_cannot_inject_terminal_controls_or_bidi() {
        assert_eq!(clean("a\x1b\n\u{202e}界z", 3), "a界");
    }
    #[test]
    fn fields_scroll_to_keep_the_grapheme_cursor_visible() {
        let mut f = Field::default();
        f.insert("/long/path/界界");
        assert_eq!(field_window(&f, 6), ("/界界".into(), 5));
    }
    #[test]
    fn chrome_clips_whole_emoji_graphemes() {
        assert_eq!(clean("a👩‍👩‍👧‍👦b", 3), "a👩‍👩‍👧‍👦");
    }
    #[test]
    fn crop_keeps_overflow_at_its_new_flat_index() {
        let mut cells = vec![0; 3 * 4 * 12];
        cells[5 * 12 + 1] = 7 << 3;
        let mut f = FrameState::from_parts(3, 4, 0, 0, 0, "", cells);
        f.overflow_mut().insert(5, "overflow".into());
        let c = crop(&f, 2, 2);
        assert_eq!(c.cell_content(1, 1), "overflow");
        assert_eq!(c.cells().len(), 48);
    }
    #[test]
    fn crop_never_prints_half_a_wide_character_at_right_edge() {
        let mut cells = vec![0; 24];
        cells[1] = 3 << 3;
        cells[8..11].copy_from_slice("界".as_bytes());
        cells[13] = 2;
        let f = FrameState::from_parts(1, 2, 0, 0, 0, "", cells);
        assert_eq!(crop(&f, 1, 1).cell_content(0, 0), " ");
    }
}
