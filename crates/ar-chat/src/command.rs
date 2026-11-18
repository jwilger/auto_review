//! Parser for `@auto-review <command>` mentions in PR comments.
//!
//! The webhook handler in `ar-gateway` calls into here to decide what
//! a user's comment is asking for. Pure function; no I/O.

/// What a PR comment asks the bot to do.
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-gateway (webhook, github_webhook) and this crate's integration tests import \
              this as ar_chat::ChatCommand; renaming it breaks those imports"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCommand {
    /// `@auto-review autofix` — ask the cheap-tier model to emit
    /// inline patch suggestions for the diff. Each patch is posted
    /// as a Forgejo review comment with a `suggestion` fenced code
    /// block that the author can apply with one click.
    Autofix,
    /// `@auto-review docstring` (or `docstrings`, `docs`) — retired.
    /// `CodeRabbit` dropped docstring generation on 2026-11-11, which
    /// suggests the feature has little value, and here it only added
    /// prompt surface (design doc theme CLEAN, Decision 18). The
    /// parser keeps the aliases as a tombstone so the command does
    /// not fall through to free-form intent classification; the
    /// handler replies with a fixed notice and makes no LLM call.
    DocstringRetired,
    /// `@auto-review forget <id>` — drop a previously-remembered
    /// learning by its numeric id.
    Forget(u64),
    /// `@auto-review <anything else>` — falls through to freeform
    /// chat. The handler will pass `text` to the LLM.
    Freeform(String),
    /// `@auto-review help` — print the supported commands.
    Help,
    /// The comment doesn't mention the bot at all (or mentions a
    /// different name). Ignored.
    NotMentioned,
    /// `@auto-review re-review` (or `rereview`, `review-again`) —
    /// re-run the full review on the current head SHA, ignoring any
    /// recorded review history.
    ReReview,
    /// `@auto-review remember <text>` — store a learning that biases
    /// future reviews of this repo.
    Remember(String),
    /// A user correction replying to a bot review comment or finding.
    ReviewCorrection(String),
    /// `@auto-review tests` — find newly-added items in the diff
    /// that lack test coverage and post a markdown comment with
    /// scaffolded test cases the author can copy into their test
    /// suite. Unlike [`Self::Autofix`], tests usually
    /// live in a separate file, so we post a single issue comment
    /// rather than inline review-comment suggestions.
    TestScaffolds,
}

/// Parse a comment body looking for `@<bot_name> <command>` on any
/// line. The first matching line wins; subsequent lines are ignored.
///
/// Mention matching is case-insensitive (`@AUTO-REVIEW` reaches a bot
/// configured as `auto-review`). A bot configured as `auto-review` also
/// accepts `@auto_review` as a temporary compatibility alias.
/// Forgejo treats usernames as case-insensitive in its UI and links
/// either form to the same user, so the parser has to follow suit
/// or the bot would silently skip legitimate mentions.
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-gateway (webhook, github_webhook, poller), ar-cli (commands) and this crate's \
              integration tests call this as parse_chat_command; renaming it breaks them"
)]
#[inline]
#[must_use = "parsing has no side effects; the command is the only result"]
pub fn parse_chat_command(body: &str, bot_name: &str) -> ChatCommand {
    let mention = format!("@{bot_name}");
    let compatibility_mention = (bot_name == "auto-review").then_some("@auto_review");
    if let Some(correction) = parse_review_correction(body, &mention, compatibility_mention) {
        return ChatCommand::ReviewCorrection(correction);
    }
    for raw_line in body.lines() {
        let line = raw_line.trim();
        // Case-insensitive prefix check so @Auto-Review matches
        // @auto-review. The mention itself is ASCII (Forgejo
        // usernames are restricted to ASCII alphanumerics + `-_.`),
        // so eq_ignore_ascii_case is the right semantic.
        let after = if let Some(after) = strip_mention(line, &mention) {
            after
        } else if let Some(alias) = compatibility_mention {
            let Some(after) = strip_mention(line, alias) else {
                continue;
            };
            after
        } else {
            continue;
        };
        // Require a separator after the mention (whitespace, punctuation,
        // or end-of-line). Avoids matching "@auto_reviewer" against
        // bot_name="auto_review".
        let next = after.chars().next();
        match next {
            Some(ch) if !ch.is_whitespace() && !is_separator(ch) => continue,
            _ => {}
        }
        // Strip leading separators (`:` `,` `!` `?` `.`) and whitespace
        // before classification so "@auto-review: help" and "@auto-review,
        // help" route to Help, not Freeform(": help").
        let rest = after
            .trim_start_matches(|ch: char| ch.is_whitespace() || is_separator(ch))
            .trim_end();
        return classify(rest);
    }
    ChatCommand::NotMentioned
}

/// Return the rest of `line` after a case-insensitive `mention` prefix, or
/// `None` when the line does not start with it.
fn strip_mention<'line>(line: &'line str, mention: &str) -> Option<&'line str> {
    line.as_bytes()
        .get(..mention.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(mention.as_bytes()))
        .and_then(|_| line.get(mention.len()..))
}

/// Recognise a quote-reply to a bot finding (`@bot wrote in ...` followed by
/// quoted lines) and return the user's unquoted prose, if any.
fn parse_review_correction(
    body: &str,
    mention: &str,
    compatibility_mention: Option<&str>,
) -> Option<String> {
    let mut lines = body.lines();
    let first = lines.next()?.trim();
    let after =
        strip_mention(first, mention).or_else(|| strip_mention(first, compatibility_mention?))?;
    if !after.trim_start().starts_with("wrote in ") {
        return None;
    }
    // Any quote-reply to a bot finding with non-empty prose is a correction.
    // We deliberately do NOT gate on a hardcoded keyword list here — whether
    // the reply asks for approval, disputes a finding, or just asks a question
    // is decided downstream by an LLM intent classifier in the handler, so a
    // natural-language reply ("no, that's fine for a release PR") is understood
    // without the user having to phrase it a particular way.
    let correction = lines
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('>') {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!correction.is_empty()).then_some(correction)
}

/// Punctuation that may separate the mention from the command.
fn is_separator(ch: char) -> bool {
    matches!(ch, ':' | ',' | '!' | '?' | '.')
}

/// Map the text after the mention to a command.
fn classify(rest: &str) -> ChatCommand {
    if rest.is_empty() {
        return ChatCommand::Help;
    }
    let mut iter = rest.splitn(2, char::is_whitespace);
    let head = iter.next().unwrap_or("").to_ascii_lowercase();
    let tail = iter.next().unwrap_or("").trim();
    match head.as_str() {
        "remember" => {
            if tail.is_empty() {
                ChatCommand::Help
            } else {
                ChatCommand::Remember(tail.to_owned())
            }
        }
        "forget" => match tail.parse::<u64>() {
            Ok(id) => ChatCommand::Forget(id),
            Err(_) => ChatCommand::Help,
        },
        "re-review" | "rereview" | "review-again" | "review_again" => ChatCommand::ReReview,
        "autofix" | "auto-fix" | "fix" => ChatCommand::Autofix,
        "docstring" | "docstrings" | "docs" => ChatCommand::DocstringRetired,
        "tests" | "test" | "unit-tests" | "scaffold-tests" => ChatCommand::TestScaffolds,
        "help" | "?" | "--help" | "-h" => ChatCommand::Help,
        _ => ChatCommand::Freeform(rest.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> ChatCommand {
        parse_chat_command(body, "auto_review")
    }

    #[test]
    fn no_mention_returns_not_mentioned() {
        assert_eq!(parse("no bot here"), ChatCommand::NotMentioned);
        assert_eq!(parse(""), ChatCommand::NotMentioned);
        assert_eq!(parse("hi @other_bot remember X"), ChatCommand::NotMentioned);
    }

    #[test]
    fn remember_with_text_returns_remember() {
        assert_eq!(
            parse("@auto_review remember Always prefer total functions"),
            ChatCommand::Remember("Always prefer total functions".into())
        );
    }

    #[test]
    fn remember_without_text_returns_help() {
        assert_eq!(parse("@auto_review remember"), ChatCommand::Help);
        assert_eq!(parse("@auto_review remember   "), ChatCommand::Help);
    }

    #[test]
    fn forget_with_numeric_id_returns_forget() {
        assert_eq!(parse("@auto_review forget 42"), ChatCommand::Forget(42));
    }

    #[test]
    fn forget_with_non_numeric_returns_help() {
        assert_eq!(parse("@auto_review forget banana"), ChatCommand::Help);
        assert_eq!(parse("@auto_review forget"), ChatCommand::Help);
    }

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "`s` is the input under test in the assertion below; renaming it would edit that \
                  assertion"
    )]
    fn autofix_aliases_all_route_to_autofix() {
        for s in [
            "@auto_review autofix",
            "@auto_review auto-fix",
            "@auto_review fix",
            "@auto_review FIX",
        ] {
            assert_eq!(parse(s), ChatCommand::Autofix, "input = {s}");
        }
    }

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "`s` is the input under test in the assertion below; renaming it would edit that \
                  assertion"
    )]
    fn docstring_aliases_all_route_to_the_retired_tombstone() {
        for handle in ["@auto-review", "@auto_review"] {
            for alias in ["docstring", "docstrings", "docs", "DOCS"] {
                let s = format!("{handle} {alias}");
                assert_eq!(
                    parse_chat_command(&s, "auto-review"),
                    ChatCommand::DocstringRetired,
                    "input = {s}"
                );
            }
        }
    }

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "`s` is the input under test in the assertion below; renaming it would edit that \
                  assertion"
    )]
    fn tests_aliases_all_route_to_test_scaffolds() {
        for s in [
            "@auto_review tests",
            "@auto_review test",
            "@auto_review unit-tests",
            "@auto_review scaffold-tests",
            "@auto_review TESTS",
        ] {
            assert_eq!(parse(s), ChatCommand::TestScaffolds, "input = {s}");
        }
    }

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "`s` is the input under test in the assertion below; renaming it would edit that \
                  assertion"
    )]
    fn re_review_aliases_all_route_to_rereview() {
        for s in [
            "@auto_review re-review",
            "@auto_review rereview",
            "@auto_review review-again",
            "@auto_review review_again",
        ] {
            assert_eq!(parse(s), ChatCommand::ReReview, "input = {s}");
        }
    }

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "`s` is the input under test in the assertion below; renaming it would edit that \
                  assertion"
    )]
    fn help_aliases_all_route_to_help() {
        for s in [
            "@auto_review help",
            "@auto_review ?",
            "@auto_review --help",
            "@auto_review -h",
            "@auto_review",
            "@auto_review:",
        ] {
            assert_eq!(parse(s), ChatCommand::Help, "input = {s}");
        }
    }

    #[test]
    fn unknown_command_falls_into_freeform() {
        assert_eq!(
            parse("@auto_review what does this function do?"),
            ChatCommand::Freeform("what does this function do?".into())
        );
    }

    #[test]
    fn mention_works_on_any_line_of_the_body() {
        let body = "Thanks for the review!\n\n@auto_review remember use Result, not panics";
        assert_eq!(
            parse(body),
            ChatCommand::Remember("use Result, not panics".into())
        );
    }

    #[test]
    fn first_mention_wins_subsequent_lines_ignored() {
        let body = "@auto_review help\n@auto_review remember X";
        assert_eq!(parse(body), ChatCommand::Help);
    }

    #[test]
    fn separators_after_mention_are_ok() {
        assert_eq!(parse("@auto_review: help"), ChatCommand::Help);
        assert_eq!(parse("@auto_review, help"), ChatCommand::Help);
    }

    #[test]
    fn similar_bot_name_does_not_false_match() {
        // @auto_reviewer is not @auto_review
        assert_eq!(
            parse("@auto_reviewer remember X"),
            ChatCommand::NotMentioned
        );
    }

    #[test]
    fn case_insensitive_command_keyword() {
        assert_eq!(
            parse("@auto_review REMEMBER something"),
            ChatCommand::Remember("something".into())
        );
        assert_eq!(parse("@auto_review HELP"), ChatCommand::Help);
    }

    #[test]
    fn case_insensitive_mention_matches_configured_bot_name() {
        // Forgejo treats usernames as case-insensitive; a user
        // typing @AUTO_REVIEW or @Auto_Review reaches the same
        // bot account as @auto_review. The parser MUST follow
        // suit or the bot silently ignores legitimate mentions.
        for body in [
            "@AUTO_REVIEW help",
            "@Auto_Review help",
            "@auto_REVIEW help",
            "@auto_review help",
        ] {
            assert_eq!(parse(body), ChatCommand::Help, "input = {body:?}");
        }
    }

    #[test]
    fn case_insensitive_mention_still_rejects_substring_extensions() {
        // "@AUTOREVIEWER" must NOT match @auto_review just because
        // a case-insensitive prefix matches — the separator check
        // still has to fire.
        assert_eq!(parse("@AUTOREVIEWER remember X"), ChatCommand::NotMentioned);
        assert_eq!(parse("@Auto_Reviewer help"), ChatCommand::NotMentioned);
    }

    #[test]
    fn quote_reply_without_keywords_is_still_a_review_correction() {
        // Previously this required a hardcoded keyword (fine/accept/wrong/...).
        // Now any quote-reply with prose is a correction; intent is judged
        // downstream by the handler.
        let body = "@auto-review wrote in https://example.com/pulls/1#issuecomment-2:\n\
             > PR metadata quality: failed\n\
             \n\
             This is a release PR, so the terse body is expected.";
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "every variant other than ReviewCorrection is a test failure, reported by \
                      the catch-all panic arm"
        )]
        match parse_chat_command(body, "auto-review") {
            ChatCommand::ReviewCorrection(text) => {
                assert_eq!(text, "This is a release PR, so the terse body is expected.");
            }
            #[expect(
                clippy::panic,
                reason = "an unexpected command must fail this test, printing the variant it got"
            )]
            other => panic!("expected ReviewCorrection, got {other:?}"),
        }
    }

    #[test]
    fn quote_reply_with_no_user_prose_is_not_a_correction() {
        // A bare quote with nothing beneath it is not a correction; it falls
        // through to the normal mention parser.
        let body = "@auto-review wrote in https://example.com/pulls/1#issuecomment-2:\n\
             > PR metadata quality: failed";
        assert!(!matches!(
            parse_chat_command(body, "auto-review"),
            ChatCommand::ReviewCorrection(_)
        ));
    }

    #[test]
    fn custom_bot_name_is_supported() {
        let cmd = parse_chat_command("@reviewer help", "reviewer");
        assert_eq!(cmd, ChatCommand::Help);
    }

    #[test]
    fn hyphenated_bot_identity_accepts_public_mention_and_temporary_underscore_alias() {
        assert_eq!(
            parse_chat_command("@auto-review help", "auto-review"),
            ChatCommand::Help,
            "the public Forgejo bot identity is hyphenated and must be recognized"
        );
        assert_eq!(
            parse_chat_command("@auto_review help", "auto-review"),
            ChatCommand::Help,
            "keep @auto_review as a temporary compatibility alias while deployments transition"
        );
    }
}
