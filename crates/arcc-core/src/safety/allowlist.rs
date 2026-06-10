use std::collections::HashSet;

/// Command safety checker — identifies dangerous commands that need human
/// confirmation. All other commands are allowed by default.
///
/// This replaces the old allowlist model: there is no blocklist/allowlist,
/// only a `require_confirm` set for commands that are inherently dangerous
/// (e.g. `rm`, `mv`, `dd`, `mkfs`, `shutdown`).
#[derive(Debug, Clone)]
pub struct Allowlist {
    /// Commands that always need human confirmation before execution.
    require_confirm: HashSet<String>,
    /// Commands the user has approved interactively in this session.
    session_approved: HashSet<String>,
}

impl Allowlist {
    pub fn new(require_confirm: Vec<String>) -> Self {
        Self {
            require_confirm: require_confirm.into_iter().collect(),
            session_approved: HashSet::new(),
        }
    }

    /// Check whether `command` is permitted.
    ///
    /// Returns `Ok(true)` if the command needs human confirmation,
    /// `Ok(false)` if it is safe to run without asking.
    pub fn check(&self, command: &str) -> Result<bool, String> {
        let cmd = command.trim();
        let cmd_name = cmd.split_whitespace().next().unwrap_or(cmd);

        // Already approved by user this session — skip confirm.
        if self.session_approved.contains(cmd_name) {
            return Ok(false);
        }

        // Check if this matches a dangerous pattern.
        let needs_confirm = self
            .require_confirm
            .iter()
            .any(|danger| cmd_name == danger.as_str() || cmd.contains(danger.as_str()));

        Ok(needs_confirm)
    }

    /// Mark a command as user-approved for the remainder of this session.
    /// Subsequent uses of the same command will not prompt.
    pub fn approve(&mut self, cmd: String) {
        self.session_approved.insert(cmd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_al() -> Allowlist {
        Allowlist::new(vec!["rm".into(), "dd".into()])
    }

    #[test]
    fn safe_command_no_confirm() {
        let al = test_al();
        assert_eq!(al.check("ls -la").unwrap(), false);
    }

    #[test]
    fn dangerous_command_needs_confirm() {
        let al = test_al();
        assert_eq!(al.check("rm -rf /").unwrap(), true);
    }

    #[test]
    fn approved_command_no_confirm() {
        let mut al = test_al();
        al.approve("rm".into());
        assert_eq!(al.check("rm -rf /").unwrap(), false);
    }
}
