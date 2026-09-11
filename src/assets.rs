//! Embedded asset source: the handful of Lucide icons (ISC license) that
//! gpui-component widgets request at runtime (e.g. the Select chevron).
//! Everything is compiled into the binary — no bundle-relative lookups, so
//! the same binary works from a terminal and from a Spotlight-launched .app.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub struct Assets;

const ICONS: &[(&str, &[u8])] = &[
    (
        "icons/binoculars.svg",
        include_bytes!("../assets/icons/binoculars.svg"),
    ),
    (
        "icons/close.svg",
        include_bytes!("../assets/icons/close.svg"),
    ),
    (
        "icons/chevron-down.svg",
        include_bytes!("../assets/icons/chevron-down.svg"),
    ),
    (
        "icons/chevron-right.svg",
        include_bytes!("../assets/icons/chevron-right.svg"),
    ),
    (
        "icons/check.svg",
        include_bytes!("../assets/icons/check.svg"),
    ),
    (
        "icons/inbox.svg",
        include_bytes!("../assets/icons/inbox.svg"),
    ),
    (
        "icons/search.svg",
        include_bytes!("../assets/icons/search.svg"),
    ),
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_close_icon_is_embedded_at_the_framework_path() {
        use gpui_component::{IconName, IconNamed};
        let bytes = Assets
            .load(&IconName::Close.path())
            .unwrap()
            .expect("missing dialog close icon");
        assert!(std::str::from_utf8(&bytes).unwrap().contains("<svg"));
    }

    #[test]
    fn settings_disclosure_icons_are_embedded() {
        for path in ["icons/chevron-right.svg", "icons/chevron-down.svg"] {
            let bytes = Assets.load(path).unwrap().expect("missing disclosure icon");
            assert!(std::str::from_utf8(&bytes).unwrap().contains("<svg"));
        }
    }
}
