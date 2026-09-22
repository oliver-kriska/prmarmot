//! `prmarmot-cli`: PR Marmot's My PRs, Review queue, and All open for
//! terminals and coding agents. Same query, categorization, sections, and
//! watch/snooze state as the desktop app; no GPUI, no runtime, read-only.

mod args;
mod auth;
mod completions;
mod render;
mod skill;
mod term;
mod until;
mod view;
mod watch;

/// The published JSON Schemas, checked against output by unit tests too.
#[cfg(test)]
#[path = "../tests/support/schema.rs"]
mod schema_check;

use std::io::Write;
use std::process::ExitCode;

use chrono::Utc;
use prmarmot_core::board::{carry_forward_conflicts, Mode};
use prmarmot_core::github::GhError;
use prmarmot_local::config;
use prmarmot_local::session::Session;

use args::{Command, EventFormat, Format, SkillAction, ViewArgs, WatchArgs};
use term::Paint;
use view::Filters;

const EXIT_GITHUB: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_AUTH: u8 = 3;
const EXIT_RATE_LIMITED: u8 = 4;
/// `watch --until`: no condition asked for can be met any more.
const EXIT_UNMET: u8 = 5;
/// `watch --timeout` passed first.
const EXIT_TIMED_OUT: u8 = 6;

fn main() -> ExitCode {
    match args::parse(std::env::args().skip(1)) {
        Err(message) => {
            eprintln!("prmarmot-cli: {message}\nRun `prmarmot-cli --help` for usage.");
            ExitCode::from(EXIT_USAGE)
        }
        Ok(Command::Help) => print(args::USAGE),
        Ok(Command::Version) => print(&format!("prmarmot-cli {}", env!("CARGO_PKG_VERSION"))),
        Ok(Command::View(args)) => run_view(args),
        Ok(Command::Watch(args)) => run_watch(args),
        Ok(Command::Auth(action, options)) => run_auth(action, options),
        Ok(Command::Skill(SkillAction::Show)) => print(skill::SKILL_MD),
        Ok(Command::Completions(shell)) => print(shell.script()),
        Ok(Command::Skill(SkillAction::Install { dir, agent, force })) => {
            run_skill_install(dir, agent, force)
        }
    }
}

fn run_auth(action: auth::Action, options: auth::Options) -> ExitCode {
    match auth::run(action, options, env!("CARGO_PKG_VERSION")) {
        auth::Outcome::Ok => ExitCode::SUCCESS,
        auth::Outcome::NotSignedIn => ExitCode::from(EXIT_AUTH),
        auth::Outcome::Failed(error) => fail(&error),
    }
}

fn run_skill_install(
    dir: Option<std::path::PathBuf>,
    agent: Option<skill::Agent>,
    force: bool,
) -> ExitCode {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from);
    let targets = match dir {
        Some(dir) => Some(vec![(skill::expand_home(dir, home), "")]),
        None => skill::skills_dirs(
            agent.unwrap_or(skill::Agent::Claude),
            std::env::var_os("CLAUDE_CONFIG_DIR").map(Into::into),
            home,
        ),
    };
    let Some(targets) = targets else {
        eprintln!("prmarmot-cli: cannot find your home directory; pass --dir");
        return ExitCode::from(EXIT_GITHUB);
    };
    let mut failed = false;
    for (skills_dir, readers) in targets {
        match skill::install(&skills_dir, skill::SKILL_MD, force) {
            Ok((file, outcome)) => {
                let file = file.display();
                let name = skill::NAME;
                let line = match outcome {
                    skill::Outcome::Created => format!("Installed the {name} skill at {file}"),
                    skill::Outcome::Updated => format!("Updated the {name} skill at {file}"),
                    skill::Outcome::Unchanged => {
                        format!("The {name} skill at {file} is already up to date")
                    }
                };
                if readers.is_empty() {
                    print(&line);
                } else {
                    print(&format!("{line} (for {readers})"));
                }
            }
            Err(message) => {
                eprintln!("prmarmot-cli: {message}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::from(EXIT_GITHUB)
    } else {
        ExitCode::SUCCESS
    }
}

/// Write to stdout; a reader that went away (`| head`) is not an error.
fn print(text: &str) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    let text = text.strip_suffix('\n').unwrap_or(text);
    let _ = writeln!(stdout, "{text}").and_then(|_| stdout.flush());
    ExitCode::SUCCESS
}

fn exit_code_for(error: &GhError) -> u8 {
    match error {
        GhError::NotInstalled | GhError::NotAuthenticated => EXIT_AUTH,
        GhError::NeedsRepository => EXIT_USAGE,
        GhError::RateLimited { .. } => EXIT_RATE_LIMITED,
        _ => EXIT_GITHUB,
    }
}

fn fail(error: &GhError) -> ExitCode {
    eprintln!("prmarmot-cli: {error}");
    ExitCode::from(exit_code_for(error))
}

fn setup(
    cli_scope: Option<prmarmot_core::board::BoardScope>,
    cli_host: Option<&str>,
    cli_auth: Option<config::AuthMode>,
) -> view::Setup {
    let setup = view::setup(cli_scope, cli_host, cli_auth);
    for warning in &setup.warnings {
        eprintln!("prmarmot-cli: {warning}");
    }
    setup
}

fn connect(setup: &view::Setup) -> Result<(Session, String), GhError> {
    let session = view::connect(setup)?;
    let login = session.login()?;
    Ok((session, login))
}

fn resolve(
    cli_scope: Option<prmarmot_core::board::BoardScope>,
    cli_host: Option<&str>,
    cli_auth: Option<config::AuthMode>,
) -> Result<(view::Setup, Session, String), GhError> {
    let setup = setup(cli_scope, cli_host, cli_auth);
    let (session, login) = connect(&setup)?;
    Ok((setup, session, login))
}

fn run_view(args: ViewArgs) -> ExitCode {
    let mut setup = setup(args.scope.clone(), args.host.as_deref(), args.auth);
    // All open is one repository's PRs; say so before asking GitHub anything.
    if args.mode == Mode::AllOpen && setup.scope.is_all() {
        eprintln!("prmarmot-cli: {}", view::all_open_needs_repository());
        return ExitCode::from(EXIT_USAGE);
    }
    let (session, login) = match connect(&setup) {
        Ok(connected) => connected,
        Err(error) => return fail(&error),
    };
    setup.board.authored_only = args.authored;
    let filters = Filters {
        changed: args.changed,
        watched: args.watched,
        stale: args.stale,
        stale_after_days: setup.board.stale_after_days,
        query: args.filter.clone(),
    };
    let mut fetch = match view::fetch(
        session.transport(),
        args.mode,
        &setup.scope,
        &login,
        &setup.board,
        &filters.remote(args.mode),
        args.pages,
    ) {
        Ok(fetch) => fetch,
        Err(error) => return fail(&error),
    };
    let attention = view::attention(&setup.auth.host, &login);
    // GitHub reports mergeability as UNKNOWN while it recomputes; show the
    // conflict the app saw last instead of a momentarily clean PR.
    carry_forward_conflicts(
        &mut fetch.rows,
        |id| attention.snapshots.last_conflict(id),
        args.mode,
        &login,
        &setup.board,
    );
    let mut board = view::build(
        fetch,
        &attention,
        args.mode,
        setup.scope,
        login,
        filters,
        Utc::now(),
    );
    board.authored_only = setup.board.authored_only;
    board.sort = args.sort;
    board.sections = setup.sections.clone();
    let format = args.format.unwrap_or(if term::stdout_is_terminal() {
        Format::Table
    } else {
        Format::Markdown
    });
    let text = match format {
        Format::Json => serde_json::to_string_pretty(&render::board_json(&board))
            .expect("board JSON is always serializable"),
        Format::Markdown => render::markdown(&board, args.snoozed),
        Format::Table => render::table(
            &board,
            term::width(),
            Paint::new(term::color_enabled(args.no_color)),
            args.snoozed,
        ),
    };
    print(&text)
}

fn run_watch(args: WatchArgs) -> ExitCode {
    let (mut setup, session, login) =
        match resolve(args.scope.clone(), args.host.as_deref(), args.auth) {
            Ok(resolved) => resolved,
            Err(error) => return fail(&error),
        };
    setup.board.authored_only = args.authored;
    let interval = args
        .interval_secs
        .map(std::time::Duration::from_secs)
        .unwrap_or_else(|| config::refresh_interval(setup.file.refresh_secs));
    let output = match args.format.unwrap_or(if term::stdout_is_terminal() {
        EventFormat::Text
    } else {
        EventFormat::Json
    }) {
        EventFormat::Json => watch::Output::Json,
        EventFormat::Text => watch::Output::Text(Paint::new(term::color_enabled(args.no_color))),
    };
    let watch_session = watch::Session {
        transport: session.transport(),
        host: setup.auth.host.clone(),
        viewer: login,
        // A followed PR is classified as in Involving me, whatever the mode word.
        mode: if args.pr.is_some() {
            Mode::Authored
        } else {
            args.mode
        },
        scope: setup.scope,
        board: setup.board,
        interval,
        filter: watch::EventFilter {
            watched_only: args.watched,
            // A PR asked for by name is never hidden by its snooze.
            include_snoozed: args.snoozed || args.pr.is_some(),
        },
        pr: args.pr,
        until: args.until,
        timeout: args.timeout,
        max_events: args.max_events,
        output,
    };
    let mut stdout = std::io::stdout().lock();
    let stop = watch::run(watch_session, &mut stdout, &mut watch::SystemClock::start());
    match stop {
        watch::Stop::Failed(error) => fail(&error),
        stop => ExitCode::from(watch_exit_code(&stop)),
    }
}

fn watch_exit_code(stop: &watch::Stop) -> u8 {
    match stop {
        watch::Stop::Done | watch::Stop::Met => 0,
        watch::Stop::Failed(error) => exit_code_for(error),
        watch::Stop::Unmet => EXIT_UNMET,
        watch::Stop::TimedOut => EXIT_TIMED_OUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_the_app() {
        let manifest = include_str!("../../Cargo.toml");
        let app_version = manifest
            .lines()
            .skip_while(|line| line.trim() != "[package]")
            .find_map(|line| line.strip_prefix("version = "))
            .map(|value| value.trim().trim_matches('"'))
            .expect("root Cargo.toml has a package version");
        assert_eq!(env!("CARGO_PKG_VERSION"), app_version);
    }

    #[test]
    fn github_failures_map_to_documented_exit_codes() {
        assert_eq!(exit_code_for(&GhError::NotInstalled), EXIT_AUTH);
        assert_eq!(exit_code_for(&GhError::NotAuthenticated), EXIT_AUTH);
        assert_eq!(
            exit_code_for(&GhError::RateLimited { reset_epoch: None }),
            EXIT_RATE_LIMITED
        );
        assert_eq!(
            exit_code_for(&GhError::Network("timeout".into())),
            EXIT_GITHUB
        );
        assert_eq!(exit_code_for(&GhError::NeedsRepository), EXIT_USAGE);
        assert!(args::USAGE.contains("0 ok, 1 GitHub, network, or file error, 2 usage error"));
    }

    #[test]
    fn a_watch_ends_with_the_exit_code_its_help_documents() {
        assert_eq!(watch_exit_code(&watch::Stop::Done), 0);
        assert_eq!(watch_exit_code(&watch::Stop::Met), 0);
        assert_eq!(watch_exit_code(&watch::Stop::Unmet), 5);
        assert_eq!(watch_exit_code(&watch::Stop::TimedOut), 6);
        assert_eq!(
            watch_exit_code(&watch::Stop::Failed(GhError::NotAuthenticated)),
            EXIT_AUTH
        );
        assert!(args::USAGE.contains("5 --until can no longer be met"));
        assert!(args::USAGE.contains("6 --timeout reached"));
    }
}
