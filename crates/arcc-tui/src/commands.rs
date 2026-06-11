//! Slash command registry — metadata and completion for TUI commands.

/// Command category for help grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Navigation,
    View,
    Tools,
    System,
}

impl Category {
    pub fn label(&self) -> &'static str {
        match self {
            Category::Navigation => "navigation",
            Category::View => "view",
            Category::Tools => "tools",
            Category::System => "system",
        }
    }
}

/// A registered slash command.
pub struct Cmd {
    pub name: &'static str,
    pub desc: &'static str,
    pub usage: &'static str,
    pub cat: Category,
}

/// All registered commands.
pub static COMMANDS: &[Cmd] = &[
    Cmd {
        name: "plan",
        desc: "Plan a complex multi-step task",
        usage: "/plan <task description>",
        cat: Category::Tools,
    },
    Cmd {
        name: "clear",
        desc: "Clear chat history",
        usage: "/clear",
        cat: Category::View,
    },
    Cmd {
        name: "model",
        desc: "Show active model providers",
        usage: "/model",
        cat: Category::View,
    },
    Cmd {
        name: "skills",
        desc: "List registered MCP tools",
        usage: "/skills",
        cat: Category::Tools,
    },
    Cmd {
        name: "dashboard",
        desc: "Interactive dashboard with sessions, tokens, audit, and system info",
        usage: "/dashboard",
        cat: Category::System,
    },
    Cmd {
        name: "exec",
        desc: "Execute a shell command directly",
        usage: "/exec <command>",
        cat: Category::Tools,
    },
    Cmd {
        name: "stats",
        desc: "Show session statistics",
        usage: "/stats",
        cat: Category::System,
    },
    Cmd {
        name: "thinking",
        desc: "Toggle DeepSeek thinking mode on/off",
        usage: "/thinking",
        cat: Category::System,
    },
    Cmd {
        name: "exit",
        desc: "Quit ARCC",
        usage: "/exit",
        cat: Category::System,
    },
    Cmd {
        name: "help",
        desc: "Show help for commands",
        usage: "/help [command]",
        cat: Category::Navigation,
    },
];

/// Find commands whose name starts with `prefix` (excluding the `/`).
pub fn complete(prefix: &str) -> Vec<&'static str> {
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(prefix))
        .map(|c| c.name)
        .collect()
}

/// Look up a command by name.
pub fn find(name: &str) -> Option<&'static Cmd> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// Format help text for a single command (markdown, inline).
pub fn help_line(cmd: &Cmd) -> String {
    format!("**{}**  — {}  _({})_", cmd.usage, cmd.desc, cmd.cat.label())
}

/// Format the full help listing as a markdown table.
pub fn help_all() -> Vec<String> {
    let mut table = String::new();

    // Markdown table header
    table.push_str("| Command | Description | Category |\n");
    table.push_str("|---|---|---|\n");

    for cmd in COMMANDS {
        let escaped_usage = cmd.usage.replace('|', "\\|");
        let escaped_desc = cmd.desc.replace('|', "\\|");
        table.push_str(&format!(
            "| `{}` | {} | {} |\n",
            escaped_usage, escaped_desc, cmd.cat.label()
        ));
    }

    vec![
        "🤖 **Available commands**".into(),
        format!("🤖 {table}"),
        "🤖 Tip: type `/help <command>` for details on a specific command.".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_complete_full() {
        let r = complete("clear");
        assert_eq!(r, vec!["clear"]);
    }

    #[test]
    fn test_complete_partial() {
        let r = complete("cl");
        assert_eq!(r, vec!["clear"]);
    }

    #[test]
    fn test_complete_multiple() {
        let r = complete("");
        assert!(r.len() >= 2);
    }

    #[test]
    fn test_complete_none() {
        let r = complete("zzz");
        assert!(r.is_empty());
    }

    #[test]
    fn test_find() {
        assert!(find("help").is_some());
        assert!(find("nope").is_none());
    }
}
