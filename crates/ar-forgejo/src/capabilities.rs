//! Map a Forgejo server version to the review features it supports.
//!
//! | Forgejo major | multi-line comments | suggestion blocks, thread replies, resolve threads |
//! |---|---|---|
//! | ≤15 | no | no |
//! | 16, 17 | yes | no |
//! | ≥18 | yes | yes |

use ar_forge::HostCapabilities;

/// First Forgejo major with multi-line review comments (PR #12582).
pub const MULTI_LINE_COMMENTS_MAJOR: u64 = 16;

/// First Forgejo major with suggestion blocks, thread replies and thread
/// resolve (PRs #13023, #14850, #14873).
pub const THREAD_FEATURES_MAJOR: u64 = 18;

/// Parse the major version from a Forgejo `GET /api/v1/version` string
/// such as `17.0.0-100-2254cb0+gitea-1.22.0` or `16.0.7`.
///
/// Returns `None` when the string does not start with a numeric major
/// component.
pub fn parse_major_version(version: &str) -> Option<u64> {
    let major = version
        .trim()
        .split(['.', '-', '+'])
        .next()
        .filter(|major| !major.is_empty() && major.bytes().all(|b| b.is_ascii_digit()))?;
    major.parse().ok()
}

/// Capabilities of a Forgejo server reporting `version`. An unparseable
/// version reports none.
pub fn capabilities_for_version(version: &str) -> Option<HostCapabilities> {
    let major = parse_major_version(version)?;
    let threads = major >= THREAD_FEATURES_MAJOR;
    Some(HostCapabilities {
        multi_line_comments: major >= MULTI_LINE_COMMENTS_MAJOR,
        suggestion_blocks: threads,
        thread_replies: threads,
        resolve_threads: threads,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_major_from_plain_release() {
        assert_eq!(parse_major_version("16.0.7"), Some(16));
    }

    #[test]
    fn parses_major_from_dev_build_with_gitea_suffix() {
        assert_eq!(
            parse_major_version("17.0.0-100-2254cb0+gitea-1.22.0"),
            Some(17)
        );
    }

    #[test]
    fn parses_bare_major() {
        assert_eq!(parse_major_version("18"), Some(18));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_major_version("not-a-version"), None);
    }

    #[test]
    fn rejects_empty_string() {
        assert_eq!(parse_major_version(""), None);
    }

    #[test]
    fn rejects_major_with_trailing_letters() {
        assert_eq!(parse_major_version("16beta.0"), None);
    }

    #[test]
    fn version_15_supports_nothing() {
        assert_eq!(
            capabilities_for_version("15.0.2"),
            Some(HostCapabilities::none())
        );
    }

    #[test]
    fn version_16_supports_only_multi_line_comments() {
        assert_eq!(
            capabilities_for_version("16.0.7"),
            Some(HostCapabilities {
                multi_line_comments: true,
                ..HostCapabilities::none()
            })
        );
    }

    #[test]
    fn version_18_supports_everything() {
        assert_eq!(
            capabilities_for_version("18.0.0"),
            Some(HostCapabilities::all())
        );
    }

    #[test]
    fn garbage_version_has_no_capabilities() {
        assert_eq!(capabilities_for_version("garbage"), None);
    }
}
