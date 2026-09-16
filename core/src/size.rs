//! Size band: a coarse, deterministic read of how big a PR's change is,
//! shared by the app and the CLI. It comes from GitHub's `additions`,
//! `deletions`, and `changedFiles`; changed lines are additions plus
//! deletions.
//!
//! - Small: at most 100 changed lines and at most 10 files.
//! - Large: more than 400 changed lines or more than 30 files.
//! - Medium: everything else.
//! - No band when GitHub did not report the counts.
//!
//! The thresholds follow Google's code review guidance ("100 lines is usually
//! a reasonable size for a CL, and 1000 lines is usually too large", and a
//! change spread across many files is larger than its line count;
//! <https://google.github.io/eng-practices/review/developer/small-cls.html>)
//! and the SmartBear/Cisco code review study, where reviewers find fewer
//! defects beyond 200–400 lines at a time
//! (<https://smartbear.com/learn/code-review/best-practices-for-peer-code-review/>).
//! Lockfiles and generated files count like any other change, because the
//! board query has no per-file data to discount them. A band is never turned
//! into a time estimate.

/// Most changed lines and files a Small change may have.
pub const SMALL_MAX_LINES: u64 = 100;
pub const SMALL_MAX_FILES: u64 = 10;
/// A change with more changed lines or files than this is Large.
pub const LARGE_OVER_LINES: u64 = 400;
pub const LARGE_OVER_FILES: u64 = 30;

/// GitHub's change counts for one PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeSize {
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
}

impl ChangeSize {
    /// `None` unless GitHub reported all three counts.
    pub fn from_counts(
        additions: Option<u64>,
        deletions: Option<u64>,
        changed_files: Option<u64>,
    ) -> Option<Self> {
        Some(Self {
            additions: additions?,
            deletions: deletions?,
            changed_files: changed_files?,
        })
    }

    /// Changed lines: additions plus deletions.
    pub fn lines(self) -> u64 {
        self.additions.saturating_add(self.deletions)
    }

    pub fn band(self) -> SizeBand {
        let (lines, files) = (self.lines(), self.changed_files);
        if lines > LARGE_OVER_LINES || files > LARGE_OVER_FILES {
            SizeBand::Large
        } else if lines <= SMALL_MAX_LINES && files <= SMALL_MAX_FILES {
            SizeBand::Small
        } else {
            SizeBand::Medium
        }
    }

    /// "42 changed lines in 3 files".
    pub fn lines_and_files(self) -> String {
        let (lines, files) = (self.lines(), self.changed_files);
        format!(
            "{lines} changed line{} in {files} file{}",
            if lines == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" },
        )
    }
}

/// The size band of a change, smallest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SizeBand {
    Small,
    Medium,
    Large,
}

impl SizeBand {
    /// Stable machine key for JSON output.
    pub fn key(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    /// The one place the displayed band names live.
    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(additions: u64, deletions: u64, changed_files: u64) -> SizeBand {
        ChangeSize {
            additions,
            deletions,
            changed_files,
        }
        .band()
    }

    #[test]
    fn bands_follow_the_documented_thresholds() {
        assert_eq!(band(0, 0, 0), SizeBand::Small);
        assert_eq!(band(60, 40, 10), SizeBand::Small);
        assert_eq!(band(61, 40, 1), SizeBand::Medium, "101 lines");
        assert_eq!(band(5, 0, 11), SizeBand::Medium, "11 files");
        assert_eq!(band(300, 100, 30), SizeBand::Medium, "400 lines, 30 files");
        assert_eq!(band(300, 101, 1), SizeBand::Large, "401 lines");
        assert_eq!(band(10, 10, 31), SizeBand::Large, "31 files");
        assert_eq!(band(u64::MAX, 1, 1), SizeBand::Large, "saturates");
    }

    #[test]
    fn no_band_without_every_count() {
        assert_eq!(ChangeSize::from_counts(Some(1), Some(2), None), None);
        assert_eq!(ChangeSize::from_counts(None, Some(2), Some(1)), None);
        assert_eq!(
            ChangeSize::from_counts(Some(1), Some(2), Some(1)).map(ChangeSize::lines),
            Some(3)
        );
    }

    #[test]
    fn describes_lines_and_files() {
        let size = |additions, deletions, changed_files| ChangeSize {
            additions,
            deletions,
            changed_files,
        };
        assert_eq!(
            size(30, 12, 3).lines_and_files(),
            "42 changed lines in 3 files"
        );
        assert_eq!(size(1, 0, 1).lines_and_files(), "1 changed line in 1 file");
        assert_eq!(SizeBand::Medium.key(), "medium");
        assert_eq!(SizeBand::Large.label(), "Large");
        assert!(SizeBand::Small < SizeBand::Medium && SizeBand::Medium < SizeBand::Large);
    }
}
