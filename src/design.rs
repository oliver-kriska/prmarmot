//! PR Marmot's visual language — Guise/Mantine-inspired token overrides on top
//! of gpui-component's shadcn defaults, plus the layout constants the views
//! share. Spec: `.claude/research/2026-07-24-visual-design.md`.
//!
//! `refine_theme` must run after every mode change (i.e. at the end of
//! `ThemePref::apply`), because `Theme::change` / `sync_system_appearance`
//! reset `theme.colors` to the built-in palette.

use std::time::Duration;

use gpui::{px, App, Hsla};
use gpui_component::theme::Theme;

/// Table row height in px — matches gpui-component `Size::Small`
/// (`Table::small()`), the Finder-like density the spec targets.
/// Documentation of what `.small()` provides, not consumed directly.
#[allow(dead_code)]
pub const ROW_HEIGHT: f32 = 30.0;
/// Horizontal cell padding in px at `Size::Small`, each side (vertical is
/// 3 px): what `.small()` provides, which the table subtracts when eliding.
pub const CELL_PAD_X: f32 = 6.0;
/// Header bar horizontal padding in px (`.px_4()`). Vertical is owned by
/// `TitleBar` (fixed 34 px row) since the header moved into the titlebar.
pub const HEADER_PAD_X: f32 = 16.0;
/// Keyboard-hint footer vertical padding in px (horizontal = HEADER_PAD_X).
pub const FOOTER_PAD_Y: f32 = 6.0;
/// Label-chip height in px (11 px medium text inside).
pub const CHIP_HEIGHT: f32 = 18.0;
/// Label-chip horizontal padding in px.
pub const CHIP_PAD_X: f32 = 6.0;
/// Label-chip corner radius in px (Guise `radius.sm`).
pub const CHIP_RADIUS: f32 = 4.0;
/// Diameter in px of the status dot that replaces emoji glyphs.
pub const STATUS_DOT: f32 = 7.0;
/// Table cell text size in px (native macOS table size; base UI text is 14).
pub const TABLE_TEXT_PX: f32 = 13.0;

/// The type scale: every text size a view sets is one of these, so the
/// scale changes in one place.
pub mod type_size {
    use gpui::{px, Pixels};

    /// Chips, keycaps and other small labels.
    pub const CAPTION: Pixels = px(11.);
    /// Secondary lines: the footer, column heads, hints, field labels.
    pub const SMALL: Pixels = px(12.);
    /// Body text, the table's own size ([`super::TABLE_TEXT_PX`]).
    pub const BODY: Pixels = px(super::TABLE_TEXT_PX);
    /// A screen's title.
    pub const TITLE: Pixels = px(18.);
    /// The app's name at the top of Settings.
    pub const DISPLAY: Pixels = px(20.);
    /// The sign-in code, read off the screen and typed on GitHub.
    pub const CODE: Pixels = px(24.);
}

/// `0xRRGGBB` -> opaque theme color.
fn c(hex: u32) -> Hsla {
    gpui::rgb(hex).into()
}

/// Tabular figures (`tnum`): every digit the same width, so a count or a
/// "synced 3m ago" that changes doesn't nudge the text beside it.
pub fn tabular_numbers() -> gpui::FontFeatures {
    gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".to_owned(), 1)]))
}

/// `0xRRGGBB` + alpha (0.0–1.0) -> translucent theme color.
fn ca(hex: u32, alpha: f32) -> Hsla {
    let mut color = c(hex);
    color.a = alpha;
    color
}

/// The text and surface tokens of one appearance mode, as `0xRRGGBB`, so a
/// test can hold every text colour against every surface it is drawn on
/// (`contrast` below). The hover/active/border/translucent tokens stay in
/// [`refine_theme`]; none of them carries text.
pub(crate) struct Palette {
    pub background: u32,
    pub foreground: u32,
    pub muted: u32,
    pub muted_foreground: u32,
    pub secondary: u32,
    pub secondary_foreground: u32,
    pub accent: u32,
    pub accent_foreground: u32,
    pub title_bar: u32,
    pub tab_bar_segmented: u32,
    pub tab_foreground: u32,
    pub tab_active: u32,
    pub tab_active_foreground: u32,
    pub primary: u32,
    pub primary_hover: u32,
    pub primary_active: u32,
    pub primary_foreground: u32,
    pub link: u32,
    pub danger: u32,
    pub warning: u32,
    pub success: u32,
    pub table_even: u32,
    pub table_head: u32,
    pub table_head_foreground: u32,
    pub table_hover: u32,
    pub popover: u32,
    pub popover_foreground: u32,
}

/// Mantine dark ramp, open-color blue, open-color status hues.
pub(crate) const DARK: Palette = Palette {
    background: 0x1A1B1E,
    foreground: 0xC1C2C5,
    muted: 0x25262B,
    muted_foreground: 0x909296,
    secondary: 0x25262B,
    secondary_foreground: 0xC1C2C5,
    accent: 0x25262B,
    accent_foreground: 0x74C0FC,
    title_bar: 0x141517,
    tab_bar_segmented: 0x25262B,
    tab_foreground: 0x909296,
    tab_active: 0x1A1B1E,
    tab_active_foreground: 0xC1C2C5,
    primary: 0x1971C2,
    primary_hover: 0x1864AB,
    primary_active: 0x1864AB,
    primary_foreground: 0xFFFFFF,
    link: 0x4DABF7,
    danger: 0xFF6B6B,
    warning: 0xFAB005,
    success: 0x40C057,
    table_even: 0x1E1F23,
    table_head: 0x141517,
    table_head_foreground: 0x909296,
    table_hover: 0x25262B,
    popover: 0x25262B,
    popover_foreground: 0xC1C2C5,
};

/// Open-color gray ramp on white; status hues from Primer where open-color has no AA step. Grays and the blue fill sit one step darker than the ramp's text defaults so they also pass on the zebra, hover, chip and header surfaces (2026-10-02 contrast test).
pub(crate) const LIGHT: Palette = Palette {
    background: 0xFFFFFF,
    foreground: 0x212529,
    muted: 0xF1F3F5,
    muted_foreground: 0x626A72,
    secondary: 0xF1F3F5,
    secondary_foreground: 0x495057,
    accent: 0xF1F3F5,
    accent_foreground: 0x1971C2,
    title_bar: 0xF8F9FA,
    tab_bar_segmented: 0xE9ECEF,
    tab_foreground: 0x626A72,
    tab_active: 0xFFFFFF,
    tab_active_foreground: 0x212529,
    primary: 0x1971C2,
    primary_hover: 0x1864AB,
    primary_active: 0x1864AB,
    primary_foreground: 0xFFFFFF,
    link: 0x1971C2,
    danger: 0xC92A2A,
    warning: 0x8F5F00,
    success: 0x1A7F37,
    table_even: 0xF8F9FA,
    table_head: 0xF8F9FA,
    table_head_foreground: 0x626A72,
    table_hover: 0xF1F3F5,
    popover: 0xFFFFFF,
    popover_foreground: 0x212529,
};

/// Overwrite the active palette with PR Marmot's Guise-derived tokens.
/// Neutrals: Mantine dark ramp / open-color gray. Accent: open-color blue.
/// Status hues are tuned as *text* colors (they render as dots + short text,
/// never filled slabs); every text token meets WCAG AA (4.5:1) on every
/// surface it is drawn on — asserted by the `contrast` test below.
pub fn refine_theme(cx: &mut App) {
    let theme = Theme::global_mut(cx);

    // Mode-independent: 14 px base UI text, small native-feeling radii.
    theme.font_size = px(14.);
    theme.radius = px(4.);
    theme.radius_lg = px(8.);

    let dark = theme.mode.is_dark();
    let t = &mut theme.colors;

    let p = if dark { &DARK } else { &LIGHT };
    if dark {
        // Surfaces & text — Mantine dark ramp (dark.0–dark.9).
        t.background = c(p.background);
        t.foreground = c(p.foreground);
        t.border = c(0x2C2E33);
        t.input = c(0x2C2E33);
        t.muted = c(p.muted);
        t.muted_foreground = c(p.muted_foreground);
        t.secondary = c(p.secondary);
        t.secondary_hover = c(0x2C2E33);
        t.secondary_active = c(0x373A40);
        t.secondary_foreground = c(p.secondary_foreground);
        t.accent = c(p.accent);
        t.accent_foreground = c(p.accent_foreground); // issue-id tag: link-adjacent blue.3

        // Chrome sits one step darker than content (Zed/macOS dark idiom).
        t.title_bar = c(p.title_bar);
        t.title_bar_border = c(0x2C2E33);
        // Primary board-scope control: muted track, body-colored selected
        // segment, and a clear text hierarchy in both appearance modes.
        t.tab_bar_segmented = c(p.tab_bar_segmented);
        t.tab_foreground = c(p.tab_foreground);
        t.tab_active = c(p.tab_active);
        t.tab_active_foreground = c(p.tab_active_foreground);

        // The one accent: open-color blue.
        t.primary = c(p.primary);
        t.primary_hover = c(p.primary_hover);
        t.primary_active = c(p.primary_active);
        t.primary_foreground = c(p.primary_foreground);
        t.link = c(p.link);
        t.link_hover = c(0x74C0FC);
        t.link_active = c(0x339AF0);
        t.ring = c(0x4DABF7);
        t.caret = c(0xC1C2C5);
        t.selection = ca(0x228BE6, 0.35);

        // Status hues — open-color red.5 / yellow.6 / green.6.
        t.danger = c(p.danger);
        t.danger_hover = c(0xFF8787);
        t.danger_active = c(0xFA5252);
        t.danger_foreground = c(0x1A1B1E);
        t.warning = c(p.warning);
        t.warning_hover = c(0xFCC419);
        t.warning_active = c(0xF59F00);
        t.warning_foreground = c(0x1A1B1E);
        t.success = c(p.success);
        t.success_hover = c(0x51CF66);
        t.success_active = c(0x37B24D);
        t.success_foreground = c(0x1A1B1E);

        // Table: subtle zebra carries row tracking; hairlines near-invisible.
        t.table = c(0x1A1B1E);
        t.table_even = c(p.table_even);
        t.table_head = c(p.table_head);
        t.table_head_foreground = c(p.table_head_foreground);
        t.table_hover = c(p.table_hover);
        t.table_active = ca(0x228BE6, 0.26);
        t.table_active_border = ca(0x228BE6, 0.60);
        t.table_row_border = ca(0xFFFFFF, 0.04);
        t.list = c(0x1A1B1E);
        t.list_even = c(0x1E1F23);
        t.list_head = c(0x141517);
        t.list_hover = c(0x25262B);
        t.list_active = ca(0x228BE6, 0.18);
        t.list_active_border = ca(0x228BE6, 0.60);

        // Tooltips on the raised-surface layer; translucent mac scrollbars.
        t.popover = c(p.popover);
        t.popover_foreground = c(p.popover_foreground);
        t.scrollbar = ca(0x000000, 0.0);
        t.scrollbar_thumb = ca(0x5C5F66, 0.7);
        t.scrollbar_thumb_hover = c(0x5C5F66);
    } else {
        // Surfaces & text — open-color gray ramp on true white.
        t.background = c(p.background);
        t.foreground = c(p.foreground);
        t.border = c(0xDEE2E6);
        t.input = c(0xCED4DA);
        t.muted = c(p.muted);
        // gray.6 #868e96 fails AA on white (3.3:1); this midpoint passes.
        t.muted_foreground = c(p.muted_foreground);
        t.secondary = c(p.secondary);
        t.secondary_hover = c(0xE9ECEF);
        t.secondary_active = c(0xDEE2E6);
        t.secondary_foreground = c(p.secondary_foreground);
        t.accent = c(p.accent);
        t.accent_foreground = c(p.accent_foreground);

        t.title_bar = c(p.title_bar);
        t.title_bar_border = c(0xDEE2E6);
        t.tab_bar_segmented = c(p.tab_bar_segmented);
        t.tab_foreground = c(p.tab_foreground);
        t.tab_active = c(p.tab_active);
        t.tab_active_foreground = c(p.tab_active_foreground);

        // Accent: blue.7 for fills, blue.8 for text (blue.7 is 4.2:1 — fails).
        t.primary = c(p.primary);
        t.primary_hover = c(p.primary_hover);
        t.primary_active = c(p.primary_active);
        t.primary_foreground = c(p.primary_foreground);
        t.link = c(p.link);
        t.link_hover = c(0x1864AB);
        t.link_active = c(0x1864AB);
        t.ring = c(0x228BE6);
        t.caret = c(0x212529);
        t.selection = ca(0x228BE6, 0.25);

        // Status: red.9; warning/success borrow Primer's AA-on-white values
        // because open-color's yellow/green ramps have no AA step on white.
        t.danger = c(p.danger);
        t.danger_hover = c(0xE03131);
        t.danger_active = c(0xB02525);
        t.danger_foreground = c(0xFFFFFF);
        t.warning = c(p.warning);
        t.warning_hover = c(0xB08000);
        t.warning_active = c(0x7D5400);
        t.warning_foreground = c(0xFFFFFF);
        t.success = c(p.success);
        t.success_hover = c(0x2F9E44);
        t.success_active = c(0x166B2E);
        t.success_foreground = c(0xFFFFFF);

        t.table = c(0xFFFFFF);
        t.table_even = c(p.table_even);
        t.table_head = c(p.table_head);
        t.table_head_foreground = c(p.table_head_foreground);
        t.table_hover = c(p.table_hover);
        t.table_active = ca(0x228BE6, 0.16);
        t.table_active_border = ca(0x228BE6, 0.50);
        t.table_row_border = ca(0x000000, 0.05);
        t.list = c(0xFFFFFF);
        t.list_even = c(0xF8F9FA);
        t.list_head = c(0xF8F9FA);
        t.list_hover = c(0xF1F3F5);
        t.list_active = ca(0x228BE6, 0.10);
        t.list_active_border = ca(0x228BE6, 0.50);

        t.popover = c(p.popover);
        t.popover_foreground = c(p.popover_foreground);
        t.scrollbar = ca(0xFFFFFF, 0.0);
        t.scrollbar_thumb = ca(0xADB5BD, 0.9);
        t.scrollbar_thumb_hover = c(0x868E96);
    }

    still_scrollbars(cx);
}

/// Scrollbars still show while you scroll and hide when idle, as macOS
/// does, but snap rather than fade: gpui-component animates the fade at the
/// display's rate, ~80 frames after every scroll or arrow key, which an
/// otherwise idle board does not need (the calm rule; 2026-10-01 audit C9).
fn still_scrollbars(cx: &mut App) {
    let base = gpui_base::Theme::global_mut(cx);
    let motion = base
        .scrollbar
        .motion()
        .with_enter(Duration::ZERO)
        .with_exit(Duration::ZERO)
        .with_expand(Duration::ZERO);
    base.scrollbar = base.scrollbar.clone().with_motion(motion);
}

#[cfg(test)]
mod contrast {
    use super::*;

    /// WCAG 2.x relative luminance of an `0xRRGGBB` colour.
    fn luminance(hex: u32) -> f64 {
        let channel = |shift: u32| {
            let c = f64::from((hex >> shift) & 0xFF) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    fn ratio(a: u32, b: u32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// Every (text, surface) pair the app draws, by token name. Body text,
    /// muted text and the status hues sit on the page, on zebra and hovered
    /// rows; muted text also on chips, the title bar and the table header;
    /// the rest are each widget's own text on its own fill.
    fn pairs(p: &Palette) -> Vec<(&'static str, u32, &'static str, u32)> {
        let mut pairs = Vec::new();
        for (text_name, text) in [
            ("foreground", p.foreground),
            ("muted_foreground", p.muted_foreground),
            ("link", p.link),
            ("danger", p.danger),
            ("warning", p.warning),
            ("success", p.success),
        ] {
            for (surface_name, surface) in [
                ("background", p.background),
                ("table_even", p.table_even),
                ("table_hover", p.table_hover),
            ] {
                pairs.push((text_name, text, surface_name, surface));
            }
        }
        for (surface_name, surface) in [
            ("muted", p.muted),
            ("secondary", p.secondary),
            ("title_bar", p.title_bar),
            ("table_head", p.table_head),
        ] {
            pairs.push((
                "muted_foreground",
                p.muted_foreground,
                surface_name,
                surface,
            ));
        }
        pairs.extend([
            (
                "secondary_foreground",
                p.secondary_foreground,
                "secondary",
                p.secondary,
            ),
            ("accent_foreground", p.accent_foreground, "accent", p.accent),
            (
                "tab_foreground",
                p.tab_foreground,
                "tab_bar_segmented",
                p.tab_bar_segmented,
            ),
            (
                "tab_active_foreground",
                p.tab_active_foreground,
                "tab_active",
                p.tab_active,
            ),
            (
                "table_head_foreground",
                p.table_head_foreground,
                "table_head",
                p.table_head,
            ),
            (
                "popover_foreground",
                p.popover_foreground,
                "popover",
                p.popover,
            ),
            (
                "primary_foreground",
                p.primary_foreground,
                "primary",
                p.primary,
            ),
            (
                "primary_foreground",
                p.primary_foreground,
                "primary_hover",
                p.primary_hover,
            ),
            (
                "primary_foreground",
                p.primary_foreground,
                "primary_active",
                p.primary_active,
            ),
        ]);
        pairs
    }

    #[test]
    fn every_text_token_meets_wcag_aa_on_every_surface_it_is_drawn_on() {
        let mut failures = Vec::new();
        for (mode, palette) in [("dark", &DARK), ("light", &LIGHT)] {
            for (text_name, text, surface_name, surface) in pairs(palette) {
                let r = ratio(text, surface);
                if r < 4.5 {
                    failures.push(format!(
                        "{mode}: {text_name} #{text:06X} on {surface_name} #{surface:06X} is {r:.2}:1"
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "below WCAG AA (4.5:1):\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn the_luminance_formula_is_wcag_s() {
        assert!((ratio(0xFFFFFF, 0x000000) - 21.0).abs() < 1e-9);
        assert!((ratio(0x6C757D, 0xFFFFFF) - 4.69).abs() < 0.01);
    }
}
