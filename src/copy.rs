//! Copy mode (leader+[) — keyboard selection over a pane's full grid
//! (scrollback + visible), yanked to the system clipboard via OSC52.
//! OSC52 works through tmux/SSH on kitty, wezterm, alacritty, foot —
//! no X11/Wayland clipboard dep.

/// Copy-mode state — lives on App; the pane is addressed by id so a
/// closed pane just invalidates the mode.
pub struct CopyState {
    pub pane_id: crate::pane::PaneId,
    /// Cursor in absolute grid coords (0 = oldest scrollback line).
    pub cursor: (usize, usize),
    /// Selection anchor — `v` toggles it; yank uses anchor..=cursor.
    pub anchor: Option<(usize, usize)>,
}

/// Extract the selected text from full-grid lines (search::grid_lines
/// output). Linear selection: partial first row, whole middle rows,
/// partial last row — trailing blanks trimmed.
pub fn extract(lines: &[String], a: (usize, usize), b: (usize, usize)) -> String {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    let (r0, c0) = a;
    let (r1, c1) = b;
    let row_chars = |r: usize| -> Vec<char> {
        lines.get(r).map(|l| l.chars().collect()).unwrap_or_default()
    };
    let mut out = String::new();
    for r in r0..=r1 {
        let chars = row_chars(r);
        let from = if r == r0 { c0.min(chars.len()) } else { 0 };
        let to = if r == r1 { (c1 + 1).min(chars.len()) } else { chars.len() };
        if from < to {
            if !out.is_empty() {
                out.push('\n');
            }
            out.extend(chars[from..to].iter());
        } else if r != r0 && !out.is_empty() {
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// Single grid line under the cursor, trimmed — the no-selection yank.
pub fn extract_line(lines: &[String], row: usize) -> String {
    lines.get(row).map(|l| l.trim_end().to_string()).unwrap_or_default()
}

fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .fold(0u32, |acc, &b| (acc << 8) | b as u32)
            << (8 * (3 - chunk.len()));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// OSC52 clipboard write — the escape goes to the real terminal
/// (stdout), never to a pane PTY. `c` targets the system clipboard.
pub fn osc52_bytes(text: &str) -> Vec<u8> {
    format!("\x1b]52;c;{}\x07", b64(text.as_bytes())).into_bytes()
}

/// Emit the yank to the host terminal's clipboard. Silent no-op on
/// terminals without OSC52 (the escape is simply ignored).
pub fn yank(text: &str) {
    use std::io::Write;
    let _ = std::io::stdout().write_all(&osc52_bytes(text));
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extract_single_row_range() {
        let l = lines(&["hello world", "other"]);
        assert_eq!(extract(&l, (0, 0), (0, 4)), "hello");
    }

    #[test]
    fn extract_multi_row_and_reverse_order() {
        let l = lines(&["aaa", "bbb", "ccc"]);
        // Anchor after cursor → normalized automatically.
        assert_eq!(extract(&l, (2, 2), (0, 1)), "aa\nbbb\nccc");
    }

    #[test]
    fn extract_trims_trailing_blanks() {
        let l = lines(&["a", "", "b"]);
        assert_eq!(extract(&l, (0, 0), (2, 0)), "a\n\nb");
    }

    #[test]
    fn extract_line_without_selection() {
        // Leading whitespace survives — it's code indentation.
        let l = lines(&["  padded  "]);
        assert_eq!(extract_line(&l, 0), "  padded");
    }

    #[test]
    fn b64_matches_known_vectors() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"hello world"), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn osc52_wraps_base64() {
        let b = osc52_bytes("hi");
        let s = String::from_utf8(b).unwrap();
        assert_eq!(s, "\x1b]52;c;aGk=\x07");
    }
}
