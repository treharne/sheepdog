use crate::app::{App, Modal, filtered};
use crate::input::Field;
use blit_remote::FrameState;
use sheepdog_client::ViewSnapshot;
use sheepdog_core::{EndReason, TerminalState};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn clean(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    let safe: String = text
        .chars()
        .filter(|c| {
            !c.is_control()
                && !('\u{202a}'..='\u{202e}').contains(c)
                && !('\u{2066}'..='\u{2069}').contains(c)
        })
        .collect();
    for grapheme in safe.graphemes(true) {
        let n = grapheme.width();
        if used + n > width {
            break;
        }
        used += n;
        out.push_str(grapheme);
    }
    out
}
fn field_window(field: &Field, width: u16) -> (String, usize) {
    let mut start = field.cursor;
    let mut x = 0;
    for (i, g) in field.text[..field.cursor].grapheme_indices(true).rev() {
        let n = clean(g, usize::MAX).width();
        if x + n > width.saturating_sub(1) as usize {
            break;
        }
        start = i;
        x += n;
    }
    (clean(&field.text[start..], width as usize), x)
}
fn line(out: &mut Vec<u8>, row: u16, text: &str, width: u16) {
    out.extend_from_slice(
        format!("\x1b[{};1H\x1b[0m{}", row + 1, clean(text, width as usize)).as_bytes(),
    );
}
pub fn crop(frame: &FrameState, rows: u16, cols: u16) -> FrameState {
    let rows = rows.min(frame.rows());
    let cols = cols.min(frame.cols());
    let size = 12;
    let mut cells = Vec::with_capacity(rows as usize * cols as usize * size);
    for row in 0..rows {
        let start = row as usize * frame.cols() as usize * size;
        cells.extend_from_slice(&frame.cells()[start..start + cols as usize * size]);
        if cols < frame.cols() && frame.cells()[start + cols as usize * size + 1] & 2 != 0 {
            let last = cells.len() - size;
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
            && clipped.cells()[(row * cols as usize + col) * size + 1] >> 3 & 7 == 7
        {
            clipped
                .overflow_mut()
                .insert(row * cols as usize + col, text.clone());
        }
    }
    clipped
}
pub fn render(app: &App, host: &str, view: Option<&ViewSnapshot>) -> Vec<u8> {
    let mut out = b"\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[0m\x1b[H\x1b[2J".to_vec();
    if !app.usable() {
        line(&mut out, 0, "Resize terminal to at least 12×5", app.width);
        out.extend_from_slice(b"\x1b[?7h\x1b[?2026l");
        return out;
    }
    let spaces = app
        .spaces
        .iter()
        .map(|s| {
            if Some(s.id) == app.space {
                format!("[{}]", s.name)
            } else {
                s.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");
    line(
        &mut out,
        0,
        &format!("Sheepdog {host}  {spaces}"),
        app.width,
    );
    let tabs = app
        .tabs
        .iter()
        .filter(|t| Some(t.space) == app.space)
        .enumerate()
        .map(|(i, t)| {
            let name = format!(
                "{}:{}{}",
                i + 1,
                t.label,
                if matches!(t.terminal, TerminalState::Ended(_)) {
                    " (ended)"
                } else {
                    ""
                }
            );
            if Some(t.id) == app.selected {
                format!("[{name}]")
            } else {
                name
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");
    line(&mut out, 1, &tabs, app.width);
    let geo = app.geometry();
    if let Some(modal) = &app.modal {
        let lines: Vec<String> =
            match modal {
                Modal::Help => [
                    "Prefix: Ctrl+B (release, then next key)",
                    "c new Tab   n/p next/previous   1–9 jump",
                    "w Spaces   g Goto (/ search, arrows/j/k, Enter)",
                    "N new Space   X close Tab   D close Space",
                    "Enter new shell in ended Tab   q detach",
                    "P arm ONE literal paste; normal paste awaits File Ingest",
                    "Esc closes dialogs; Ctrl+B Ctrl+B sends Ctrl+B",
                    "History/copy, rename, agent Attention are not built yet",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
                Modal::Name(f) => vec![
                    "New Space — name (Enter, Esc cancels)".into(),
                    field_window(f, app.width).0,
                ],
                Modal::Directory(name, f) => vec![
                    format!("New Space {name} — remote absolute directory"),
                    field_window(f, app.width).0,
                ],
                Modal::ConfirmTab(_, name) => vec![
                    format!("Close Tab {name}?"),
                    "This terminates its server-owned work. y confirms; n/Esc cancels.".into(),
                ],
                Modal::ConfirmSpace(_, name) => vec![
                    format!("Close Space {name}?"),
                    "This terminates ALL its Tabs. y confirms; n/Esc cancels.".into(),
                ],
                Modal::Picker {
                    spaces,
                    choices,
                    cursor,
                    filter,
                    ..
                } => {
                    let choices = filtered(choices, &filter.text);
                    let count = geo.rows.saturating_sub(2) as usize;
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
        for (i, text) in lines.iter().take(geo.rows as usize).enumerate() {
            line(&mut out, 2 + i as u16, text, app.width);
        }
        if let Modal::Name(f) | Modal::Directory(_, f) = modal {
            let x = field_window(f, app.width).1;
            out.extend_from_slice(format!("\x1b[4;{}H\x1b[?25h", x + 1).as_bytes());
        }
    } else {
        if let Some(frame) = view.and_then(|v| v.frame.as_ref()) {
            let clipped = crop(frame, geo.rows, geo.cols);
            out.extend_from_slice(b"\x1b[3;1H");
            out.extend_from_slice(&quosh_proto::frame_ansi(&clipped));
            if app.live()
                && frame.mode() & quosh_proto::MODE_CURSOR_VISIBLE != 0
                && frame.cursor_row() < geo.rows
                && frame.cursor_col() < geo.cols
            {
                out.extend_from_slice(
                    format!(
                        "\x1b[{};{}H\x1b[?25h",
                        frame.cursor_row() + 3,
                        frame.cursor_col() + 1
                    )
                    .as_bytes(),
                );
            }
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
            line(&mut out, 2, &reason, app.width);
            line(
                &mut out,
                3,
                "Ctrl+B Enter explicitly opens a NEW shell in the saved directory",
                app.width,
            );
        } else if view.is_none() {
            line(
                &mut out,
                2,
                if app.spaces.is_empty() {
                    "Ctrl+B N creates a Space. No shell starts automatically."
                } else if app.selected.is_none() {
                    "Empty Space — Ctrl+B c opens a shell"
                } else {
                    "Connecting to selected terminal…"
                },
                app.width,
            );
        }
    }
    let footer = if !app.notice.is_empty() {
        app.notice.clone()
    } else if app.prefix {
        "Prefix: ? help | q detach".into()
    } else if app.busy {
        "Control operation pending; Ctrl+B q still detaches".into()
    } else if matches!(app.modal, Some(Modal::Name(_) | Modal::Directory(..))) {
        "Field: Enter confirms | Esc cancels | arrows/Home/End edit".into()
    } else if app
        .tab()
        .is_some_and(|t| matches!(t.terminal, TerminalState::Ended(_)))
    {
        "Ended — Ctrl+B Enter opens a NEW shell | Ctrl+B q detaches".into()
    } else {
        format!(
            "Ctrl+B ? help | q detach{}",
            view.map_or(String::new(), |v| format!(" | {:?}", v.status))
        )
    };
    // Preserve the terminal cursor while painting local chrome.
    out.extend_from_slice(b"\x1b7");
    line(&mut out, app.height - 1, &footer, app.width);
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
        let (text, x) = field_window(&f, 6);
        assert_eq!(text, "/界界");
        assert_eq!(x, 5);
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
        assert_eq!(c.cells().len(), 2 * 2 * 12);
    }
    #[test]
    fn crop_never_prints_half_a_wide_character_at_right_edge() {
        let mut cells = vec![0; 2 * 12];
        cells[1] = 3 << 3;
        cells[8..11].copy_from_slice("界".as_bytes());
        cells[13] = 2;
        let f = FrameState::from_parts(1, 2, 0, 0, 0, "", cells);
        assert_eq!(crop(&f, 1, 1).cell_content(0, 0), " ");
    }
}
