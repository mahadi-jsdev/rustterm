use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const POLL: Duration = Duration::from_millis(400);
pub const BUSY_MIN: Duration = Duration::from_secs(10);
pub const QUIET: Duration = Duration::from_secs(8);
pub const WAITING_QUIET: Duration = Duration::from_millis(1500);
pub const RUNNING_WINDOW: Duration = Duration::from_millis(1500);
pub const OUTPUT_TAIL_MAX: usize = 2000;

#[derive(Debug, PartialEq)]
pub enum WatchEvent {
    Done { command: String },
    Waiting(bool),
    Command(String),
}

/// Escape-sequence state for input scanning — without it, arrow keys
/// (`ESC [ C`) and friends push their printable bytes into `input_buf`
/// and leak into notification bodies ("n[C").
#[derive(Debug, PartialEq, Clone, Copy)]
enum InputEsc {
    None,
    Esc,
    Csi,
    Osc,
    Ss3,
}

pub struct Watcher {
    input_buf: Vec<u8>,
    esc: InputEsc,
    last_command: String,
    output_tail: String,
    last_hash: u64,
    last_change: Option<Instant>,
    busy_since: Option<Instant>,
    waiting: bool,
    running: bool,
}

impl Watcher {
    pub fn new() -> Watcher {
        Watcher {
            input_buf: Vec::new(),
            esc: InputEsc::None,
            last_command: String::new(),
            output_tail: String::new(),
            last_hash: 0,
            last_change: None,
            busy_since: None,
            waiting: false,
            running: false,
        }
    }

    /// Feed the same bytes that were written to the pane's PTY. Returns a
    /// `Command` event when Enter commits a non-empty line, and
    /// `Waiting(false)` if the keystroke cleared a waiting state.
    pub fn on_input(&mut self, bytes: &[u8]) -> Vec<WatchEvent> {
        let mut events = Vec::new();
        if !bytes.is_empty() && self.waiting {
            self.waiting = false;
            events.push(WatchEvent::Waiting(false));
        }
        for &b in bytes {
            match self.esc {
                InputEsc::Csi => {
                    // CSI = params (0x30-0x3f) + intermediates (0x20-0x2f)
                    // + one final byte (0x40-0x7e).
                    if (0x40..=0x7e).contains(&b) {
                        self.esc = InputEsc::None;
                    }
                    continue;
                }
                InputEsc::Osc => {
                    // OSC ends on BEL or ST (ESC \).
                    self.esc = match b {
                        0x07 => InputEsc::None,
                        0x1b => InputEsc::Esc,
                        _ => InputEsc::Osc,
                    };
                    continue;
                }
                InputEsc::Ss3 => {
                    self.esc = InputEsc::None;
                    continue;
                }
                InputEsc::Esc => {
                    self.esc = InputEsc::None;
                    match b {
                        b'[' => {
                            self.esc = InputEsc::Csi;
                            continue;
                        }
                        b']' => {
                            self.esc = InputEsc::Osc;
                            continue;
                        }
                        b'O' => {
                            self.esc = InputEsc::Ss3;
                            continue;
                        }
                        0x1b => {
                            self.esc = InputEsc::Esc;
                            continue;
                        }
                        b'\\' => continue, // ST closing an OSC
                        // Control keys after a bare ESC still act
                        // (Esc-then-Enter must commit); Alt+printable
                        // sequences are swallowed as non-text.
                        b'\r' | 0x03 | 0x7f | 0x08 => {}
                        _ => continue,
                    }
                }
                InputEsc::None => {}
            }
            match b {
                b'\r' => {
                    let line = String::from_utf8_lossy(&self.input_buf).trim().to_string();
                    self.input_buf.clear();
                    if !line.is_empty() {
                        self.last_command = line.clone();
                        events.push(WatchEvent::Command(line));
                    }
                }
                0x7f | 0x08 => {
                    self.input_buf.pop();
                }
                0x03 => self.input_buf.clear(),
                0x1b => self.esc = InputEsc::Esc,
                // Pasted newlines arrive as \n — keep them as a space so a
                // multiline paste reads as one command line, not "a\nb"→"ab".
                b'\n' => self.input_buf.push(b' '),
                b if b >= 0x20 => self.input_buf.push(b),
                _ => {}
            }
        }
        events
    }

    /// Poll with the pane's current visible screen text. Detects activity
    /// via a hash of the text, records the tail for prompt matching, and
    /// emits Done / Waiting transitions.
    pub fn update(&mut self, now: Instant, screen_text: &str) -> Vec<WatchEvent> {
        let mut events = Vec::new();

        let mut hasher = DefaultHasher::new();
        screen_text.hash(&mut hasher);
        let hash = hasher.finish();

        if hash != self.last_hash {
            self.last_hash = hash;
            self.last_change = Some(now);
            if self.busy_since.is_none() {
                self.busy_since = Some(now);
            }
            self.output_tail = tail_chars(screen_text, OUTPUT_TAIL_MAX).to_string();
            if self.waiting {
                self.waiting = false;
                events.push(WatchEvent::Waiting(false));
            }
        }

        self.running = self
            .last_change
            .map(|t| now.duration_since(t) < RUNNING_WINDOW)
            .unwrap_or(false);

        if !self.waiting {
            if let Some(t) = self.last_change {
                if now.duration_since(t) >= WAITING_QUIET && looks_like_prompt(&self.output_tail) {
                    self.waiting = true;
                    events.push(WatchEvent::Waiting(true));
                }
            }
        }

        // FLAME resets busyStart on ANY quiet >= quietMs, not only when Done
        // fires — otherwise a stale busy_since survives a short burst and a
        // later single-frame change looks like a >=10s busy period.
        if let (Some(busy_start), Some(last)) = (self.busy_since, self.last_change) {
            if now.duration_since(last) >= QUIET {
                let busy_for = last.duration_since(busy_start);
                self.busy_since = None;
                if busy_for >= BUSY_MIN {
                    events.push(WatchEvent::Done {
                        command: self.last_command.clone(),
                    });
                }
            }
        }

        events
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn last_command(&self) -> &str {
        &self.last_command
    }
}

fn tail_chars(text: &str, max: usize) -> &str {
    if text.len() <= max {
        text
    } else {
        let mut start = text.len() - max;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        &text[start..]
    }
}

/// Prompt patterns ported verbatim from FLAME's taskWatcher.ts.
fn prompt_patterns() -> &'static [regex::Regex] {
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"(?i)\(y/n\)",
            r"(?i)\[y/n\]",
            r"\[y/N\]",
            r"\[Y/n\]",
            r"(?i)\(yes/no\)",
            r"(?i)do you want to proceed",
            r"(?i)do you want to make this edit",
            r"(?i)do you want to create",
            r"(?i)do you trust the files",
            r"(?i)trust this (folder|workspace|directory)",
            r"(?i)allow this (action|edit|command)",
            r"(?im)overwrite\?\s*$",
            r"(?im)continue\?\s*$",
            r"(?i)press enter to continue",
            r"(?i)\by/n\b",
            r"(?i)enter to (select|confirm)",
            r"(?i)esc(ape)? to cancel",
            r"(?i)\byes, and don't ask again\b",
            r"(?i)\(y\)es\b",
            r"(?i)\(n\)o\b",
            r"(?i)\(d\)on't ask",
            r"(?i)allow execution",
            r"(?i)yes, allow",
        ]
        .iter()
        .map(|p| regex::Regex::new(p).unwrap())
        .collect()
    })
}

pub fn looks_like_prompt(text: &str) -> bool {
    prompt_patterns().iter().any(|re| re.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn input_line_commits_on_enter_as_command_event() {
        let mut w = Watcher::new();
        assert!(w.on_input(b"git sta").is_empty());
        let events = w.on_input(b"tus\r");
        assert_eq!(events, vec![WatchEvent::Command("git status".to_string())]);
        assert_eq!(w.last_command(), "git status");
    }

    #[test]
    fn backspace_and_ctrl_c_edit_the_input_line() {
        let mut w = Watcher::new();
        w.on_input(b"ab\x7fc");           // "ac"
        let events = w.on_input(b"\x03ignored\r"); // ctrl-C clears, then a fresh line
        assert_eq!(events, vec![WatchEvent::Command("ignored".to_string())]);
    }

    #[test]
    fn empty_enter_emits_no_command() {
        let mut w = Watcher::new();
        assert!(w.on_input(b"\r").is_empty());
        assert!(w.on_input(b"  \r").is_empty());
    }

    #[test]
    fn escape_sequences_dont_pollute_the_command() {
        let mut w = Watcher::new();
        // Arrow keys mid-line: CSI \x1b[C (cursor keys) and SS3 \x1bOD.
        w.on_input(b"git \x1b[D\x1b[Cstatus\x1bOD\r");
        assert_eq!(w.last_command(), "git status");
        // Alt+key (ESC + printable) is swallowed, not typed.
        let mut w = Watcher::new();
        w.on_input(b"make\x1bx\r");
        assert_eq!(w.last_command(), "make");
        // A stray bracketed-paste frame would be stripped too.
        let mut w = Watcher::new();
        w.on_input(b"\x1b[200~deploy prod\x1b[201~\r");
        assert_eq!(w.last_command(), "deploy prod");
    }

    #[test]
    fn esc_then_enter_still_commits() {
        let mut w = Watcher::new();
        w.on_input(b"build\x1b\r");
        assert_eq!(w.last_command(), "build");
    }

    #[test]
    fn pasted_newlines_merge_as_spaces() {
        let mut w = Watcher::new();
        w.on_input(b"echo one\necho two\r");
        assert_eq!(w.last_command(), "echo one echo two");
    }

    #[test]
    fn busy_then_quiet_emits_done() {
        let mut w = Watcher::new();
        w.on_input(b"claude\r");
        // Screen changes at t=0..11s (busy 11s >= BUSY_MIN)
        assert!(w.update(t(0), "a").is_empty());
        assert!(w.update(t(11_000), "ab").is_empty());
        // 8s of quiet after last change at t=11s
        let events = w.update(t(19_100), "ab");
        assert_eq!(events, vec![WatchEvent::Done { command: "claude".to_string() }]);
    }

    #[test]
    fn short_burst_then_quiet_emits_nothing() {
        let mut w = Watcher::new();
        w.update(t(0), "a");
        w.update(t(2_000), "ab"); // only 2s busy
        assert!(w.update(t(10_100), "ab").is_empty());
    }

    #[test]
    fn prompt_text_at_rest_emits_waiting() {
        let mut w = Watcher::new();
        w.update(t(0), "Do you want to proceed?");
        let events = w.update(t(1_600), "Do you want to proceed?");
        assert_eq!(events, vec![WatchEvent::Waiting(true)]);
        assert!(w.is_waiting());
    }

    #[test]
    fn waiting_clears_on_new_output_and_on_input() {
        let mut w = Watcher::new();
        w.update(t(0), "Do you want to proceed?");
        w.update(t(1_600), "Do you want to proceed?");
        let events = w.update(t(2_000), "Do you want to proceed? ok");
        assert_eq!(events, vec![WatchEvent::Waiting(false)]);

        let mut w2 = Watcher::new();
        w2.update(t(0), "Do you want to proceed?");
        w2.update(t(1_600), "Do you want to proceed?");
        let events = w2.on_input(b"y");
        assert_eq!(events, vec![WatchEvent::Waiting(false)]);
    }

    #[test]
    fn running_flag_tracks_recent_change() {
        let mut w = Watcher::new();
        w.update(t(0), "a");
        assert!(w.is_running());
        w.update(t(2_000), "a");
        assert!(!w.is_running());
    }

    #[test]
    fn looks_like_prompt_matches_ported_patterns() {
        for text in [
            "Overwrite? (y/n)",
            "Continue? [y/N]",
            "Do you want to proceed?",
            "Do you trust the files in this folder?",
            "Allow this command?",
            "press enter to continue",
            "1. Yes  2. No\nEnter to select",
            "esc to cancel",
            "Yes, and don't ask again",
            "(Y)es/(N)o",
            "Allow execution of this tool?",
            "yes, allow all",
        ] {
            assert!(looks_like_prompt(text), "expected prompt match: {text}");
        }
        for text in ["compiling project...", "$ cargo build", "yes allowlist", ""] {
            assert!(!looks_like_prompt(text), "unexpected match: {text}");
        }
    }
}
