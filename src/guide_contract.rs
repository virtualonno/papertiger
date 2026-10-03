//! Shipped guides name only commands and flags the CLI accepts. Every
//! `papertiger ...` invocation and every inline code span that starts with a
//! top-level command is checked against the clap command tree.

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    const GUIDES: &[(&str, &str)] = &[
        (
            "templates/skills/papertiger/SKILL.md",
            include_str!("../templates/skills/papertiger/SKILL.md"),
        ),
        (
            "templates/agent_integration.md",
            include_str!("../templates/agent_integration.md"),
        ),
        ("README.md", include_str!("../README.md")),
    ];

    /// Command lines in a Markdown guide: fenced lines that invoke
    /// `papertiger` (joining `\` continuations) and inline code spans.
    fn command_lines(text: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let mut in_fence = false;
        let mut pending = String::new();
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                let trimmed = line.trim();
                if !pending.is_empty() || trimmed.starts_with("papertiger ") {
                    pending.push_str(trimmed.trim_end_matches('\\'));
                    pending.push(' ');
                    if !trimmed.ends_with('\\') {
                        lines.push(std::mem::take(&mut pending));
                    }
                }
                continue;
            }
            let mut parts = line.split('`');
            parts.next();
            while let Some(span) = parts.next() {
                lines.push(span.to_owned());
                parts.next();
            }
        }
        lines
    }

    /// Check one command line; `None` when it does not name a command.
    fn check(root: &clap::Command, line: &str) -> Option<Result<(), String>> {
        let mut tokens = line.split_whitespace().peekable();
        if tokens.peek() == Some(&"papertiger") {
            tokens.next();
        } else if tokens.peek().is_some_and(|token| token.starts_with("papertiger")) {
            return None;
        }
        let first = *tokens.peek()?;
        root.find_subcommand(first)?;
        let mut command = root;
        let mut path = Vec::new();
        while let Some(&token) = tokens.peek() {
            match command.find_subcommand(token) {
                Some(sub) => {
                    path.push(token);
                    command = sub;
                    tokens.next();
                }
                None => break,
            }
        }
        for token in tokens {
            let Some(flag) = token.strip_prefix("--") else {
                continue;
            };
            let flag = flag.split('=').next().unwrap_or_default();
            let flag = flag.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
            if flag.is_empty() {
                continue;
            }
            let known = |command: &clap::Command| {
                command
                    .get_arguments()
                    .any(|argument| argument.get_long() == Some(flag))
            };
            if !known(command) && !known(root) && flag != "help" {
                return Some(Err(format!(
                    "`{}` names --{flag}, which `papertiger {}` does not accept",
                    line.trim(),
                    path.join(" ")
                )));
            }
        }
        Some(Ok(()))
    }

    #[test]
    fn shipped_guides_name_only_commands_and_flags_the_cli_accepts() {
        let root = crate::Cli::command();
        let mut problems = Vec::new();
        let mut checked = 0;
        for (path, text) in GUIDES {
            for line in command_lines(text) {
                match check(&root, &line) {
                    Some(Ok(())) => checked += 1,
                    Some(Err(problem)) => problems.push(format!("{path}: {problem}")),
                    None => {}
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
        assert!(checked > 50, "only {checked} command lines were checked");
    }

    #[test]
    fn the_guide_check_refuses_a_retired_flag() {
        let root = crate::Cli::command();
        assert!(check(&root, "papertiger add \"x\" --intent-source user").unwrap().is_err());
        assert!(check(&root, "blocker add 1 vendor").is_none(), "not a command any more");
        assert!(check(&root, "gate add <N> <name> --kind external --requirement <c>")
            .unwrap()
            .is_ok());
    }
}
