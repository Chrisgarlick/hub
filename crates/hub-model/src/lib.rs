//! Hub's published types (spec sections 4–6A): what other apps and MCP
//! clients read. Plain types with no I/O.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a rule asks for (spec section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Must,
    MustNot,
    Prefer,
    Avoid,
}

impl RuleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Must => "must",
            Self::MustNot => "must_not",
            Self::Prefer => "prefer",
            Self::Avoid => "avoid",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "must" => Self::Must,
            "must_not" => Self::MustNot,
            "prefer" => Self::Prefer,
            "avoid" => Self::Avoid,
            _ => return None,
        })
    }

    /// `must` and `must_not` with a check are hard: a match is a violation.
    /// `prefer` and `avoid` with a check are flagged as advice.
    pub fn is_strict(self) -> bool {
        matches!(self, Self::Must | Self::MustNot)
    }
}

/// The apps a rule or decision can apply to.
pub const TARGETS: [&str; 5] = ["all", "board", "design", "motion", "typeset"];

/// A deterministic check: code decides, not Claude.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Words or phrases that mustn't appear (whole words).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terms: Vec<String>,
    /// A regular expression that mustn't match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Words or phrases that must appear (for a `must`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_terms: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub case_sensitive: bool,
}

impl Check {
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.pattern.is_none() && self.required_terms.is_empty()
    }
}

/// How far to rely on something, from facts Hub knows (spec section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    /// Approved, or written or approved by you.
    Authoritative,
    /// Proposed, or written by Claude and not yet reviewed.
    Tentative,
    /// Superseded, retired, rejected, or overridden by a later decision.
    Outdated,
}

/// Where a check found something, in the text that was checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    /// Byte offsets into the text.
    pub start: usize,
    pub end: usize,
    /// 1-based line, and column in characters.
    pub line: usize,
    pub column: usize,
    /// The text that matched.
    pub text: String,
}
