//! GitHub's enum values, parsed once where a response is read so the rest of
//! core matches on variants instead of comparing strings. Each keeps a value
//! it does not know as it came (`Other`), and writes every value back exactly
//! as GitHub spells it, so the attention file, the CLI's JSON and the golden
//! fixtures read the same as when these were strings — and a value GitHub adds
//! later round-trips unchanged rather than failing the whole response.

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! github_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal,)+ }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
            /// A value this version does not know, kept as GitHub sent it.
            Other(String),
        }

        impl $name {
            /// GitHub's spelling.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $text,)+
                    Self::Other(text) => text,
                }
            }

            pub fn parse(text: &str) -> Self {
                match text {
                    $($text => Self::$variant,)+
                    other => Self::Other(other.to_owned()),
                }
            }
        }

        impl From<&str> for $name {
            fn from(text: &str) -> Self {
                Self::parse(text)
            }
        }

        impl From<String> for $name {
            fn from(text: String) -> Self {
                Self::parse(&text)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        /// By GitHub's spelling, as the strings these replaced sorted, so a
        /// sorted list in a saved file still compares equal.
        impl Ord for $name {
            fn cmp(&self, other: &Self) -> Ordering {
                self.as_str().cmp(other.as_str())
            }
        }

        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer).map(Self::from)
            }
        }
    };
}

github_enum! {
    /// One review's state (`PullRequestReviewState`), and the viewer's
    /// standing review on a row.
    ReviewVerdict {
        Approved => "APPROVED",
        ChangesRequested => "CHANGES_REQUESTED",
        Commented => "COMMENTED",
        Dismissed => "DISMISSED",
        Pending => "PENDING",
        /// The viewer has no standing review: the prototype's `"NONE"`.
        NoReview => "NONE",
    }
}

impl ReviewVerdict {
    /// A review that says where the reviewer stands — approved, commented or
    /// requested changes — as opposed to a dismissed, pending or absent one.
    pub fn is_standing(&self) -> bool {
        matches!(
            self,
            Self::Approved | Self::Commented | Self::ChangesRequested
        )
    }
}

github_enum! {
    /// The PR's overall review decision (`PullRequestReviewDecision`).
    ReviewDecision {
        Approved => "APPROVED",
        ChangesRequested => "CHANGES_REQUESTED",
        ReviewRequired => "REVIEW_REQUIRED",
    }
}

github_enum! {
    /// Whether the PR merges cleanly (`MergeableState`).
    Mergeable {
        Mergeable => "MERGEABLE",
        Conflicting => "CONFLICTING",
        /// GitHub has not finished computing it.
        Unknown => "UNKNOWN",
    }
}

github_enum! {
    /// Open, closed or merged (`PullRequestState`).
    PrState {
        Open => "OPEN",
        Closed => "CLOSED",
        Merged => "MERGED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_value_reads_and_writes_as_github_spells_it() {
        for text in [
            "APPROVED",
            "CHANGES_REQUESTED",
            "COMMENTED",
            "DISMISSED",
            "PENDING",
            "NONE",
            "SOMETHING_NEW",
        ] {
            let verdict: ReviewVerdict = serde_json::from_str(&format!("\"{text}\"")).unwrap();
            assert_eq!(
                serde_json::to_string(&verdict).unwrap(),
                format!("\"{text}\"")
            );
            assert_eq!(verdict.as_str(), text);
        }
        assert_eq!(
            ReviewVerdict::parse("SOMETHING_NEW"),
            ReviewVerdict::Other("SOMETHING_NEW".into())
        );
        assert_eq!(
            ReviewDecision::parse("REVIEW_REQUIRED"),
            ReviewDecision::ReviewRequired
        );
        assert_eq!(Mergeable::parse("CONFLICTING"), Mergeable::Conflicting);
        assert_eq!(PrState::parse("MERGED"), PrState::Merged);
    }

    #[test]
    fn order_is_the_spelling_order_the_strings_had() {
        let mut verdicts = [
            ReviewVerdict::Pending,
            ReviewVerdict::Approved,
            ReviewVerdict::Other("ZZZ".into()),
            ReviewVerdict::Commented,
            ReviewVerdict::ChangesRequested,
        ];
        verdicts.sort();
        let spelled: Vec<_> = verdicts.iter().map(ReviewVerdict::as_str).collect();
        assert_eq!(
            spelled,
            [
                "APPROVED",
                "CHANGES_REQUESTED",
                "COMMENTED",
                "PENDING",
                "ZZZ"
            ]
        );
    }
}
