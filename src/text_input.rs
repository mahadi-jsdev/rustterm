use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum EditResult {
    Editing,
    Submit(String),
    Cancel,
}

pub struct LineEdit {
    buf: String,
    error: Option<String>,
    /// Completion hints the caller maintains (e.g. directory names for the
    /// add-project prompt); rendered by the status bar. Unused for plain
    /// text prompts like rename.
    pub suggestions: Vec<String>,
}

impl LineEdit {
    pub fn new() -> LineEdit {
        LineEdit { buf: String::new(), error: None, suggestions: Vec::new() }
    }

    pub fn from_str(s: &str) -> LineEdit {
        LineEdit { buf: s.to_string(), error: None, suggestions: Vec::new() }
    }

    pub fn as_str(&self) -> &str {
        &self.buf
    }

    pub fn set_text(&mut self, s: String) {
        self.buf = s;
    }

    /// Appends pasted text; newlines collapse to spaces so a multi-line
    /// clipboard can't smuggle a fake Enter into a one-line field.
    pub fn insert_str(&mut self, s: &str) {
        self.error = None;
        self.buf
            .push_str(&s.replace("\r\n", " ").replace(['\r', '\n'], " "));
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EditResult {
        self.error = None;
        match key.code {
            KeyCode::Enter => EditResult::Submit(std::mem::take(&mut self.buf)),
            KeyCode::Esc => EditResult::Cancel,
            KeyCode::Backspace => {
                self.buf.pop();
                EditResult::Editing
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.buf.clear();
                EditResult::Editing
            }
            KeyCode::Char(c) => {
                self.buf.push(c);
                EditResult::Editing
            }
            _ => EditResult::Editing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn chars_append_and_backspace_pops() {
        let mut e = LineEdit::new();
        for c in ['a', 'b', 'c'] {
            e.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(e.as_str(), "abc");
        e.handle_key(key(KeyCode::Backspace));
        assert_eq!(e.as_str(), "ab");
    }

    #[test]
    fn enter_submits_and_esc_cancels() {
        let mut e = LineEdit::from_str("/tmp/foo");
        match e.handle_key(key(KeyCode::Enter)) {
            EditResult::Submit(s) => assert_eq!(s, "/tmp/foo"),
            _ => panic!("expected Submit"),
        }
        let mut e = LineEdit::new();
        assert!(matches!(e.handle_key(key(KeyCode::Esc)), EditResult::Cancel));
    }

    #[test]
    fn ctrl_u_clears() {
        let mut e = LineEdit::from_str("junk");
        e.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(e.as_str(), "");
    }

    #[test]
    fn typing_clears_error() {
        let mut e = LineEdit::new();
        e.set_error("bad".into());
        assert_eq!(e.error(), Some("bad"));
        e.handle_key(key(KeyCode::Char('x')));
        assert_eq!(e.error(), None);
    }
}
