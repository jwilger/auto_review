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
/// One optional leading `v` is allowed. Returns `None` when the string does
/// not start with a numeric major component.
#[must_use = "parsing the version has no side effect"]
#[inline]
pub fn parse_major_version(version: &str) -> Option<u64> {
    let trimmed = version.trim();
    let unprefixed = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let major = unprefixed
        .split(['.', '-', '+'])
        .next()
        .filter(|major| !major.is_empty() && major.bytes().all(|byte| byte.is_ascii_digit()))?;
    major.parse().ok()
}

/// Capabilities of a Forgejo server reporting `version`. An unparseable
/// version reports none.
#[expect(
    clippy::module_name_repetitions,
    reason = "public path ar_forgejo::capabilities::capabilities_for_version; renaming it \
              changes the crate's exported API"
)]
#[must_use = "computing capabilities has no side effect"]
#[inline]
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

    const NONE: HostCapabilities = HostCapabilities {
        multi_line_comments: false,
        suggestion_blocks: false,
        thread_replies: false,
        resolve_threads: false,
    };
    const MULTI_LINE_ONLY: HostCapabilities = HostCapabilities {
        multi_line_comments: true,
        ..NONE
    };

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
    fn parses_major_after_a_leading_v() {
        assert_eq!(parse_major_version("v18.0.0"), Some(18));
    }

    #[test]
    fn rejects_major_with_trailing_letters() {
        assert_eq!(parse_major_version("16beta.0"), None);
    }

    #[test]
    #[expect(
        clippy::pattern_type_mismatch,
        reason = "the loop binds through the table's references so the assertion below keeps \
                  its `*expected` comparison as written"
    )]
    fn maps_each_version_to_its_capabilities() {
        let cases: &[(&str, Option<HostCapabilities>)] = &[
            ("15.0.1", Some(NONE)),
            ("16.0.7", Some(MULTI_LINE_ONLY)),
            ("17.0.0-100-2254cb0+gitea-1.22.0", Some(MULTI_LINE_ONLY)),
            ("18.0.0", Some(HostCapabilities::all())),
            ("19.2.3", Some(HostCapabilities::all())),
            ("v18.0.0", Some(HostCapabilities::all())),
            ("1.21.11-1", Some(NONE)),
            ("garbage", None),
            ("", None),
            (".16", None),
            ("v", None),
            ("vv18.0.0", None),
        ];

        for (version, expected) in cases {
            assert_eq!(
                capabilities_for_version(version),
                *expected,
                "version {version:?}"
            );
        }
    }
}
