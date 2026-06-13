//! Pure helpers for the reversible human-override marker stamped onto a PR.
//!
//! When an authorized human forces an approval over auto-review's outstanding
//! findings, auto-review records that fact ON the PR itself — a short title
//! prefix plus a sentinel-delimited body section — so it rides into the squash
//! commit a maintainer creates from the PR title/body (auto-review never
//! merges). The marker must be reversible: if a later push genuinely addresses
//! the findings and auto-review approves cleanly, the marker is stripped back
//! out. These functions are deterministic and idempotent so apply/strip can be
//! called without first inspecting the current state.

/// Short, scannable title prefix. Kept terse so it reads clearly in a list of
/// commit titles while leaving room for the real title.
pub const TITLE_MARKER: &str = "[override-approved] ";

/// Sentinel delimiters around the body section. HTML comments so the markers
/// are invisible in rendered markdown yet trivially machine-detectable,
/// independent of the surrounding body content.
const BODY_START: &str = "<!-- auto-review:override:start -->";
const BODY_END: &str = "<!-- auto-review:override:end -->";

/// Whether `title` already carries the override marker.
pub fn title_has_marker(title: &str) -> bool {
    title.starts_with(TITLE_MARKER)
}

/// Prepend the override marker to `title`, unless it is already present.
pub fn apply_title_marker(title: &str) -> String {
    if title_has_marker(title) {
        title.to_string()
    } else {
        format!("{TITLE_MARKER}{title}")
    }
}

/// Remove the override marker prefix from `title`, if present.
pub fn strip_title_marker(title: &str) -> String {
    title.strip_prefix(TITLE_MARKER).unwrap_or(title).to_string()
}

/// Whether `body` already contains the override section.
pub fn body_has_section(body: &str) -> bool {
    body.contains(BODY_START) && body.contains(BODY_END)
}

fn build_block(inner: &str) -> String {
    format!("{BODY_START}\n{}\n{BODY_END}", inner.trim())
}

/// Add or replace the override section in `body`. `inner` is the section's
/// markdown content (e.g. a `## Approval override` heading plus the reason and
/// overridden findings). Idempotent: calling it again replaces the existing
/// block rather than nesting a second one.
pub fn apply_body_section(body: &str, inner: &str) -> String {
    let block = build_block(inner);
    let base = strip_body_section(body);
    if base.trim().is_empty() {
        block
    } else {
        format!("{}\n\n{block}", base.trim_end())
    }
}

/// Remove the override section (and the blank lines hugging it) from `body`.
/// A no-op when no section is present.
pub fn strip_body_section(body: &str) -> String {
    let (Some(start), Some(end_pos)) = (body.find(BODY_START), body.find(BODY_END)) else {
        return body.to_string();
    };
    if end_pos < start {
        return body.to_string();
    }
    let end = end_pos + BODY_END.len();
    let before = body[..start].trim_end_matches(['\n', '\r', ' ', '\t']);
    let after = body[end..].trim_start_matches(['\n', '\r', ' ', '\t']);
    match (before.is_empty(), after.is_empty()) {
        (true, _) => after.to_string(),
        (false, true) => before.to_string(),
        (false, false) => format!("{before}\n\n{after}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_title_marker_prepends_once_and_is_idempotent() {
        let once = apply_title_marker("chore(release): v0.8.1");
        assert_eq!(once, "[override-approved] chore(release): v0.8.1");
        // Applying again does not double the marker.
        assert_eq!(apply_title_marker(&once), once);
        assert!(title_has_marker(&once));
    }

    #[test]
    fn strip_title_marker_removes_marker_and_is_noop_when_absent() {
        assert_eq!(
            strip_title_marker("[override-approved] fix: thing"),
            "fix: thing"
        );
        assert_eq!(strip_title_marker("fix: thing"), "fix: thing");
    }

    #[test]
    fn title_round_trip_restores_original() {
        let original = "feat(api): add endpoint";
        assert_eq!(strip_title_marker(&apply_title_marker(original)), original);
    }

    #[test]
    fn apply_body_section_appends_block_with_inner_content() {
        let body = "Original description.";
        let out = apply_body_section(body, "## Approval override\nReason: it is fine.");
        assert!(out.starts_with("Original description."));
        assert!(body_has_section(&out));
        assert!(out.contains("## Approval override"));
        assert!(out.contains("Reason: it is fine."));
    }

    #[test]
    fn apply_body_section_into_empty_body_is_just_the_block() {
        let out = apply_body_section("", "## Approval override\nReason: x.");
        assert!(body_has_section(&out));
        assert!(out.starts_with(BODY_START));
    }

    #[test]
    fn apply_body_section_is_idempotent_and_replaces_existing_block() {
        let body = "Desc.";
        let first = apply_body_section(body, "## Approval override\nReason: first.");
        let second = apply_body_section(&first, "## Approval override\nReason: second.");
        // Exactly one block, with the updated content.
        assert_eq!(second.matches(BODY_START).count(), 1);
        assert_eq!(second.matches(BODY_END).count(), 1);
        assert!(second.contains("Reason: second."));
        assert!(!second.contains("Reason: first."));
    }

    #[test]
    fn strip_body_section_restores_base_and_is_noop_when_absent() {
        let body = "Original description.";
        let with = apply_body_section(body, "## Approval override\nReason: y.");
        assert_eq!(strip_body_section(&with), body);
        // No-op when there is no section.
        assert_eq!(strip_body_section(body), body);
    }

    #[test]
    fn body_round_trip_restores_original() {
        let original = "Some PR body\n\nwith multiple paragraphs.";
        let out = apply_body_section(original, "## Approval override\nReason: z.");
        assert_eq!(strip_body_section(&out), original);
    }
}
