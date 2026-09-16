//! `prmarmot-cli completions SHELL`: static completion scripts from
//! `cli/completions/`. The tests keep them in step with the usage text and run
//! each script in its shell when that shell is installed.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            "fish" => Ok(Self::Fish),
            other => Err(format!("unknown shell: {other} (use bash, zsh, or fish)")),
        }
    }

    pub fn script(self) -> &'static str {
        match self {
            Self::Bash => include_str!("../completions/prmarmot-cli.bash"),
            Self::Zsh => include_str!("../completions/prmarmot-cli.zsh"),
            Self::Fish => include_str!("../completions/prmarmot-cli.fish"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::USAGE;
    use std::process::Command;

    const SHELLS: [Shell; 3] = [Shell::Bash, Shell::Zsh, Shell::Fish];

    /// Every `--flag` the usage text documents.
    fn documented_flags() -> Vec<&'static str> {
        let mut flags: Vec<&str> = USAGE
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .filter(|word| word.starts_with("--") && word.len() > 2)
            .collect();
        flags.sort_unstable();
        flags.dedup();
        flags
    }

    #[test]
    fn every_documented_command_and_flag_completes() {
        let flags = documented_flags();
        assert!(flags.contains(&"--until") && flags.contains(&"--agent"));
        for shell in SHELLS {
            let script = shell.script();
            for flag in &flags {
                let spelled = match shell {
                    Shell::Fish => format!("-l {}", &flag[2..]),
                    _ => flag.to_string(),
                };
                assert!(script.contains(&spelled), "{shell:?} lacks {flag}");
            }
            for word in [
                "mine",
                "review",
                "watch",
                "skill",
                "install",
                "completions",
                "ci-pass",
                "approved",
                "mergeable",
                "merged",
                "markdown",
                "text",
                "claude",
                "agents",
            ] {
                assert!(script.contains(word), "{shell:?} lacks {word}");
            }
        }
        for line in USAGE.lines() {
            if let Some(command) = line.trim().strip_prefix("prmarmot-cli ") {
                let command = command.split_whitespace().next().unwrap();
                if !command.chars().all(|c| c.is_ascii_lowercase()) {
                    continue; // the title line
                }
                assert!(
                    SHELLS.iter().all(|shell| shell.script().contains(command)),
                    "{command}"
                );
            }
        }
    }

    #[test]
    fn the_command_prints_each_script() {
        assert_eq!(Shell::parse("zsh"), Ok(Shell::Zsh));
        assert!(Shell::parse("powershell").is_err());
        assert!(Shell::Zsh.script().starts_with("#compdef prmarmot-cli"));
        assert!(Shell::Bash
            .script()
            .contains("complete -F _prmarmot_cli prmarmot-cli"));
        assert!(Shell::Fish.script().contains("complete -c prmarmot-cli"));
    }

    /// Runs `script` in `shell`; `None` when that shell isn't installed.
    fn run(shell: &str, args: &[&str], script: &str) -> Option<String> {
        let output = match Command::new(shell).args(args).arg(script).output() {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => panic!("{shell}: {error}"),
        };
        assert!(
            output.status.success(),
            "{shell} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(String::from_utf8(output.stdout).unwrap())
    }

    #[test]
    fn each_script_parses_in_its_shell() {
        run("bash", &["-n", "-c"], Shell::Bash.script());
        run("zsh", &["-n", "-c"], Shell::Zsh.script());
        run("fish", &["--no-execute", "-c"], Shell::Fish.script());
    }

    /// What bash offers for the words typed so far (the last one is being
    /// completed).
    fn bash_offers(words: &[&str]) -> Option<String> {
        let quoted: Vec<String> = words.iter().map(|word| format!("'{word}'")).collect();
        let driver = format!(
            "{}\nCOMP_WORDS=({})\nCOMP_CWORD={}\n_prmarmot_cli\necho \"${{COMPREPLY[*]}}\"",
            Shell::Bash.script(),
            quoted.join(" "),
            words.len() - 1
        );
        run("bash", &["-c"], &driver).map(|out| out.trim().to_owned())
    }

    #[test]
    fn bash_offers_what_each_position_accepts() {
        let cases: [(&[&str], &str); 13] = [
            (&["prmarmot-cli", "wa"], "watch"),
            (&["prmarmot-cli", "--v"], "--version"),
            (&["prmarmot-cli", "watch", ""], "mine review"),
            (
                &["prmarmot-cli", "watch", "--pr", "o/n#1", "--un"],
                "--until",
            ),
            (
                &["prmarmot-cli", "watch", "--pr", "o/n#1", ""],
                concat!(
                    "--repo --all-repos --format --json --watched --snoozed --no-color --help ",
                    "--authored --interval --events --pr --until --timeout"
                ),
            ),
            (
                &["prmarmot-cli", "watch", "--until", "ci-pass,ap"],
                "ci-pass,approved",
            ),
            (&["prmarmot-cli", "watch", "-f", ""], "text json"),
            (&["prmarmot-cli", "mine", "--format", "=", "j"], "json"),
            (&["prmarmot-cli", "review", "--au"], ""),
            (&["prmarmot-cli", "review", "--so"], "--sort"),
            (&["prmarmot-cli", "review", "--sort", "s"], "smallest"),
            (
                &["prmarmot-cli", "skill", "install", "--agent", "a"],
                "agents all",
            ),
            (&["prmarmot-cli", "completions", ""], "bash zsh fish"),
        ];
        for (words, expected) in cases {
            let Some(offered) = bash_offers(words) else {
                return;
            };
            assert_eq!(offered, expected, "{words:?}");
        }
    }

    #[test]
    fn fish_offers_what_each_position_accepts() {
        let offers = |line: &str| {
            let driver = format!("{}\ncomplete -C '{line}'", Shell::Fish.script());
            run("fish", &["--no-config", "-c"], &driver).map(|out| {
                out.lines()
                    .map(|line| line.split('\t').next().unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
        };
        let Some(commands) = offers("prmarmot-cli wa") else {
            return;
        };
        assert_eq!(commands, ["watch"]);
        assert_eq!(offers("prmarmot-cli watch ").unwrap(), ["mine", "review"]);
        assert_eq!(
            offers("prmarmot-cli watch --until ci-pass,ap").unwrap(),
            ["ci-pass,approved"]
        );
        assert_eq!(
            offers("prmarmot-cli mine --format ").unwrap(),
            ["json", "markdown", "table"]
        );
        assert!(!offers("prmarmot-cli review --")
            .unwrap()
            .contains(&"--authored".to_owned()));
        assert!(!offers("prmarmot-cli mine --")
            .unwrap()
            .contains(&"--sort".to_owned()));
        assert_eq!(
            offers("prmarmot-cli review --sort ").unwrap(),
            ["smallest", "wait"]
        );
        assert_eq!(
            offers("prmarmot-cli completions ").unwrap(),
            ["bash", "fish", "zsh"]
        );
    }
}
