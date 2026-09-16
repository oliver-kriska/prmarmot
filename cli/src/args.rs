//! Command-line parsing. Hand-rolled like the app's: a handful of flags does
//! not justify a parser dependency, and the grammar stays easy to test.

use std::path::PathBuf;
use std::time::Duration;

use prmarmot_core::board::{BoardScope, Mode, MAX_PAGES_PER_ALIAS};

use crate::completions::Shell;
use crate::skill::Agent;
use crate::until::{parse_duration, Condition};
use prmarmot_core::github::rate_limit::MIN_REFRESH_SECS;

pub const USAGE: &str = "\
prmarmot-cli — PR Marmot's views for terminals and coding agents

Usage:
  prmarmot-cli mine   [options]          PRs you authored (My PRs); with --all-repos,
                                         every open PR involving you (Involving me)
                                         unless --authored
  prmarmot-cli review [options]          PRs waiting for your review (Review queue)
  prmarmot-cli watch  [mine|review] [options]
                                         Poll and print what changes, one event per line
  prmarmot-cli watch --pr OWNER/NAME#N   Follow one pull request until it merges or closes
  prmarmot-cli watch --pr OWNER/NAME#N --until ci-pass [--timeout 30m]
                                         Wait for a condition instead of polling in a loop
  prmarmot-cli skill                     Print the coding-agent skill (SKILL.md)
  prmarmot-cli skill install [--agent AGENT | --dir DIR] [--force]
                                         Install it as a user-level skill
  prmarmot-cli completions SHELL         Print a completion script for bash, zsh, or fish

Scope (default: --repo/--all-repos > PRMARMOT_REPO/PRMARMOT_SCOPE > config file):
      --repo OWNER/NAME     One repository
      --all-repos           Every repository involving you
      --authored            With `mine --all-repos`: only PRs you authored (My PRs)

View options:
  -f, --format FORMAT       table | markdown | json
                            (default: table on a terminal, markdown when piped)
      --json                Same as --format json
      --changed             Only PRs changed since you last looked in PR Marmot
      --watched             Only PRs you watch in PR Marmot
      --stale               Only PRs that have waited stale_after_days (default 3)
                            or longer for a reviewer
      --snoozed             Show snoozed PRs instead of collapsing them
      --pages N             Result pages to load per queue, 1-5 (default 1)
      --no-color            Plain text (also honors NO_COLOR)

Watch options:
  -f, --format FORMAT       text | json (default: text on a terminal, JSON lines when piped)
      --json                Same as --format json
      --interval SECONDS    Poll interval, at least 30 (default: refresh_secs, 300)
      --events N            Exit after N change events
      --watched             Only events for PRs you watch in PR Marmot
      --snoozed             Include events for snoozed PRs
      --pr OWNER/NAME#N     One pull request (or its URL), in any repository; the watch
                            ends with a `removed` event when it merges, closes, or
                            becomes inaccessible. Not combined with scope, --watched,
                            or --authored
      --until CONDITION     With --pr: stop with an `until` event once the PR is
                            ci-pass (check rollup green; no checks never is),
                            approved (GitHub's review decision; without branch
                            protection, the latest reviews, yours included, approve
                            and none request changes; a later comment keeps an
                            approval), mergeable (approved, CI green,
                            no conflict, not a draft, nothing blocking), or merged.
                            Repeat or comma-separate for any of them. A merge meets
                            any of them (the `until` event then names merged). Checked
                            on every poll, the first included; not with --events
      --timeout DURATION    With --pr: give up after 90s, 30m, 2h, 1h30m, ...

Skill install options:
      --agent AGENT         claude (default): $CLAUDE_CONFIG_DIR/skills or ~/.claude/skills
                            agents: ~/.agents/skills, read by Codex, Copilot, Cursor,
                            Gemini CLI, OpenCode, and Amp (their names work too)
                            all: both
      --dir DIR             Any other skills directory
      --force               Replace a copy that differs or is a symlink

  -h, --help                Show this help
  -V, --version             Show the version

Exit codes: 0 ok, 1 GitHub, network, or file error, 2 usage error,
            3 gh missing or not signed in, 4 rate limited,
            5 --until can no longer be met (CI failed, changes requested, or the
              PR closed without merging or became inaccessible),
            6 --timeout reached.

Reads ~/.config/prmarmot/config.toml and PR Marmot's watch/snooze state, never
writes them. Authentication comes from the GitHub CLI (`gh auth login`).";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Table,
    Markdown,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewArgs {
    pub mode: Mode,
    pub scope: Option<BoardScope>,
    /// `--authored`: all-repositories My PRs lists only your PRs.
    pub authored: bool,
    /// `None` picks by whether stdout is a terminal.
    pub format: Option<Format>,
    pub changed: bool,
    pub watched: bool,
    pub stale: bool,
    pub snoozed: bool,
    pub pages: u8,
    pub no_color: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchArgs {
    pub mode: Mode,
    pub scope: Option<BoardScope>,
    pub authored: bool,
    pub pr: Option<PrRef>,
    /// `--until`, any-of, in the order given; empty = follow until it closes.
    pub until: Vec<Condition>,
    pub timeout: Option<Duration>,
    pub format: Option<EventFormat>,
    pub interval_secs: Option<u64>,
    pub max_events: Option<u64>,
    pub watched: bool,
    pub snoozed: bool,
    pub no_color: bool,
}

/// `watch --pr`: one pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillAction {
    Show,
    Install {
        dir: Option<PathBuf>,
        agent: Option<Agent>,
        force: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    View(ViewArgs),
    Watch(WatchArgs),
    Skill(SkillAction),
    Completions(Shell),
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    // `--flag=value` is accepted everywhere a value is.
    let mut tokens = Vec::new();
    for arg in args {
        match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                tokens.push(flag.to_owned());
                tokens.push(value.to_owned());
            }
            _ => tokens.push(arg),
        }
    }
    if tokens.iter().any(|t| t == "-h" || t == "--help") {
        return Ok(Command::Help);
    }
    if tokens.iter().any(|t| t == "-V" || t == "--version") {
        return Ok(Command::Version);
    }
    let mut tokens = tokens.into_iter().peekable();
    let Some(command) = tokens.next() else {
        return Ok(Command::Help);
    };
    let watch = match command.as_str() {
        "help" => return Ok(Command::Help),
        "skill" => return parse_skill(tokens),
        "completions" => return parse_completions(tokens),
        "watch" => true,
        _ => false,
    };
    let mode = if watch {
        match tokens.peek().map(String::as_str) {
            Some(word) if !word.starts_with('-') => {
                let mode = parse_mode(word)?;
                tokens.next();
                mode
            }
            _ => Mode::Authored,
        }
    } else {
        parse_mode(&command)?
    };

    let mut scope = None;
    let mut authored = false;
    let mut pr = None;
    let mut format = None;
    let mut changed = false;
    let mut watched = false;
    let mut stale = false;
    let mut snoozed = false;
    let mut pages = None;
    let mut no_color = false;
    let mut interval_secs = None;
    let mut max_events = None;
    let mut until: Vec<Condition> = Vec::new();
    let mut timeout = None;

    while let Some(flag) = tokens.next() {
        let mut value = |name: &str| tokens.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--repo" => scope = Some(BoardScope::Repository(parse_repo(&value("--repo")?)?)),
            "--all-repos" => scope = Some(BoardScope::AllRepositories),
            "--authored" if mode == Mode::Authored => authored = true,
            "--authored" => return Err("--authored applies to `mine`, not `review`".into()),
            "-f" | "--format" => format = Some(value("--format")?),
            "--json" => format = Some("json".into()),
            "--watched" => watched = true,
            "--snoozed" => snoozed = true,
            "--no-color" => no_color = true,
            "--changed" if !watch => changed = true,
            "--stale" if !watch => stale = true,
            "--pages" if !watch => pages = Some(parse_pages(&value("--pages")?)?),
            "--interval" if watch => interval_secs = Some(parse_interval(&value("--interval")?)?),
            "--events" if watch => max_events = Some(parse_events(&value("--events")?)?),
            "--pr" if watch => pr = Some(parse_pr(&value("--pr")?)?),
            "--pr" => return Err("--pr applies to `watch`".into()),
            "--until" if watch => {
                let words = value("--until")?;
                let mut words = words.split(',').filter(|w| !w.trim().is_empty()).peekable();
                if words.peek().is_none() {
                    return Err(
                        "--until needs a condition: ci-pass, approved, mergeable, or merged".into(),
                    );
                }
                for word in words {
                    let condition = Condition::parse(word)?;
                    if !until.contains(&condition) {
                        until.push(condition);
                    }
                }
            }
            "--timeout" if watch => timeout = Some(parse_duration(&value("--timeout")?)?),
            "--until" | "--timeout" => {
                return Err(format!("{flag} applies to `watch --pr`"));
            }
            "--changed" | "--stale" | "--pages" => {
                return Err(format!(
                    "{flag} applies to `mine` and `review`, not `watch`"
                ))
            }
            "--interval" | "--events" => {
                return Err(format!("{flag} applies to `watch`"));
            }
            other if other.starts_with('-') => return Err(format!("unknown option: {other}")),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }

    if pr.is_some() && (scope.is_some() || watched || authored) {
        return Err(
            "--pr follows one pull request; drop --repo, --all-repos, --watched, and --authored"
                .into(),
        );
    }

    if (!until.is_empty() || timeout.is_some()) && pr.is_none() {
        return Err("--until and --timeout need --pr OWNER/NAME#N".into());
    }
    if !until.is_empty() && max_events.is_some() {
        return Err("--until and --events both end the watch; pass one".into());
    }

    if watch {
        let format = match format.as_deref() {
            None => None,
            Some("text") => Some(EventFormat::Text),
            Some("json") | Some("jsonl") | Some("ndjson") => Some(EventFormat::Json),
            Some(other) => return Err(format!("unknown watch format: {other} (use text or json)")),
        };
        Ok(Command::Watch(WatchArgs {
            mode,
            scope,
            authored,
            pr,
            until,
            timeout,
            format,
            interval_secs,
            max_events,
            watched,
            snoozed,
            no_color,
        }))
    } else {
        let format = match format.as_deref() {
            None => None,
            Some("table") => Some(Format::Table),
            Some("markdown") | Some("md") => Some(Format::Markdown),
            Some("json") => Some(Format::Json),
            Some(other) => {
                return Err(format!(
                    "unknown format: {other} (use table, markdown, or json)"
                ))
            }
        };
        Ok(Command::View(ViewArgs {
            mode,
            scope,
            authored,
            format,
            changed,
            watched,
            stale,
            snoozed,
            pages: pages.unwrap_or(1),
            no_color,
        }))
    }
}

fn parse_skill(mut tokens: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut install = false;
    let mut dir = None;
    let mut agent = None;
    let mut force = false;
    while let Some(token) = tokens.next() {
        match token.as_str() {
            "install" if !install => install = true,
            "--dir" => {
                let value = tokens.next().ok_or("--dir needs a value")?;
                dir = Some(PathBuf::from(value));
            }
            "--agent" => {
                let value = tokens.next().ok_or("--agent needs a value")?;
                agent = Some(Agent::parse(&value)?);
            }
            "--force" => force = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option for `skill`: {other}"))
            }
            other => {
                return Err(format!(
                    "unknown skill action: {other} (use `skill` or `skill install`)"
                ))
            }
        }
    }
    if !install && (dir.is_some() || agent.is_some() || force) {
        return Err("--agent, --dir, and --force apply to `skill install`".into());
    }
    if dir.is_some() && agent.is_some() {
        return Err("pass --agent or --dir, not both".into());
    }
    Ok(Command::Skill(if install {
        SkillAction::Install { dir, agent, force }
    } else {
        SkillAction::Show
    }))
}

fn parse_completions(mut tokens: impl Iterator<Item = String>) -> Result<Command, String> {
    let shell = tokens
        .next()
        .ok_or("completions needs a shell: bash, zsh, or fish")?;
    if let Some(extra) = tokens.next() {
        return Err(format!("unexpected argument: {extra}"));
    }
    Ok(Command::Completions(Shell::parse(&shell)?))
}

fn parse_mode(word: &str) -> Result<Mode, String> {
    match word {
        "mine" | "authored" => Ok(Mode::Authored),
        "review" | "reviews" => Ok(Mode::Review),
        other => Err(format!(
            "unknown command: {other} (use mine, review, watch, skill, or completions)"
        )),
    }
}

fn parse_repo(value: &str) -> Result<String, String> {
    let repo = value.trim();
    match repo.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
            Ok(repo.to_owned())
        }
        _ => Err(format!("--repo needs OWNER/NAME, got: {value}")),
    }
}

/// `owner/name#123` or `https://github.com/owner/name/pull/123[/…]`.
fn parse_pr(value: &str) -> Result<PrRef, String> {
    let invalid = || format!("--pr needs OWNER/NAME#NUMBER or a pull request URL, got: {value}");
    let value = value.trim();
    let (repo, number) = match value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    {
        Some(rest) => {
            let parts: Vec<&str> = rest.split(['/', '?', '#']).collect();
            match parts.as_slice() {
                [_host, owner, name, "pull", number, ..] => (format!("{owner}/{name}"), *number),
                _ => return Err(invalid()),
            }
        }
        None => {
            let (repo, number) = value.split_once('#').ok_or_else(invalid)?;
            (repo.to_owned(), number)
        }
    };
    let number = number
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(invalid)?;
    let repo = parse_repo(&repo).map_err(|_| invalid())?;
    Ok(PrRef { repo, number })
}

fn parse_pages(value: &str) -> Result<u8, String> {
    value
        .parse::<u8>()
        .ok()
        .filter(|n| (1..=MAX_PAGES_PER_ALIAS).contains(n))
        .ok_or_else(|| format!("--pages must be 1-{MAX_PAGES_PER_ALIAS}, got: {value}"))
}

fn parse_interval(value: &str) -> Result<u64, String> {
    let secs = value
        .parse::<u64>()
        .map_err(|_| format!("--interval needs whole seconds, got: {value}"))?;
    if secs < MIN_REFRESH_SECS {
        return Err(format!(
            "--interval must be at least {MIN_REFRESH_SECS} seconds; PR Marmot shares your GitHub API budget"
        ));
    }
    Ok(secs)
}

fn parse_events(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("--events needs a positive number, got: {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(line: &str) -> Result<Command, String> {
        parse(line.split_whitespace().map(str::to_owned))
    }

    fn view(line: &str) -> ViewArgs {
        match parse_str(line).unwrap() {
            Command::View(args) => args,
            other => panic!("expected a view, got {other:?}"),
        }
    }

    fn watch(line: &str) -> WatchArgs {
        match parse_str(line).unwrap() {
            Command::Watch(args) => args,
            other => panic!("expected watch, got {other:?}"),
        }
    }

    #[test]
    fn views_default_to_one_page_and_automatic_format() {
        let args = view("mine");
        assert_eq!(args.mode, Mode::Authored);
        assert_eq!(args.scope, None);
        assert_eq!(args.format, None);
        assert_eq!(args.pages, 1);
        assert_eq!(view("review").mode, Mode::Review);
        assert_eq!(view("authored").mode, Mode::Authored);
    }

    #[test]
    fn scope_format_and_filters_parse_in_any_order() {
        let args =
            view("review --json --changed --repo acme/api --watched --pages=3 --snoozed --stale");
        assert_eq!(args.scope, Some(BoardScope::Repository("acme/api".into())));
        assert_eq!(args.format, Some(Format::Json));
        assert!(args.changed && args.watched && args.snoozed && args.stale);
        assert_eq!(args.pages, 3);
        assert_eq!(
            view("mine --repo acme/api --all-repos").scope,
            Some(BoardScope::AllRepositories)
        );
        assert_eq!(view("mine -f md").format, Some(Format::Markdown));
    }

    #[test]
    fn authored_narrows_mine_and_watch_but_not_review() {
        assert!(!view("mine --all-repos").authored);
        assert!(view("mine --all-repos --authored").authored);
        assert!(watch("watch mine --authored --all-repos").authored);
        assert!(watch("watch --authored").authored);
        for line in ["review --authored", "watch review --authored"] {
            assert_eq!(
                parse_str(line).unwrap_err(),
                "--authored applies to `mine`, not `review`"
            );
        }
    }

    #[test]
    fn pr_accepts_a_reference_or_url_and_stands_alone() {
        let expected = Some(PrRef {
            repo: "acme/api".into(),
            number: 12,
        });
        assert_eq!(watch("watch --pr acme/api#12").pr, expected);
        assert_eq!(
            watch("watch --pr https://github.com/acme/api/pull/12/files?w=1").pr,
            expected
        );
        for bad in [
            "acme#12",
            "acme/api#0",
            "acme/api#x",
            "https://github.com/acme/api/issues/12",
        ] {
            assert!(
                parse_str(&format!("watch --pr {bad}"))
                    .unwrap_err()
                    .starts_with("--pr needs"),
                "{bad}"
            );
        }
        assert_eq!(
            parse_str("mine --pr acme/api#12").unwrap_err(),
            "--pr applies to `watch`"
        );
        assert!(parse_str("watch --pr acme/api#12 --repo acme/api").is_err());
        assert!(parse_str("watch --pr acme/api#12 --watched").is_err());
    }

    #[test]
    fn until_and_timeout_wait_on_one_pr() {
        let args = watch(
            "watch --pr acme/api#12 --until ci-pass,approved --until merged --until=ci_pass --timeout 1h30m",
        );
        assert_eq!(
            args.until,
            [Condition::CiPass, Condition::Approved, Condition::Merged]
        );
        assert_eq!(args.timeout, Some(Duration::from_secs(5400)));
        let args = watch("watch --pr acme/api#12");
        assert!(args.until.is_empty() && args.timeout.is_none());
        // A deadline alone bounds a plain follow.
        assert_eq!(
            watch("watch --pr acme/api#12 --timeout 90").timeout,
            Some(Duration::from_secs(90))
        );
        for (line, needle) in [
            ("watch --until ci-pass", "need --pr"),
            ("watch mine --timeout 30m", "need --pr"),
            ("mine --until ci-pass", "applies to `watch --pr`"),
            ("review --timeout 5m", "applies to `watch --pr`"),
            (
                "watch --pr acme/api#12 --until green",
                "unknown --until condition: green",
            ),
            ("watch --pr acme/api#12 --until", "needs a value"),
            (
                "watch --pr acme/api#12 --until=,",
                "--until needs a condition",
            ),
            (
                "watch --pr acme/api#12 --timeout 0",
                "--timeout needs a duration",
            ),
            (
                "watch --pr acme/api#12 --timeout soon",
                "--timeout needs a duration",
            ),
            (
                "watch --pr acme/api#12 --until merged --events 1",
                "both end the watch",
            ),
        ] {
            let error = parse_str(line).unwrap_err();
            assert!(error.contains(needle), "{line}: {error}");
        }
    }

    #[test]
    fn watch_takes_an_optional_mode_and_its_own_options() {
        let args = watch("watch");
        assert_eq!(args.mode, Mode::Authored);
        let args = watch("watch review --json --interval 60 --events 1 --watched");
        assert_eq!(args.mode, Mode::Review);
        assert_eq!(args.format, Some(EventFormat::Json));
        assert_eq!(args.interval_secs, Some(60));
        assert_eq!(args.max_events, Some(1));
        assert!(args.watched);
        assert_eq!(watch("watch --format text").format, Some(EventFormat::Text));
    }

    #[test]
    fn skill_shows_by_default_and_installs_with_options() {
        assert_eq!(
            parse_str("skill").unwrap(),
            Command::Skill(SkillAction::Show)
        );
        assert_eq!(
            parse_str("skill install").unwrap(),
            Command::Skill(SkillAction::Install {
                dir: None,
                agent: None,
                force: false
            })
        );
        assert_eq!(
            parse_str("skill install --dir=/tmp/skills --force").unwrap(),
            Command::Skill(SkillAction::Install {
                dir: Some(PathBuf::from("/tmp/skills")),
                agent: None,
                force: true
            })
        );
        assert_eq!(
            parse_str("skill install --agent codex").unwrap(),
            Command::Skill(SkillAction::Install {
                dir: None,
                agent: Some(Agent::Agents),
                force: false
            })
        );
        assert_eq!(
            parse_str("skill install --agent all --dir /tmp/x").unwrap_err(),
            "pass --agent or --dir, not both"
        );
    }

    #[test]
    fn completions_take_exactly_one_known_shell() {
        assert_eq!(
            parse_str("completions fish").unwrap(),
            Command::Completions(Shell::Fish)
        );
        assert_eq!(
            parse_str("completions").unwrap_err(),
            "completions needs a shell: bash, zsh, or fish"
        );
        assert!(parse_str("completions tcsh")
            .unwrap_err()
            .contains("unknown shell"));
        assert!(parse_str("completions zsh bash")
            .unwrap_err()
            .contains("unexpected argument"));
    }

    #[test]
    fn help_and_version_win_anywhere() {
        assert_eq!(parse_str("").unwrap(), Command::Help);
        assert_eq!(parse_str("help").unwrap(), Command::Help);
        assert_eq!(parse_str("mine --bogus --help").unwrap(), Command::Help);
        assert_eq!(parse_str("watch -V").unwrap(), Command::Version);
    }

    #[test]
    fn invalid_input_is_explained() {
        for (line, needle) in [
            ("list", "unknown command"),
            ("mine --repo acme", "OWNER/NAME"),
            ("mine --repo acme/api/extra", "OWNER/NAME"),
            ("mine --pages 6", "--pages must be 1-5"),
            ("mine --pages 0", "--pages must be 1-5"),
            ("mine --format yaml", "unknown format"),
            ("mine --interval 60", "applies to `watch`"),
            ("watch --changed", "not `watch`"),
            ("watch --interval 10", "at least 30"),
            ("watch --events 0", "positive"),
            ("watch --format table", "unknown watch format"),
            ("mine --repo", "needs a value"),
            ("mine extra", "unexpected argument"),
            ("skill uninstall", "unknown skill action"),
            ("skill --force", "apply to `skill install`"),
            ("skill --agent all", "apply to `skill install`"),
            ("skill install --agent vim", "unknown agent: vim"),
            ("skill install --json", "unknown option for `skill`"),
            ("skill install --dir", "needs a value"),
        ] {
            let error = parse_str(line).unwrap_err();
            assert!(error.contains(needle), "{line}: {error}");
        }
    }
}
