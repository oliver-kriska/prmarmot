//! prboard's visual language — Guise/Mantine-inspired token overrides on top
//! of gpui-component's shadcn defaults, plus the layout constants the views
//! share. Spec: `.claude/research/2026-07-24-visual-design.md`.
//!
//! `refine_theme` must run after every mode change (i.e. at the end of
//! `ThemePref::apply`), because `Theme::change` / `sync_system_appearance`
//! reset `theme.colors` to the built-in palette.

use gpui::{px, App, Hsla};
use gpui_component::theme::Theme;

/// Table row height in px — matches gpui-component `Size::Small`
/// (`Table::small()`), the Finder-like density the spec targets.
/// Documentation of what `.small()` provides, not consumed directly.
#[allow(dead_code)]
pub const ROW_HEIGHT: f32 = 30.0;
/// Horizontal cell padding in px at `Size::Small` (vertical is 3 px).
/// Documentation of what `.small()` provides, not consumed directly.
#[allow(dead_code)]
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

/// `0xRRGGBB` -> opaque theme color.
fn c(hex: u32) -> Hsla {
    gpui::rgb(hex).into()
}

/// `0xRRGGBB` + alpha (0.0–1.0) -> translucent theme color.
fn ca(hex: u32, alpha: f32) -> Hsla {
    let mut color = c(hex);
    color.a = alpha;
    color
}

/// Overwrite the active palette with prboard's Guise-derived tokens.
/// Neutrals: Mantine dark ramp / open-color gray. Accent: open-color blue.
/// Status hues are tuned as *text* colors (they render as dots + short text,
/// never filled slabs); every text token meets WCAG AA on its background —
/// ratios are documented in the spec.
pub fn refine_theme(cx: &mut App) {
    let theme = Theme::global_mut(cx);

    // Mode-independent: 14 px base UI text, small native-feeling radii.
    theme.font_size = px(14.);
    theme.radius = px(4.);
    theme.radius_lg = px(8.);

    let dark = theme.mode.is_dark();
    let t = &mut theme.colors;

    if dark {
        // Surfaces & text — Mantine dark ramp (dark.0–dark.9).
        t.background = c(0x1A1B1E);
        t.foreground = c(0xC1C2C5);
        t.border = c(0x2C2E33);
        t.input = c(0x2C2E33);
        t.muted = c(0x25262B);
        t.muted_foreground = c(0x909296);
        t.secondary = c(0x25262B);
        t.secondary_hover = c(0x2C2E33);
        t.secondary_active = c(0x373A40);
        t.secondary_foreground = c(0xC1C2C5);
        t.accent = c(0x25262B);
        t.accent_foreground = c(0x74C0FC); // issue-id tag: link-adjacent blue.3

        // Chrome sits one step darker than content (Zed/macOS dark idiom).
        t.title_bar = c(0x141517);
        t.title_bar_border = c(0x2C2E33);
        // Primary board-scope control: muted track, body-colored selected
        // segment, and a clear text hierarchy in both appearance modes.
        t.tab_bar_segmented = c(0x25262B);
        t.tab_foreground = c(0x909296);
        t.tab_active = c(0x1A1B1E);
        t.tab_active_foreground = c(0xC1C2C5);

        // The one accent: open-color blue.
        t.primary = c(0x228BE6);
        t.primary_hover = c(0x339AF0);
        t.primary_active = c(0x1C7ED6);
        t.primary_foreground = c(0xFFFFFF);
        t.link = c(0x4DABF7);
        t.link_hover = c(0x74C0FC);
        t.link_active = c(0x339AF0);
        t.ring = c(0x4DABF7);
        t.caret = c(0xC1C2C5);
        t.selection = ca(0x228BE6, 0.35);

        // Status hues — open-color red.5 / yellow.6 / green.6.
        t.danger = c(0xFF6B6B);
        t.danger_hover = c(0xFF8787);
        t.danger_active = c(0xFA5252);
        t.danger_foreground = c(0x1A1B1E);
        t.warning = c(0xFAB005);
        t.warning_hover = c(0xFCC419);
        t.warning_active = c(0xF59F00);
        t.warning_foreground = c(0x1A1B1E);
        t.success = c(0x40C057);
        t.success_hover = c(0x51CF66);
        t.success_active = c(0x37B24D);
        t.success_foreground = c(0x1A1B1E);

        // Table: subtle zebra carries row tracking; hairlines near-invisible.
        t.table = c(0x1A1B1E);
        t.table_even = c(0x1E1F23);
        t.table_head = c(0x141517);
        t.table_head_foreground = c(0x909296);
        t.table_hover = c(0x25262B);
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
        t.popover = c(0x25262B);
        t.popover_foreground = c(0xC1C2C5);
        t.scrollbar = ca(0x000000, 0.0);
        t.scrollbar_thumb = ca(0x5C5F66, 0.7);
        t.scrollbar_thumb_hover = c(0x5C5F66);
    } else {
        // Surfaces & text — open-color gray ramp on true white.
        t.background = c(0xFFFFFF);
        t.foreground = c(0x212529);
        t.border = c(0xDEE2E6);
        t.input = c(0xCED4DA);
        t.muted = c(0xF1F3F5);
        // gray.6 #868e96 fails AA on white (3.3:1); this midpoint passes.
        t.muted_foreground = c(0x6C757D);
        t.secondary = c(0xF1F3F5);
        t.secondary_hover = c(0xE9ECEF);
        t.secondary_active = c(0xDEE2E6);
        t.secondary_foreground = c(0x495057);
        t.accent = c(0xF1F3F5);
        t.accent_foreground = c(0x1971C2);

        t.title_bar = c(0xF8F9FA);
        t.title_bar_border = c(0xDEE2E6);
        t.tab_bar_segmented = c(0xE9ECEF);
        t.tab_foreground = c(0x6C757D);
        t.tab_active = c(0xFFFFFF);
        t.tab_active_foreground = c(0x212529);

        // Accent: blue.7 for fills, blue.8 for text (blue.7 is 4.2:1 — fails).
        t.primary = c(0x1C7ED6);
        t.primary_hover = c(0x1971C2);
        t.primary_active = c(0x1864AB);
        t.primary_foreground = c(0xFFFFFF);
        t.link = c(0x1971C2);
        t.link_hover = c(0x1864AB);
        t.link_active = c(0x1864AB);
        t.ring = c(0x228BE6);
        t.caret = c(0x212529);
        t.selection = ca(0x228BE6, 0.25);

        // Status: red.9; warning/success borrow Primer's AA-on-white values
        // because open-color's yellow/green ramps have no AA step on white.
        t.danger = c(0xC92A2A);
        t.danger_hover = c(0xE03131);
        t.danger_active = c(0xB02525);
        t.danger_foreground = c(0xFFFFFF);
        t.warning = c(0x9A6700);
        t.warning_hover = c(0xB08000);
        t.warning_active = c(0x7D5400);
        t.warning_foreground = c(0xFFFFFF);
        t.success = c(0x1A7F37);
        t.success_hover = c(0x2F9E44);
        t.success_active = c(0x166B2E);
        t.success_foreground = c(0xFFFFFF);

        t.table = c(0xFFFFFF);
        t.table_even = c(0xF8F9FA);
        t.table_head = c(0xF8F9FA);
        t.table_head_foreground = c(0x6C757D);
        t.table_hover = c(0xF1F3F5);
        t.table_active = ca(0x228BE6, 0.16);
        t.table_active_border = ca(0x228BE6, 0.50);
        t.table_row_border = ca(0x000000, 0.05);
        t.list = c(0xFFFFFF);
        t.list_even = c(0xF8F9FA);
        t.list_head = c(0xF8F9FA);
        t.list_hover = c(0xF1F3F5);
        t.list_active = ca(0x228BE6, 0.10);
        t.list_active_border = ca(0x228BE6, 0.50);

        t.popover = c(0xFFFFFF);
        t.popover_foreground = c(0x212529);
        t.scrollbar = ca(0xFFFFFF, 0.0);
        t.scrollbar_thumb = ca(0xADB5BD, 0.9);
        t.scrollbar_thumb_hover = c(0x868E96);
    }
}
