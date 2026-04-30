use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn encode(key: &KeyEvent) -> Option<Vec<u8>> {
    let mods = key.modifiers;

    match key.code {
        KeyCode::Char(c) => {
            if mods.contains(KeyModifiers::CONTROL) {
                let upper = c.to_ascii_uppercase();
                if upper.is_ascii_uppercase() {
                    return Some(vec![upper as u8 - b'A' + 1]);
                }
                return None;
            }
            if mods.contains(KeyModifiers::ALT) {
                let mut bytes = vec![0x1b];
                let mut buf = [0u8; 4];
                bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                return Some(bytes);
            }
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            Some(s.as_bytes().to_vec())
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::Insert => Some(b"\x1b[2~".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::F(1) => Some(b"\x1bOP".to_vec()),
        KeyCode::F(2) => Some(b"\x1bOQ".to_vec()),
        KeyCode::F(3) => Some(b"\x1bOR".to_vec()),
        KeyCode::F(4) => Some(b"\x1bOS".to_vec()),
        KeyCode::F(5) => Some(b"\x1b[15~".to_vec()),
        KeyCode::F(6) => Some(b"\x1b[17~".to_vec()),
        KeyCode::F(7) => Some(b"\x1b[18~".to_vec()),
        KeyCode::F(8) => Some(b"\x1b[19~".to_vec()),
        KeyCode::F(9) => Some(b"\x1b[20~".to_vec()),
        KeyCode::F(10) => Some(b"\x1b[21~".to_vec()),
        KeyCode::F(11) => Some(b"\x1b[23~".to_vec()),
        KeyCode::F(12) => Some(b"\x1b[24~".to_vec()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn regular_char() {
        assert_eq!(encode(&key(KeyCode::Char('a'))), Some(b"a".to_vec()));
    }

    #[test]
    fn utf8_char() {
        let bytes = encode(&key(KeyCode::Char('\u{00e9}'))).unwrap();
        assert_eq!(bytes, "\u{00e9}".as_bytes());
    }

    #[test]
    fn ctrl_a() {
        assert_eq!(encode(&ctrl('a')), Some(vec![0x01]));
    }

    #[test]
    fn ctrl_c() {
        assert_eq!(encode(&ctrl('c')), Some(vec![0x03]));
    }

    #[test]
    fn ctrl_z() {
        assert_eq!(encode(&ctrl('z')), Some(vec![0x1a]));
    }

    #[test]
    fn alt_char() {
        assert_eq!(encode(&alt('x')), Some(vec![0x1b, b'x']));
    }

    #[test]
    fn enter() {
        assert_eq!(encode(&key(KeyCode::Enter)), Some(vec![b'\r']));
    }

    #[test]
    fn backspace() {
        assert_eq!(encode(&key(KeyCode::Backspace)), Some(vec![0x7f]));
    }

    #[test]
    fn escape() {
        assert_eq!(encode(&key(KeyCode::Esc)), Some(vec![0x1b]));
    }

    #[test]
    fn arrows() {
        assert_eq!(encode(&key(KeyCode::Up)), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode(&key(KeyCode::Down)), Some(b"\x1b[B".to_vec()));
        assert_eq!(encode(&key(KeyCode::Right)), Some(b"\x1b[C".to_vec()));
        assert_eq!(encode(&key(KeyCode::Left)), Some(b"\x1b[D".to_vec()));
    }

    #[test]
    fn function_keys() {
        assert_eq!(encode(&key(KeyCode::F(1))), Some(b"\x1bOP".to_vec()));
        assert_eq!(encode(&key(KeyCode::F(12))), Some(b"\x1b[24~".to_vec()));
    }

    #[test]
    fn nav_keys() {
        assert_eq!(encode(&key(KeyCode::Home)), Some(b"\x1b[H".to_vec()));
        assert_eq!(encode(&key(KeyCode::End)), Some(b"\x1b[F".to_vec()));
        assert_eq!(encode(&key(KeyCode::PageUp)), Some(b"\x1b[5~".to_vec()));
        assert_eq!(encode(&key(KeyCode::PageDown)), Some(b"\x1b[6~".to_vec()));
        assert_eq!(encode(&key(KeyCode::Insert)), Some(b"\x1b[2~".to_vec()));
        assert_eq!(encode(&key(KeyCode::Delete)), Some(b"\x1b[3~".to_vec()));
    }
}
