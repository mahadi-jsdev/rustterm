use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn key_event_to_bytes(key: KeyEvent) -> Vec<u8> {
    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let lower = c.to_ascii_lowercase();
                if lower.is_ascii_alphabetic() {
                    vec![(lower as u8) - b'a' + 1]
                } else {
                    vec![c as u8]
                }
            } else {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf).as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn plain_char_passes_through_as_utf8() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Char('a'), KeyModifiers::NONE)), b"a".to_vec());
    }

    #[test]
    fn ctrl_c_becomes_control_byte_3() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), vec![3u8]);
    }

    #[test]
    fn enter_becomes_carriage_return() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Enter, KeyModifiers::NONE)), vec![b'\r']);
    }

    #[test]
    fn backspace_becomes_del_byte() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Backspace, KeyModifiers::NONE)), vec![0x7f]);
    }

    #[test]
    fn arrow_keys_become_ansi_escape_sequences() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Up, KeyModifiers::NONE)), b"\x1b[A".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Down, KeyModifiers::NONE)), b"\x1b[B".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Right, KeyModifiers::NONE)), b"\x1b[C".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Left, KeyModifiers::NONE)), b"\x1b[D".to_vec());
    }

    #[test]
    fn unmapped_key_yields_empty_bytes() {
        assert_eq!(key_event_to_bytes(key(KeyCode::F(5), KeyModifiers::NONE)), Vec::<u8>::new());
    }
}
