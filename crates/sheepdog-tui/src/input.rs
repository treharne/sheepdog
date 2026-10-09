use crossterm::event::{KeyCode, KeyEvent, KeyModifiers as M};
use unicode_segmentation::UnicodeSegmentation;

pub fn ctrl(key: KeyEvent, letter: char) -> bool {
    key.modifiers.contains(M::CONTROL)
        && !key.modifiers.contains(M::ALT)
        && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&letter))
}
pub fn remote(key: KeyEvent) -> Option<Vec<u8>> {
    let modifier = 1
        + u8::from(key.modifiers.contains(M::SHIFT))
        + 2 * u8::from(key.modifiers.contains(M::ALT))
        + 4 * u8::from(key.modifiers.contains(M::CONTROL));
    let cursor = |c: char| {
        if modifier == 1 {
            format!("\x1b[{c}")
        } else {
            format!("\x1b[1;{modifier}{c}")
        }
    };
    let tilde = |n: u8| {
        if modifier == 1 {
            format!("\x1b[{n}~")
        } else {
            format!("\x1b[{n};{modifier}~")
        }
    };
    let mut bytes = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(M::CONTROL) => {
            let n = match c {
                ' ' | '@' => 0,
                '?' => 127,
                c if c.is_ascii_alphabetic() || ('['..='_').contains(&c) => {
                    c.to_ascii_uppercase() as u8 & 31
                }
                _ => return None,
            };
            vec![n]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![13],
        KeyCode::Tab => vec![9],
        KeyCode::Backspace => vec![if key.modifiers.contains(M::CONTROL) {
            8
        } else {
            127
        }],
        KeyCode::Esc => vec![27],
        KeyCode::BackTab => return Some(b"\x1b[Z".to_vec()),
        KeyCode::Up => return Some(cursor('A').into_bytes()),
        KeyCode::Down => return Some(cursor('B').into_bytes()),
        KeyCode::Right => return Some(cursor('C').into_bytes()),
        KeyCode::Left => return Some(cursor('D').into_bytes()),
        KeyCode::Home => return Some(cursor('H').into_bytes()),
        KeyCode::End => return Some(cursor('F').into_bytes()),
        KeyCode::Insert => return Some(tilde(2).into_bytes()),
        KeyCode::Delete => return Some(tilde(3).into_bytes()),
        KeyCode::PageUp => return Some(tilde(5).into_bytes()),
        KeyCode::PageDown => return Some(tilde(6).into_bytes()),
        KeyCode::F(n @ 1..=4) => {
            return Some(
                if modifier == 1 {
                    format!("\x1bO{}", (b'P' + n - 1) as char)
                } else {
                    cursor((b'P' + n - 1) as char)
                }
                .into_bytes(),
            );
        }
        KeyCode::F(n @ 5..=12) => {
            return Some(tilde([15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5]).into_bytes());
        }
        _ => return None,
    };
    if key.modifiers.contains(M::ALT) {
        bytes.insert(0, 27);
    }
    Some(bytes)
}

#[derive(Clone, Default)]
pub struct Field {
    pub text: String,
    pub cursor: usize,
    cut: String,
}
impl Field {
    fn prev(&self) -> usize {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }
    fn next(&self) -> usize {
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map_or(self.cursor, |g| self.cursor + g.len())
    }
    pub fn insert(&mut self, value: &str) {
        if self.text.len().saturating_add(value.len()) > 4096 || value.chars().any(char::is_control)
        {
            return;
        }
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        // Appending a combining mark may merge with the adjacent grapheme.
        while self.cursor < self.text.len()
            && !self
                .text
                .grapheme_indices(true)
                .any(|(i, _)| i == self.cursor)
        {
            self.cursor += self.text[self.cursor..].chars().next().unwrap().len_utf8();
        }
    }
    fn remove(&mut self, a: usize, b: usize, cut: bool) {
        if cut {
            self.cut = self.text[a..b].to_owned();
        }
        self.text.replace_range(a..b, "");
        self.cursor = a;
    }
    fn left_word(&self) -> usize {
        self.text[..self.cursor]
            .unicode_word_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }
    fn right_word(&self) -> usize {
        self.text[self.cursor..]
            .unicode_word_indices()
            .next()
            .map_or(self.text.len(), |(i, w)| self.cursor + i + w.len())
    }
    pub fn key(&mut self, key: KeyEvent) {
        if ctrl(key, 'a') || key.code == KeyCode::Home {
            self.cursor = 0;
        } else if ctrl(key, 'e') || key.code == KeyCode::End {
            self.cursor = self.text.len();
        } else if ctrl(key, 'u') {
            self.remove(0, self.cursor, true);
        } else if ctrl(key, 'k') {
            self.remove(self.cursor, self.text.len(), true);
        } else if ctrl(key, 'w') {
            self.remove(self.left_word(), self.cursor, true);
        } else if ctrl(key, 'y') {
            self.insert(&self.cut.clone());
        } else if key.modifiers.contains(M::ALT) {
            match key.code {
                KeyCode::Char('b') => self.cursor = self.left_word(),
                KeyCode::Char('f') => self.cursor = self.right_word(),
                KeyCode::Char('d') => self.remove(self.cursor, self.right_word(), true),
                KeyCode::Backspace => self.remove(self.left_word(), self.cursor, true),
                _ => {}
            }
        } else if key.code == KeyCode::Left || ctrl(key, 'b') {
            self.cursor = self.prev();
        } else if key.code == KeyCode::Right || ctrl(key, 'f') {
            self.cursor = self.next();
        } else if key.code == KeyCode::Backspace || ctrl(key, 'h') {
            self.remove(self.prev(), self.cursor, false);
        } else if key.code == KeyCode::Delete || ctrl(key, 'd') {
            self.remove(self.cursor, self.next(), false);
        } else if let KeyCode::Char(c) = key.code
            && !key.modifiers.contains(M::CONTROL)
        {
            self.insert(&c.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_preserve_control_alt_and_unicode() {
        assert_eq!(
            remote(KeyEvent::new(KeyCode::Char('c'), M::CONTROL)).unwrap(),
            [3]
        );
        assert_eq!(
            remote(KeyEvent::new(KeyCode::Char('λ'), M::NONE)).unwrap(),
            "λ".as_bytes()
        );
        assert_eq!(
            remote(KeyEvent::new(KeyCode::Left, M::CONTROL | M::ALT)).unwrap(),
            b"\x1b[1;7D"
        );
        assert_eq!(
            remote(KeyEvent::new(KeyCode::Char('x'), M::ALT)).unwrap(),
            b"\x1bx"
        );
    }
    #[test]
    fn fields_move_and_delete_whole_graphemes() {
        let mut f = Field::default();
        f.insert("a👩‍👩‍👧‍👦e\u{301}");
        f.key(KeyEvent::new(KeyCode::Backspace, M::NONE));
        assert_eq!(f.text, "a👩‍👩‍👧‍👦");
        f.key(KeyEvent::new(KeyCode::Left, M::NONE));
        assert_eq!(f.cursor, 1);
        f.key(KeyEvent::new(KeyCode::Delete, M::NONE));
        assert_eq!(f.text, "a");
        f.key(KeyEvent::new(KeyCode::Char('u'), M::CONTROL));
        assert!(f.text.is_empty());
        f.key(KeyEvent::new(KeyCode::Char('y'), M::CONTROL));
        assert_eq!(f.text, "a");
    }
}
