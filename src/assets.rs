//! Embedded asset source: the app mark and the handful of Lucide icons (ISC
//! license) that gpui-component widgets request at runtime (e.g. the Select chevron).
//! Everything is compiled into the binary — no bundle-relative lookups, so
//! the same binary works from a terminal and from a Spotlight-launched .app.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub struct Assets;

const ICONS: &[(&str, &[u8])] = &[
    (
        "branding/mascot.png",
        include_bytes!("../assets/branding/mascot.png"),
    ),
    (
        "icons/arrow-down.svg",
        include_bytes!("../assets/icons/arrow-down.svg"),
    ),
    (
        "icons/arrow-up.svg",
        include_bytes!("../assets/icons/arrow-up.svg"),
    ),
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
    fn mascot_is_embedded_at_retina_resolution() {
        let bytes = Assets
            .load("branding/mascot.png")
            .unwrap()
            .expect("missing mascot");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 192);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 212);
    }

    /// Every icon the app names must be embedded: a missing asset draws
    /// nothing, silently, so a button renders as an empty square. The names
    /// are read from the sources so a new `IconName::` use cannot be forgotten.
    #[test]
    fn every_icon_the_app_names_is_embedded_at_the_framework_path() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut names = icon_names_in(&src);
        // Requested by gpui-component widgets themselves, not by name in src/.
        names.extend(["Close", "ChevronDown"].map(String::from));
        names.sort();
        names.dedup();
        assert!(names.len() >= 5, "found only {names:?}");
        for name in names {
            let path = format!("icons/{}.svg", kebab(&name));
            let bytes = Assets
                .load(&path)
                .unwrap()
                .unwrap_or_else(|| panic!("IconName::{name} needs {path} in ICONS"));
            assert!(std::str::from_utf8(&bytes).unwrap().contains("<svg"));
        }
    }

    fn icon_names_in(dir: &std::path::Path) -> Vec<String> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                names.extend(icon_names_in(&path));
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                for (ix, _) in text.match_indices("IconName::") {
                    let rest = &text[ix + "IconName::".len()..];
                    let name: String = rest
                        .chars()
                        .take_while(char::is_ascii_alphanumeric)
                        .collect();
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
        }
        names
    }

    fn kebab(variant: &str) -> String {
        let mut out = String::new();
        for (ix, c) in variant.chars().enumerate() {
            if c.is_ascii_uppercase() && ix > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        }
        out
    }
}
