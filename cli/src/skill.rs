//! The coding-agent skill ships inside the binary, so the instructions always
//! match the CLI that runs them. `prmarmot-cli skill` prints it;
//! `prmarmot-cli skill install` writes it where Claude Code, or the agents
//! sharing `~/.agents/skills`, find user-level skills.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub const NAME: &str = "prmarmot-cli";
pub const SKILL_MD: &str = include_str!("../skills/prmarmot-cli/SKILL.md");

/// Whose user-level skills directory `skill install` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    /// Claude Code, which reads only its own directory.
    Claude,
    /// The shared `~/.agents/skills`.
    Agents,
    /// Both.
    All,
}

/// Who reads the shared directory (first-party docs or source, 2026-09).
pub const SHARED_READERS: &str = "Codex, Copilot, Cursor, Gemini CLI, OpenCode, and Amp";

impl Agent {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Ok(Self::Claude),
            "agents" | "codex" | "copilot" | "cursor" | "gemini" | "opencode" | "amp" => {
                Ok(Self::Agents)
            }
            "all" => Ok(Self::All),
            _ => Err(format!(
                "unknown agent: {value} (use claude, agents, or all; codex, copilot, cursor, \
                 gemini, opencode, and amp mean agents)"
            )),
        }
    }
}

/// The directories to install into for `agent`, each with who reads it.
pub fn skills_dirs(
    agent: Agent,
    claude_config_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<Vec<(PathBuf, &'static str)>> {
    let claude = || {
        default_skills_dir(claude_config_dir.clone(), home.clone()).map(|dir| (dir, "Claude Code"))
    };
    let shared = || {
        home.clone()
            .map(|home| (home.join(".agents").join("skills"), SHARED_READERS))
    };
    match agent {
        Agent::Claude => Some(vec![claude()?]),
        Agent::Agents => Some(vec![shared()?]),
        Agent::All => Some(vec![claude()?, shared()?]),
    }
}

/// Claude Code's user-level skills directory: `$CLAUDE_CONFIG_DIR/skills`,
/// otherwise `~/.claude/skills`.
pub fn default_skills_dir(
    claude_config_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    claude_config_dir
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join("skills"))
        .or_else(|| home.map(|home| home.join(".claude").join("skills")))
}

/// `~/…` from `--dir=~/…`, which the shell does not expand after `=`.
pub fn expand_home(path: PathBuf, home: Option<PathBuf>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Updated,
    Unchanged,
}

fn describe_link(path: &Path) -> String {
    fs::read_link(path)
        .map(|target| target.display().to_string())
        .unwrap_or_else(|_| "an unreadable target".into())
}

/// Refuse to write through a symlink (it would edit whatever it points at,
/// such as a checkout); with `force`, remove the link itself.
fn clear_symlink(path: &Path, force: bool) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if !force {
                return Err(format!(
                    "{} is a symlink to {}; pass --force to replace the link with an installed copy",
                    path.display(),
                    describe_link(path)
                ));
            }
            fs::remove_file(path)
                .map_err(|error| format!("cannot remove {}: {error}", path.display()))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Install `content` as `<skills_dir>/prmarmot-cli/SKILL.md`. An identical
/// copy is left alone; a different one (edited, or from another version) is
/// replaced only with `force`. The write is atomic.
pub fn install(
    skills_dir: &Path,
    content: &str,
    force: bool,
) -> Result<(PathBuf, Outcome), String> {
    let dir = skills_dir.join(NAME);
    let file = dir.join("SKILL.md");
    let mut replaced = clear_symlink(&dir, force)?;
    if dir.exists() && !dir.is_dir() {
        return Err(format!("{} exists and is not a directory", dir.display()));
    }
    replaced |= clear_symlink(&file, force)?;
    match fs::read(&file) {
        Ok(existing) if existing == content.as_bytes() => {
            return Ok((file, Outcome::Unchanged));
        }
        Ok(_) if !force => {
            return Err(format!(
                "{} differs from this prmarmot-cli's skill (edited, or from another version); \
                 pass --force to replace it",
                file.display()
            ));
        }
        Ok(_) => replaced = true,
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot read {}: {error}", file.display())),
    }
    fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let temp = dir.join(format!(".SKILL.md.{}.tmp", std::process::id()));
    fs::write(&temp, content)
        .and_then(|_| fs::rename(&temp, &file))
        .map_err(|error| {
            let _ = fs::remove_file(&temp);
            format!("cannot write {}: {error}", file.display())
        })?;
    Ok((
        file,
        if replaced {
            Outcome::Updated
        } else {
            Outcome::Created
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{self, Command};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!(
                "prmarmot-cli-{label}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn skills_dir_prefers_claude_config_dir_then_home() {
        assert_eq!(
            default_skills_dir(Some("/cfg".into()), Some("/home/me".into())),
            Some(PathBuf::from("/cfg/skills"))
        );
        assert_eq!(
            default_skills_dir(Some("".into()), Some("/home/me".into())),
            Some(PathBuf::from("/home/me/.claude/skills"))
        );
        assert_eq!(default_skills_dir(None, None), None);
        assert_eq!(
            expand_home("~/agents/skills".into(), Some("/home/me".into())),
            PathBuf::from("/home/me/agents/skills")
        );
        assert_eq!(
            expand_home("/abs".into(), Some("/home/me".into())),
            PathBuf::from("/abs")
        );
    }

    #[test]
    fn agents_pick_claude_the_shared_directory_or_both() {
        let home = || Some(PathBuf::from("/home/me"));
        assert_eq!(Agent::parse("Codex"), Ok(Agent::Agents));
        assert_eq!(Agent::parse("all"), Ok(Agent::All));
        assert!(Agent::parse("vim")
            .unwrap_err()
            .starts_with("unknown agent: vim"));
        assert_eq!(
            skills_dirs(Agent::Claude, Some("/cfg".into()), home()),
            Some(vec![(PathBuf::from("/cfg/skills"), "Claude Code")])
        );
        assert_eq!(
            skills_dirs(Agent::Agents, Some("/cfg".into()), home()),
            Some(vec![(
                PathBuf::from("/home/me/.agents/skills"),
                SHARED_READERS
            )])
        );
        assert_eq!(
            skills_dirs(Agent::All, None, home())
                .unwrap()
                .into_iter()
                .map(|(dir, _)| dir)
                .collect::<Vec<_>>(),
            [
                PathBuf::from("/home/me/.claude/skills"),
                PathBuf::from("/home/me/.agents/skills")
            ]
        );
        assert_eq!(skills_dirs(Agent::Agents, Some("/cfg".into()), None), None);
    }

    #[test]
    fn install_creates_then_is_idempotent_and_guards_edits() {
        let root = TempDir::new("install");
        let (file, outcome) = install(&root.0, "v1", false).unwrap();
        assert_eq!(outcome, Outcome::Created);
        assert_eq!(file, root.0.join("prmarmot-cli/SKILL.md"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "v1");
        assert_eq!(install(&root.0, "v1", false).unwrap().1, Outcome::Unchanged);

        let error = install(&root.0, "v2", false).unwrap_err();
        assert!(error.contains("--force"), "{error}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "v1");
        assert_eq!(install(&root.0, "v2", true).unwrap().1, Outcome::Updated);
        assert_eq!(fs::read_to_string(&file).unwrap(), "v2");
        let leftovers: Vec<_> = fs::read_dir(root.0.join(NAME))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["SKILL.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn install_never_writes_through_a_symlinked_skill() {
        let root = TempDir::new("symlink");
        let checkout = root.0.join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("SKILL.md"), "source").unwrap();
        let skills = root.0.join("skills");
        fs::create_dir_all(&skills).unwrap();
        std::os::unix::fs::symlink(&checkout, skills.join(NAME)).unwrap();

        let error = install(&skills, "installed", false).unwrap_err();
        assert!(error.contains("is a symlink"), "{error}");
        let (file, outcome) = install(&skills, "installed", true).unwrap();
        assert_eq!(outcome, Outcome::Updated);
        assert!(!fs::symlink_metadata(skills.join(NAME))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(file).unwrap(), "installed");
        assert_eq!(
            fs::read_to_string(checkout.join("SKILL.md")).unwrap(),
            "source"
        );
    }

    #[test]
    fn embedded_skill_is_named_and_its_commands_parse() {
        assert!(SKILL_MD.starts_with(&format!("---\nname: {NAME}\ndescription: ")));
        let mut in_code = false;
        let mut checked = 0;
        for line in SKILL_MD.lines() {
            if line.trim_start().starts_with("```") {
                in_code = !in_code;
                continue;
            }
            let line = line.trim();
            if !in_code || !line.starts_with("prmarmot-cli ") {
                continue;
            }
            let words = line
                .split_whitespace()
                .skip(1)
                .take_while(|word| *word != "#" && *word != "|")
                .map(str::to_owned);
            match args::parse(words) {
                Ok(Command::View(_) | Command::Watch(_) | Command::Skill(_)) => checked += 1,
                other => panic!("skill example does not parse: {line} -> {other:?}"),
            }
        }
        assert!(checked >= 5, "only {checked} examples found");
    }
}
