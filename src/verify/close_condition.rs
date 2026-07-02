use std::path::Path;

use anyhow::Result;

use super::{VerifyResult, VerifyTier, verification_env_overrides};

#[derive(Debug, Clone, PartialEq, Eq)]
struct CloseConditionCommand {
    program: String,
    args: Vec<String>,
    display: String,
}

/// A bead-declared verification command parsed from an explicitly runnable
/// close condition.
pub(super) struct CloseConditionCheck {
    command: CloseConditionCommand,
}

impl CloseConditionCheck {
    pub(super) fn from_text(text: &str) -> Option<Self> {
        extract_close_condition_command(text).map(|command| Self { command })
    }

    #[cfg(test)]
    fn from_command(command: &str) -> Option<Self> {
        parse_close_condition_command(command).map(|command| Self { command })
    }
}

impl VerifyTier for CloseConditionCheck {
    fn name(&self) -> &str {
        "close-condition"
    }

    fn check(&self, work_dir: &Path) -> Result<VerifyResult> {
        let mut command = std::process::Command::new(&self.command.program);
        command
            .args(&self.command.args)
            .current_dir(work_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        for (key, value) in verification_env_overrides(&self.command.program) {
            command.env(key, value);
        }

        let output = match command.output() {
            Ok(output) => output,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(VerifyResult::Fail(format!(
                    "close condition command not found: {}",
                    self.command.program
                )));
            }
            Err(e) => return Err(e.into()),
        };

        if output.status.success() {
            Ok(VerifyResult::Pass)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.lines().take(3).collect::<Vec<_>>().join(" ");
            let suffix = if detail.trim().is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            };
            Ok(VerifyResult::Fail(format!(
                "close condition failed with exit code {} for `{}`{}",
                output.status.code().unwrap_or(-1),
                self.command.display,
                suffix
            )))
        }
    }
}

fn extract_close_condition_command(text: &str) -> Option<CloseConditionCommand> {
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    for line in &lines {
        if let Some(candidate) = labeled_close_condition(line)
            && let Some(command) = parse_candidate(candidate)
        {
            return Some(command);
        }
    }

    for candidate in backticked_segments(text) {
        if let Some(command) = parse_close_condition_command(&candidate) {
            return Some(command);
        }
    }

    for line in lines {
        if labeled_close_condition(line).is_some() {
            continue;
        }
        if let Some(candidate) = embedded_close_condition_command(line)
            && let Some(command) = parse_close_condition_command(candidate)
        {
            return Some(command);
        } else if let Some(command) = parse_close_condition_command(line) {
            return Some(command);
        }
    }

    None
}

fn parse_candidate(candidate: &str) -> Option<CloseConditionCommand> {
    if let Some(command) = parse_close_condition_command(candidate) {
        return Some(command);
    }
    for segment in backticked_segments(candidate) {
        if let Some(command) = parse_close_condition_command(&segment) {
            return Some(command);
        }
    }
    embedded_close_condition_command(candidate).and_then(parse_close_condition_command)
}

fn backticked_segments(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('`') else {
            break;
        };
        segments.push(after_start[..end].trim().to_string());
        rest = &after_start[end + 1..];
    }
    segments
}

fn labeled_close_condition(line: &str) -> Option<&str> {
    let lower = line.to_ascii_lowercase();
    for prefix in [
        "close condition:",
        "close-condition:",
        "verification:",
        "verify:",
        "acceptance:",
        "acceptance criteria:",
        "run:",
    ] {
        if lower.starts_with(prefix) {
            return Some(line[prefix.len()..].trim());
        }
    }
    None
}

fn embedded_close_condition_command(line: &str) -> Option<&str> {
    let lower = line.to_ascii_lowercase();
    let mut earliest: Option<usize> = None;
    for pattern in [
        "cargo test",
        "cargo check",
        "cargo build",
        "cargo clippy",
        "cargo nextest",
        "npm run test",
        "npm test",
        "pnpm test",
        "yarn test",
        "pytest",
        "go test",
        "make test",
        "task test",
        "just test",
    ] {
        if let Some(index) = lower.find(pattern) {
            earliest = Some(earliest.map_or(index, |current| current.min(index)));
        }
    }
    earliest.map(|index| line[index..].trim().trim_end_matches(['.', ';', ',']))
}

fn parse_close_condition_command(command: &str) -> Option<CloseConditionCommand> {
    let tokens = split_command_words(command)?;
    if tokens.is_empty()
        || tokens.iter().any(|token| contains_shell_operator(token))
        || !is_allowed_close_condition_command(&tokens)
    {
        return None;
    }
    let program = tokens[0].clone();
    let args = tokens[1..].to_vec();
    Some(CloseConditionCommand {
        program,
        args,
        display: tokens.join(" "),
    })
}

fn contains_shell_operator(token: &str) -> bool {
    token.contains("&&")
        || token.contains("||")
        || token.contains(';')
        || token.contains('|')
        || token.contains('<')
        || token.contains('>')
}

fn split_command_words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = command.trim().chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => quote = Some(ch),
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (None, c) => current.push(c),
        }
    }

    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        words.push(current);
    }
    Some(words)
}

fn is_allowed_close_condition_command(tokens: &[String]) -> bool {
    match tokens {
        [program, subcommand, ..]
            if program == "cargo"
                && matches!(
                    subcommand.as_str(),
                    "test" | "check" | "build" | "clippy" | "nextest"
                ) =>
        {
            true
        }
        [program, ..] if program == "pytest" => true,
        [program, subcommand, ..] if program == "go" && subcommand == "test" => true,
        [program, subcommand, ..] if program == "make" && subcommand == "test" => true,
        [program, subcommand, ..] if program == "task" && subcommand == "test" => true,
        [program, subcommand, ..] if program == "just" && subcommand == "test" => true,
        [program, subcommand, ..] if program == "npm" && subcommand == "test" => true,
        [program, subcommand, script, ..]
            if program == "npm" && subcommand == "run" && script == "test" =>
        {
            true
        }
        [program, subcommand, ..]
            if matches!(program.as_str(), "pnpm" | "yarn") && subcommand == "test" =>
        {
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn command_is_extracted_only_from_runnable_commands() {
        let extracted = extract_close_condition_command(
            "Resolved by running `cargo test -p rosary close_condition_verify_tier`.",
        )
        .expect("backticked cargo test should be executable");
        assert_eq!(extracted.program, "cargo");
        assert_eq!(
            extracted.args,
            vec!["test", "-p", "rosary", "close_condition_verify_tier"]
        );

        let labeled = extract_close_condition_command(
            "Close condition: cargo test -p rosary \"close condition verify tier\"",
        )
        .expect("labeled close condition should be executable");
        assert_eq!(labeled.program, "cargo");
        assert_eq!(
            labeled.args,
            vec!["test", "-p", "rosary", "close condition verify tier"]
        );

        let embedded = extract_close_condition_command("Run cargo test --help.")
            .expect("legacy prose with a known command prefix should be executable");
        assert_eq!(embedded.program, "cargo");
        assert_eq!(embedded.args, vec!["test", "--help"]);

        let labeled_with_backticks =
            extract_close_condition_command("Verify: `cargo test -p rosary labeled`")
                .expect("labeled close condition should parse backticked command");
        assert_eq!(labeled_with_backticks.program, "cargo");
        assert_eq!(
            labeled_with_backticks.args,
            vec!["test", "-p", "rosary", "labeled"]
        );

        let labeled_wins = extract_close_condition_command(
            "Incidental example: `cargo test --help`\nClose condition: cargo test -p rosary intended",
        )
        .expect("labeled close condition should win over incidental backticks");
        assert_eq!(labeled_wins.program, "cargo");
        assert_eq!(labeled_wins.args, vec!["test", "-p", "rosary", "intended"]);

        assert!(
            extract_close_condition_command(crate::bead::DEFAULT_PR_MERGE_CLOSE_CONDITION)
                .is_none(),
            "default PR-merge close signal is observable, not directly executable"
        );
        assert!(
            extract_close_condition_command("Resolved when the reviewer approves the PR.")
                .is_none()
        );

        assert!(
            extract_close_condition_command("Close condition: cargo test && cargo clippy")
                .is_none(),
            "shell control operators are not native executable commands"
        );
    }

    #[test]
    fn check_gates_on_command_exit_status() {
        let pass = CloseConditionCheck::from_command("cargo test --help")
            .expect("cargo test --help is a recognized runnable command");
        assert_eq!(pass.name(), "close-condition");
        assert_eq!(pass.check(Path::new(".")).unwrap(), VerifyResult::Pass);

        let fail =
            CloseConditionCheck::from_command("cargo test --manifest-path /definitely/missing")
                .expect("cargo test command should parse");
        let result = fail.check(Path::new(".")).unwrap();
        match result {
            VerifyResult::Fail(message) => {
                assert!(message.contains("close condition failed"), "{message}");
            }
            other => panic!("expected failing close condition, got {other:?}"),
        }
    }
}
