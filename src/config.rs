//! User config — ~/.config/rustterm/config.toml (or $RUSTTERM_CONFIG).
//! Missing file = defaults; a parse error falls back to defaults and
//! reports the reason so the status bar can flash it.

use ratatui::style::Color;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Leader key — always Ctrl+<char>. TOML: `leader = "ctrl+b"`.
    pub leader_char: char,
    /// Editor command for the finder — beats $VISUAL/$EDITOR/nvim.
    pub editor: Option<String>,
    pub sidebar_width: u16,
    /// Wheel scroll step, lines per notch.
    pub scroll_lines: usize,
    /// vt100 scrollback lines per pane.
    pub scrollback: usize,
    /// Popup floats span this % of the screen in both dimensions.
    pub float_pct: u16,
    /// Focused-pane border accent (name or #rrggbb).
    pub accent: Color,
}

#[derive(Deserialize)]
struct Raw {
    leader: Option<String>,
    editor: Option<String>,
    sidebar_width: Option<u16>,
    scroll_lines: Option<usize>,
    scrollback: Option<usize>,
    float_pct: Option<u16>,
    accent: Option<String>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            leader_char: 'a',
            editor: None,
            sidebar_width: 24,
            scroll_lines: 3,
            scrollback: 10_000,
            float_pct: 90,
            accent: Color::Cyan,
        }
    }
}

pub fn path() -> PathBuf {
    if let Ok(p) = std::env::var("RUSTTERM_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });
    base.join("rustterm/config.toml")
}

/// Load config; (defaults, None) when absent. Parse problems return
/// defaults + a human-readable reason for the startup flash.
pub fn load() -> (Config, Option<String>) {
    let path = path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return (Config::default(), None),
    };
    match toml::from_str::<Raw>(&text) {
        Ok(raw) => (apply(raw), None),
        Err(e) => (Config::default(), Some(format!("config {}: {e}", path.display()))),
    }
}

fn apply(raw: Raw) -> Config {
    let mut c = Config::default();
    if let Some(l) = raw.leader {
        c.leader_char = parse_leader(&l).unwrap_or(c.leader_char);
    }
    if let Some(e) = raw.editor {
        if !e.trim().is_empty() {
            c.editor = Some(e.trim().to_string());
        }
    }
    if let Some(w) = raw.sidebar_width {
        c.sidebar_width = w.clamp(10, 80);
    }
    if let Some(n) = raw.scroll_lines {
        c.scroll_lines = n.clamp(1, 50);
    }
    if let Some(n) = raw.scrollback {
        c.scrollback = n.clamp(100, 1_000_000);
    }
    if let Some(p) = raw.float_pct {
        c.float_pct = p.clamp(30, 100);
    }
    if let Some(a) = raw.accent {
        c.accent = parse_color(&a).unwrap_or(c.accent);
    }
    c
}

/// "ctrl+x" → 'x'. Anything else falls back to the caller's default.
fn parse_leader(s: &str) -> Option<char> {
    let ch = s.strip_prefix("ctrl+")?.chars().next()?;
    Some(ch.to_ascii_lowercase())
}

fn parse_color(s: &str) -> Option<Color> {
    let named = match s.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "white" => Color::White,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        hex if hex.starts_with('#') && hex.len() == 7 => {
            let v = u32::from_str_radix(&hex[1..], 16).ok()?;
            return Some(Color::Rgb(
                (v >> 16) as u8,
                (v >> 8) as u8,
                v as u8,
            ));
        }
        _ => return None,
    };
    Some(named)
}

/// The leader check shared by Normal/Sidebar/Copy input paths.
impl Config {
    pub fn is_leader(&self, key: &crossterm::event::KeyEvent) -> bool {
        key.code == crossterm::event::KeyCode::Char(self.leader_char)
            && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_file_missing() {
        std::env::set_var("RUSTTERM_CONFIG", "/nonexistent/rustterm-test.toml");
        let (c, err) = load();
        assert_eq!(c, Config::default());
        assert!(err.is_none());
    }

    #[test]
    fn parses_all_fields() {
        let raw: Raw = toml::from_str(
            r##"leader = "ctrl+b"
               editor = "hx"
               sidebar_width = 30
               scroll_lines = 5
               scrollback = 50000
               float_pct = 80
               accent = "#ff8800""##,
        )
        .unwrap();
        let c = apply(raw);
        assert_eq!(c.leader_char, 'b');
        assert_eq!(c.editor.as_deref(), Some("hx"));
        assert_eq!(c.sidebar_width, 30);
        assert_eq!(c.scroll_lines, 5);
        assert_eq!(c.scrollback, 50000);
        assert_eq!(c.float_pct, 80);
        assert_eq!(c.accent, Color::Rgb(0xff, 0x88, 0x00));
    }

    #[test]
    fn bad_leader_and_color_fall_back() {
        let c = apply(Raw {
            leader: Some("nope".into()), editor: None, sidebar_width: None,
            scroll_lines: None, scrollback: None, float_pct: None,
            accent: Some("banana".into()),
        });
        assert_eq!(c.leader_char, 'a');
        assert_eq!(c.accent, Color::Cyan);
    }

    #[test]
    fn clamps_out_of_range_values() {
        let c = apply(Raw {
            leader: None, editor: None, sidebar_width: Some(200),
            scroll_lines: Some(0), scrollback: Some(5), float_pct: Some(5), accent: None,
        });
        assert_eq!(c.sidebar_width, 80);
        assert_eq!(c.scroll_lines, 1);
        assert_eq!(c.scrollback, 100);
        assert_eq!(c.float_pct, 30);
    }

    #[test]
    fn named_colors_parse() {
        assert_eq!(parse_color("magenta"), Some(Color::Magenta));
        assert_eq!(parse_color("#123456"), Some(Color::Rgb(0x12, 0x34, 0x56)));
        assert_eq!(parse_color("zzz"), None);
    }
}
