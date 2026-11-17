//! Repository scope for learnings and other per-repository state.
//!
//! A [`RepoScope`] is the lowercased `owner/repo` pair of a pull
//! request's repository. It deliberately carries no forge provider, so
//! a repository mirrored between Forgejo and GitHub under the same
//! `owner/repo` shares its learnings.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Lowercased `owner/repo` scope of a repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoScope {
    owner: String,
    repo: String,
}

impl RepoScope {
    /// Build a scope, lowercasing both parts so lookups are
    /// case-insensitive the way forge owner and repository names are.
    pub fn new(owner: impl AsRef<str>, repo: impl AsRef<str>) -> Self {
        Self {
            owner: owner.as_ref().to_lowercase(),
            repo: repo.as_ref().to_lowercase(),
        }
    }

    /// Parse the `owner/repo` form produced by [`fmt::Display`]. Returns
    /// `None` when either part is empty or the owner contains a slash.
    pub fn parse(value: &str) -> Option<Self> {
        let (owner, repo) = value.split_once('/')?;
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        Some(Self::new(owner, repo))
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }
}

impl fmt::Display for RepoScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

impl From<RepoScope> for String {
    fn from(scope: RepoScope) -> Self {
        scope.to_string()
    }
}

impl TryFrom<String> for RepoScope {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value).ok_or_else(|| format!("invalid repository scope: {value}"))
    }
}

/// Scope of a legacy review-correction learning, read from its
/// `Repository <owner>/<repo> only.` text prefix.
///
/// Equivalent to the regex `^Repository ([^/\s]+)/([^\s]+) only\.`.
/// Before learnings carried a repository column, review corrections
/// embedded their repository in this prefix; stores use it to scope
/// such rows. Any other text is unscoped and yields `None`.
pub fn parse_legacy_correction_repo(text: &str) -> Option<RepoScope> {
    let rest = text.strip_prefix("Repository ")?;
    let token_end = rest.find(char::is_whitespace)?;
    let (token, after) = rest.split_at(token_end);
    if !after.starts_with(" only.") {
        return None;
    }
    let (owner, repo) = token.split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(RepoScope::new(owner, repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_lowercases_owner_and_repo() {
        assert_eq!(
            RepoScope::new("Acme", "Widget"),
            RepoScope::new("acme", "widget")
        );
    }

    #[test]
    fn displays_as_owner_slash_repo() {
        assert_eq!(RepoScope::new("Acme", "Widget").to_string(), "acme/widget");
    }

    #[test]
    fn parse_round_trips_display_form() {
        assert_eq!(
            RepoScope::parse("Acme/Widget"),
            Some(RepoScope::new("acme", "widget"))
        );
        assert_eq!(RepoScope::parse("acme"), None);
        assert_eq!(RepoScope::parse("/widget"), None);
    }

    #[test]
    fn legacy_correction_prefix_yields_lowercased_scope() {
        assert_eq!(
            parse_legacy_correction_repo(
                "Repository Acme/Widget only. Review correction from PR feedback: x"
            ),
            Some(RepoScope::new("acme", "widget"))
        );
    }

    #[test]
    fn text_without_prefix_is_unscoped() {
        assert_eq!(
            parse_legacy_correction_repo("Prefer Result over panics."),
            None
        );
    }

    #[test]
    fn prefix_in_the_middle_of_text_is_unscoped() {
        assert_eq!(
            parse_legacy_correction_repo("Note: Repository acme/widget only. Something"),
            None
        );
    }

    #[test]
    fn prefix_without_slash_is_unscoped() {
        assert_eq!(
            parse_legacy_correction_repo("Repository acme only. Something"),
            None
        );
    }
}
