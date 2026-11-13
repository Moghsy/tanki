//! A single-line text input (type-in-the-answer, prompts, the palette).
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    /// Byte offset of the cursor, always on a char boundary.
    cursor: usize,
}

impl TextInput {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self { text, cursor }
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn insert_str(&mut self, s: &str) {
        // Pasted newlines would end a single-line input; keep them as spaces.
        let clean: String = s
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .filter(|c| !c.is_control())
            .collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }
    /// Display columns before the cursor.
    pub fn cursor_column(&self) -> usize {
        self.text[..self.cursor].width()
    }
    fn prev_boundary(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }
    fn next_boundary(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }
    /// Applies an editing key. Returns false when the key is not an editing
    /// key (the caller handles it).
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('u') if ctrl => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            KeyCode::Char('w') if ctrl => {
                let before = &self.text[..self.cursor];
                let trimmed = before.trim_end();
                let start = trimmed
                    .char_indices()
                    .rev()
                    .find(|(_, c)| c.is_whitespace())
                    .map_or(0, |(i, c)| i + c.len_utf8());
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                let mut buf = [0; 4];
                self.insert_str(c.encode_utf8(&mut buf));
            }
            KeyCode::Backspace => {
                let start = self.prev_boundary();
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Delete => {
                let end = self.next_boundary();
                self.text.drain(self.cursor..end);
            }
            KeyCode::Left => self.cursor = self.prev_boundary(),
            KeyCode::Right => self.cursor = self.next_boundary(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn edits_at_the_cursor_with_wide_chars() {
        let mut input = TextInput::default();
        for c in "日本go".chars() {
            assert!(input.handle_key(&key(KeyCode::Char(c))));
        }
        assert_eq!(input.cursor_column(), 6);
        input.handle_key(&key(KeyCode::Left));
        input.handle_key(&key(KeyCode::Left));
        input.handle_key(&key(KeyCode::Backspace));
        assert_eq!(input.text(), "日go");
        assert_eq!(input.cursor_column(), 2);
        input.handle_key(&key(KeyCode::Delete));
        assert_eq!(input.text(), "日o");
        input.insert_str("a\nb");
        assert_eq!(input.text(), "日a bo");
        assert!(input.handle_key(&KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)));
        assert_eq!(input.text(), "日a o");
        assert!(!input.handle_key(&key(KeyCode::Enter)));
        assert!(!input.handle_key(&KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL
        )));
    }
}
