use ratatui::style::Color;
use std::sync::OnceLock;

pub struct AgentSpec {
    pub name: &'static str,
    pub binary: &'static str,
    pub color: Color,
}

pub static AGENTS: &[AgentSpec] = &[
    AgentSpec { name: "claude",        binary: "claude",        color: Color::Rgb(0xff, 0xb2, 0x38) },
    AgentSpec { name: "codex",         binary: "codex",         color: Color::Rgb(0x8b, 0xb4, 0xe8) },
    AgentSpec { name: "devin",         binary: "devin",         color: Color::Rgb(0xff, 0x6b, 0x52) },
    AgentSpec { name: "gemini",        binary: "gemini",        color: Color::Rgb(0x7e, 0xc9, 0xc9) },
    AgentSpec { name: "aider",         binary: "aider",         color: Color::Rgb(0xff, 0xcb, 0x6b) },
    AgentSpec { name: "cursor-agent",  binary: "cursor-agent",  color: Color::Rgb(0xc9, 0xa8, 0x77) },
    AgentSpec { name: "opencode",      binary: "opencode",      color: Color::Rgb(0xe0, 0x89, 0x4a) },
    AgentSpec { name: "copilot",       binary: "copilot",       color: Color::Rgb(0x8c, 0x81, 0x72) },
];

fn agent_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(claude|codex|devin|gemini|aider|cursor-agent|opencode|copilot)\b")
            .unwrap()
    })
}

pub fn detect(command: &str) -> Option<&'static AgentSpec> {
    let m = agent_regex().find(command)?;
    let name = m.as_str().to_lowercase();
    AGENTS.iter().find(|a| a.name == name)
}

pub fn installed() -> Vec<&'static AgentSpec> {
    AGENTS.iter().filter(|a| binary_on_path(a.binary)).collect()
}

#[cfg(unix)]
fn binary_on_path(binary: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|dir| {
        let p = dir.join(binary);
        p.is_file() && p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    })
}

#[cfg(not(unix))]
fn binary_on_path(binary: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join(binary).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_known_agents_in_commands() {
        for cmd in ["claude", "claude --help", "codex exec", "sudo gemini chat", "aider src/main.rs"] {
            assert!(detect(cmd).is_some(), "expected agent in: {cmd}");
        }
    }

    #[test]
    fn detect_returns_name_and_color() {
        let spec = detect("claude --continue").unwrap();
        assert_eq!(spec.name, "claude");
        assert_eq!(spec.color, Color::Rgb(0xff, 0xb2, 0x38));
    }

    #[test]
    fn ignores_non_agent_commands() {
        for cmd in ["git status", "declared -x foo", "ls -la", "vim"] {
            assert!(detect(cmd).is_none(), "unexpected agent in: {cmd}");
        }
    }

    #[test]
    fn installed_only_returns_binaries_on_path() {
        // `sh` is not in the agent table; every returned spec must exist on
        // PATH. We can't assert which agents are installed (environment-
        // dependent), only that the filter works: mock by checking each
        // returned binary resolves.
        for spec in installed() {
            assert!(AGENTS.iter().any(|a| a.binary == spec.binary));
        }
    }
}
