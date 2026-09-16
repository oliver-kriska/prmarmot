//! PR Marmot — a GitHub PR review dashboard as a native desktop app.
//! Step-0 walking skeleton: one window, one repo, the authored view.

mod app;
mod assets;
mod attention_state;
mod config;
mod design;
mod notification_help;
mod platform;
mod settings;
mod state;
mod table;
mod theme;
pub mod updates;

use std::sync::Arc;

use gpui::{
    px, size, App, AppContext, KeyBinding, Menu, MenuItem, OsAction, WindowBounds, WindowKind,
    WindowOptions,
};
use prmarmot_core::board::{BoardConfig, BoardScope, Mode};
use prmarmot_core::github::gh_cli::GhCliTransport;

use crate::app::RootView;
use crate::state::{AppState, AttentionPreferences, GhLoginResolver};

const RELEASE_REPO: &str = "oliver-kriska/prmarmot";
const CASK_TOKEN: &str = "prmarmot";
const APPLICATION_NAME: &str = "prmarmot";

gpui::actions!(prmarmot, [Quit]);

fn install_app_menu(cx: &mut App) {
    let macos = cfg!(target_os = "macos");
    cx.bind_keys([
        KeyBinding::new(if macos { "cmd-q" } else { "ctrl-q" }, Quit, None),
        // Same as `/`. Global, and bound after gpui-component's own
        // find-in-input key, so it also wins inside the search field. Inputs
        // outside the board (dialogs) have no handler and fall through.
        KeyBinding::new(
            if macos { "cmd-f" } else { "ctrl-f" },
            app::FocusSearch,
            None,
        ),
    ]);
    cx.on_action(|_: &Quit, cx: &mut App| {
        // Application quit does not call the individual window-close callback.
        for handle in cx.windows() {
            let _ = handle.update(cx, |_, window, _| app::save_window_size(window));
        }
        cx.quit();
    });
    use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    cx.set_menus(vec![
        Menu {
            name: "PR Marmot".into(),
            items: vec![MenuItem::action("Quit PR Marmot", Quit)],
            disabled: false,
        },
        // The standard editing items act on the focused text field (search,
        // repository picker, Settings); Find is the same as `/`.
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::action("Find", app::FocusSearch),
            ],
            disabled: false,
        },
    ]);
}

const USAGE: &str = "usage: prmarmot [--repo owner/name | --all-repos] [--review]

Scope resolution: CLI, then $PRMARMOT_REPO/$PRMARMOT_SCOPE, then `scope` + `repo`
in ~/.config/prmarmot/config.toml. A clean config opens All repositories.
Authentication is checked inside the window; setup failures are retryable.

Config file (~/.config/prmarmot/config.toml): repo, repos = [..] for the
repo picker, refresh_secs, theme, view, default_reviewers, [repo_reviewers],
[issue_link],
[window], automatic_update_checks. Repo/theme/view/window-size changes made
in-app are saved back.
Env vars override the file:
  PRMARMOT_REPO                owner/name
  PRMARMOT_SCOPE               all | repo
  PRMARMOT_REFRESH_SECS        refresh interval (default 300, floor 30)
  PRMARMOT_THEME               system | light | dark (default system; `t` cycles)
  PRMARMOT_ISSUE_PATTERN       e.g. PROJ-[0-9]+
  PRMARMOT_ISSUE_URL_TEMPLATE  e.g. https://tracker.example.com/issues/{id}
  PRMARMOT_DEFAULT_REVIEWERS   comma-separated logins for the no-reviewer note
                               (replaces default_reviewers; [repo_reviewers] still apply)";

fn parse_args() -> Result<(Option<BoardScope>, Option<Mode>), String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(
    args: impl IntoIterator<Item = String>,
) -> Result<(Option<BoardScope>, Option<Mode>), String> {
    let mut scope = None;
    let mut mode = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--repo" => {
                scope = Some(BoardScope::Repository(
                    args.next().ok_or("--repo needs owner/name")?,
                ))
            }
            "--all-repos" => scope = Some(BoardScope::AllRepositories),
            "--review" => mode = Some(Mode::Review),
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown arg: {other}\n\n{USAGE}")),
        }
    }
    Ok((scope, mode))
}

fn board_config(file: &config::FileConfig) -> BoardConfig {
    let (config, warning) = prmarmot_local::config::board_config(file);
    if let Some(warning) = warning {
        eprintln!("prmarmot: {warning}");
    }
    config
}

fn main() {
    // The staged helper is this same executable. Dispatch its fixed protocol
    // before normal CLI parsing or any GPUI/platform initialization.
    match updates::HelperInvocation::parse_cli_args(std::env::args_os().skip(1)) {
        Ok(Some(invocation)) => {
            if let Err(error) = updates::run_upgrade_helper(
                &invocation,
                &updates::SystemCommandRunner,
                &updates::SystemParentWaiter,
            ) {
                eprintln!("prmarmot update helper: {error}");
                std::process::exit(1);
            }
            return;
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("prmarmot update helper: {error}");
            std::process::exit(2);
        }
    }

    let (scope_arg, mode_arg) = match parse_args() {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    if let Err(error) = config::migrate_legacy_data() {
        eprintln!("prmarmot: could not migrate existing data: {error}");
        std::process::exit(1);
    }
    let file = config::load();
    // --review > persisted `view` in config > authored.
    let mode = mode_arg.unwrap_or(match file.view.as_deref() {
        Some("review") => Mode::Review,
        _ => Mode::Authored,
    });

    let scope = config::resolve_scope(
        scope_arg,
        std::env::var("PRMARMOT_REPO").ok(),
        std::env::var("PRMARMOT_SCOPE").ok().as_deref(),
        &file,
    );
    let config = board_config(&file);

    // Repo-picker entries: config list with the active repo always present.
    let mut repos = file.repos.clone();
    let pinned_repos = config::normalized_pins(&file.pinned_repos);
    for pin in &pinned_repos {
        if !repos.contains(pin) {
            repos.push(pin.clone());
        }
    }
    if let BoardScope::Repository(repo) = &scope {
        if !repos.contains(repo) {
            repos.insert(0, repo.clone());
        }
    }
    let update_paths = config::update_paths();
    let launch = app::Launch {
        theme: crate::theme::ThemePref::resolve(file.theme.as_deref()),
        refresh: crate::state::refresh_interval(file.refresh_secs),
        repos,
        pinned_repos,
        automatic_update_checks: file.automatic_update_checks,
        update_failure: consume_update_failure(&update_paths),
        update_paths,
    };
    let attention_preferences = AttentionPreferences {
        notifications: file.notifications,
        notification_sound: file.notification_sound,
        notify_all_needs_action: file.notify_all_needs_action,
        dock_badge: file.dock_badge,
    };
    let window_pref = file.window;

    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            cx.activate(true);
            install_app_menu(cx);

            // Persisted size from the last session, else the default that fits
            // the authored column set (~1430px — at 1280 the Note column, the
            // product, was clipped by the viewport edge).
            let win_size = match &window_pref {
                Some(w) => size(px(w.width.max(900.)), px(w.height.max(560.))),
                None => size(px(1440.), px(860.)),
            };
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(win_size, cx)),
                // Transparent native chrome: the app's own header row IS the
                // titlebar (traffic lights overlay it), like Zed/modern Mac apps.
                titlebar: Some(gpui_component::TitleBar::title_bar_options()),
                window_min_size: Some(size(px(900.), px(560.))),
                kind: WindowKind::Normal,
                app_id: Some("dev.oliverkriska.prmarmot".into()),
                ..Default::default()
            };

            cx.open_window(options, |window, cx| {
                let state = cx.new(|_| {
                    AppState::new(
                        scope,
                        mode,
                        config,
                        Arc::new(GhCliTransport::new()),
                        Arc::new(GhLoginResolver),
                        attention_preferences,
                    )
                });
                let view = cx.new(|cx| RootView::new(state, launch, window, cx));
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            })
            .expect("failed to open window");
        });
}

fn consume_update_failure(paths: &config::UpdatePaths) -> Option<String> {
    let receipt = match updates::load_upgrade_receipt(&paths.upgrade_receipt) {
        Ok(receipt) => receipt,
        Err(error) => {
            eprintln!("prmarmot: could not read update receipt: {error}");
            None
        }
    };
    if let Err(error) = std::fs::remove_file(&paths.upgrade_receipt) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!("prmarmot: could not consume update receipt: {error}");
        }
    }
    receipt
        .filter(|receipt| !receipt.upgrade_succeeded || !receipt.reopen_succeeded)
        .map(|receipt| receipt.message)
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn cli_scope_is_explicit_and_last_flag_wins() {
        assert_eq!(
            parse_args_from(["--all-repos".to_owned()]).unwrap().0,
            Some(BoardScope::AllRepositories)
        );
        assert_eq!(
            parse_args_from([
                "--all-repos".to_owned(),
                "--repo".to_owned(),
                "acme/widgets".to_owned(),
            ])
            .unwrap()
            .0,
            Some(BoardScope::Repository("acme/widgets".into()))
        );
    }

    #[test]
    fn update_receipts_are_consumed_and_only_failures_are_shown() {
        let root =
            std::env::temp_dir().join(format!("prmarmot-receipt-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let paths = config::UpdatePaths {
            check_state: root.join("updates.toml"),
            upgrade_receipt: root.join("receipt.toml"),
            helper_lock: root.join("helper.lock"),
        };

        std::fs::write(
            &paths.upgrade_receipt,
            "completed_at_unix = 1\nupgrade_succeeded = true\nreopen_succeeded = true\nmessage = 'done'\n",
        )
        .unwrap();
        assert_eq!(consume_update_failure(&paths), None);
        assert!(!paths.upgrade_receipt.exists());

        std::fs::write(
            &paths.upgrade_receipt,
            "completed_at_unix = 2\nupgrade_succeeded = false\nreopen_succeeded = true\nmessage = 'brew failed'\n",
        )
        .unwrap();
        assert_eq!(
            consume_update_failure(&paths).as_deref(),
            Some("brew failed")
        );
        assert!(!paths.upgrade_receipt.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
