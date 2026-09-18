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
                let bytes = c.encode_utf8(&mut buf).as_bytes();
                if key.modifiers.contains(KeyModifiers::ALT) {
                    // Meta prefix: ESC + the character's bytes.
                    let mut v = Vec::with_capacity(bytes.len() + 1);
                    v.push(0x1b);
                    v.extend_from_slice(bytes);
                    v
                } else {
                    bytes.to_vec()
                }
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => arrow(b'A', key.modifiers),
        KeyCode::Down => arrow(b'B', key.modifiers),
        KeyCode::Right => arrow(b'C', key.modifiers),
        KeyCode::Left => arrow(b'D', key.modifiers),
        KeyCode::Home => home_end(b'H', key.modifiers),
        KeyCode::End => home_end(b'F', key.modifiers),
        KeyCode::Insert => tilde_key(2, key.modifiers),
        KeyCode::Delete => tilde_key(3, key.modifiers),
        KeyCode::PageUp => tilde_key(5, key.modifiers),
        KeyCode::PageDown => tilde_key(6, key.modifiers),
        KeyCode::F(n) => f_key(n),
        _ => Vec::new(),
    }
}

/// xterm modifier parameter: 1 + (shift 1) + (alt 2) + (ctrl 4).
fn modifier_param(mods: KeyModifiers) -> u8 {
    let mut m = 1u8;
    if mods.contains(KeyModifiers::SHIFT) {
        m += 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        m += 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        m += 4;
    }
    m
}

fn arrow(final_byte: u8, mods: KeyModifiers) -> Vec<u8> {
    if mods.is_empty() {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[1;{}{}", modifier_param(mods), final_byte as char).into_bytes()
    }
}

fn home_end(final_byte: u8, mods: KeyModifiers) -> Vec<u8> {
    if mods.is_empty() {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[1;{}{}", modifier_param(mods), final_byte as char).into_bytes()
    }
}

fn tilde_key(n: u8, mods: KeyModifiers) -> Vec<u8> {
    if mods.is_empty() {
        format!("\x1b[{n}~").into_bytes()
    } else {
        format!("\x1b[{n};{}~", modifier_param(mods)).into_bytes()
    }
}

fn f_key(n: u8) -> Vec<u8> {
    match n {
        1..=4 => vec![0x1b, b'O', b'P' + (n - 1)],
        5..=12 => format!("\x1b[{}~", [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize]).into_bytes(),
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
    fn alt_char_becomes_meta_prefixed_bytes() {
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Char('b'), KeyModifiers::ALT)),
            b"\x1bb".to_vec()
        );
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
    fn shift_tab_becomes_backtab_csi_z() {
        assert_eq!(
            key_event_to_bytes(key(KeyCode::BackTab, KeyModifiers::SHIFT)),
            b"\x1b[Z".to_vec()
        );
    }

    #[test]
    fn arrow_keys_become_ansi_escape_sequences() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Up, KeyModifiers::NONE)), b"\x1b[A".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Down, KeyModifiers::NONE)), b"\x1b[B".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Right, KeyModifiers::NONE)), b"\x1b[C".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Left, KeyModifiers::NONE)), b"\x1b[D".to_vec());
    }

    #[test]
    fn modified_arrows_carry_the_modifier_parameter() {
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Right, KeyModifiers::CONTROL)),
            b"\x1b[1;5C".to_vec()
        );
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Left, KeyModifiers::SHIFT | KeyModifiers::ALT)),
            b"\x1b[1;4D".to_vec()
        );
    }

    #[test]
    fn navigation_keys_become_xterm_sequences() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Home, KeyModifiers::NONE)), b"\x1b[H".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::End, KeyModifiers::NONE)), b"\x1b[F".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Insert, KeyModifiers::NONE)), b"\x1b[2~".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Delete, KeyModifiers::NONE)), b"\x1b[3~".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::PageUp, KeyModifiers::NONE)), b"\x1b[5~".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::PageDown, KeyModifiers::NONE)), b"\x1b[6~".to_vec());
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Home, KeyModifiers::CONTROL)),
            b"\x1b[1;5H".to_vec()
        );
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Delete, KeyModifiers::SHIFT)),
            b"\x1b[3;2~".to_vec()
        );
    }

    #[test]
    fn f_keys_become_xterm_sequences() {
        assert_eq!(key_event_to_bytes(key(KeyCode::F(1), KeyModifiers::NONE)), b"\x1bOP".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::F(4), KeyModifiers::NONE)), b"\x1bOS".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::F(5), KeyModifiers::NONE)), b"\x1b[15~".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::F(12), KeyModifiers::NONE)), b"\x1b[24~".to_vec());
    }
}
