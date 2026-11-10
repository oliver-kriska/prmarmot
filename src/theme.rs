//! Theme preference: dark / light / follow-system. gpui-component owns the
//! base palettes; this decides which mode is active, keeps it in sync with
//! macOS appearance changes while in System mode, and layers the look on
//! top in a fixed order: colours, then geometry, then skin.

use gpui::{App, Window};
use gpui_component::theme::{Theme, ThemeMode, ThemeRegistry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePref {
    System,
    Light,
    Dark,
}

impl ThemePref {
    /// `PRMARMOT_THEME` > config-file `theme` > system.
    pub fn resolve(config_theme: Option<&str>) -> Self {
        let pref = prmarmot_local::config::EnvOverrides::current()
            .theme
            .or_else(|| config_theme.map(str::to_string));
        match pref.as_deref() {
            Some("light") => ThemePref::Light,
            Some("dark") => ThemePref::Dark,
            _ => ThemePref::System,
        }
    }

    /// Cycle order for the `t` key.
    pub fn next(self) -> Self {
        match self {
            ThemePref::System => ThemePref::Light,
            ThemePref::Light => ThemePref::Dark,
            ThemePref::Dark => ThemePref::System,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemePref::System => "system",
            ThemePref::Light => "light",
            ThemePref::Dark => "dark",
        }
    }

    pub fn apply(self, window: &mut Window, cx: &mut App) {
        // Native material must follow the same explicit appearance as the
        // content. `None` resumes the OS preference (one window per app).
        cx.set_window_appearance(match self {
            ThemePref::System => None,
            ThemePref::Light => Some(gpui::WindowAppearance::Light),
            ThemePref::Dark => Some(gpui::WindowAppearance::Dark),
        });
        // A user theme applied last time was stored as the mode's base;
        // start every change from the framework's own defaults so a switch
        // back to a built-in set leaves none of it behind.
        if cx.has_global::<Theme>() {
            let (light, dark) = {
                let registry = ThemeRegistry::global(cx);
                (
                    registry.default_light_theme().clone(),
                    registry.default_dark_theme().clone(),
                )
            };
            let theme = Theme::global_mut(cx);
            theme.light_theme = light;
            theme.dark_theme = dark;
        }
        match self {
            ThemePref::System => {
                // On macOS the window's appearance notification is deferred;
                // after clearing an override, read the application instead.
                #[cfg(target_os = "macos")]
                Theme::change(cx.window_appearance(), Some(window), cx);
                #[cfg(not(target_os = "macos"))]
                Theme::sync_system_appearance(Some(window), cx);
            }
            ThemePref::Light => Theme::change(ThemeMode::Light, Some(window), cx),
            ThemePref::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        }
        // Every change/sync above resets colors to the built-in palette;
        // layer the look on top: colours, geometry, skin.
        let dark = Theme::global(cx).mode.is_dark();
        crate::platform::set_app_icon(dark);
        let font_family = Theme::global(cx).font_family.clone();
        let (skin, user, built_in) = {
            let look = crate::skin::look(cx);
            let user = look.user_theme(dark).map(|(_, config)| config.clone());
            (look.skin, user, crate::design::built_in(&look.colors))
        };
        // A built-in set is solid colours; a theme file's config may carry
        // gradients, which `apply_config` resolved into the tokens.
        let from_file = user.is_some();
        match (user, built_in) {
            (Some(config), _) => {
                Theme::global_mut(cx).apply_config(&config);
                // A theme file may name a font or size; the type is ours.
                Theme::global_mut(cx).font_family = font_family;
            }
            (None, Some(set)) => {
                crate::design::apply_palette(cx, set.palette(dark));
                crate::design::apply_primary_buttons(cx, set.palette(dark));
            }
            (None, None) => {
                let palette = crate::design::CLASSIC.palette(dark);
                crate::design::apply_palette(cx, palette);
                crate::design::apply_primary_buttons(cx, palette);
            }
        }
        crate::design::apply_geometry(cx, skin);
        crate::design::apply_skin(cx, skin);
        // The table paints its body, header band and selection from the
        // theme's tokens, not its colours. A built-in set projects every
        // token from its palette (critique 2026-10-21: Classic kept the
        // framework's, so its dark table body painted #101010 under #1B1D1E
        // chrome); a theme file keeps the tokens `apply_config` resolved —
        // a gradient would flatten to a solid — and takes only the
        // selection fill the skin wrote.
        let theme = Theme::global_mut(cx);
        if from_file {
            theme.tokens.table_active = theme.colors.table_active.into();
        } else {
            theme.tokens = theme.colors.into();
        }
        // The framework projected its Base layer (scrollbar radius and
        // tints, text-view defaults) inside `change`/`sync_system_appearance`,
        // before the look was written; project it again from the look.
        Theme::sync_base(cx);
        crate::design::still_scrollbars(cx);
        crate::skin::Chrome::sync(window, cx);
    }
}
