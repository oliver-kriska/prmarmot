//! Theme preference: dark / light / follow-system. gpui-component owns the
//! palettes; this just decides which mode is active and keeps it in sync
//! with macOS appearance changes while in System mode.

use gpui::{App, Window};
use gpui_component::theme::{Theme, ThemeMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePref {
    System,
    Light,
    Dark,
}

impl ThemePref {
    /// `PRMARMOT_THEME` > config-file `theme` > system.
    pub fn resolve(config_theme: Option<&str>) -> Self {
        let pref = std::env::var("PRMARMOT_THEME")
            .ok()
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
        match self {
            ThemePref::System => Theme::sync_system_appearance(Some(window), cx),
            ThemePref::Light => Theme::change(ThemeMode::Light, Some(window), cx),
            ThemePref::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        }
        // Every change/sync above resets colors to the built-in palette;
        // re-apply PR Marmot's own tokens on top.
        crate::design::refine_theme(cx);
    }
}
