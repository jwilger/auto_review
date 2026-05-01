//! Parser for `@auto_review <command>` mentions in PR comments.
//!
//! The webhook handler in `ar-gateway` calls into here to decide what
//! a user's comment is asking for. Pure function; no I/O.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCommand {
    /// `@auto_review remember <text>` — store a learning that biases
    /// future reviews of this repo.
    Remember(String),
    /// `@auto_review forget <id>` — drop a previously-remembered
    /// learning by its numeric id.
    Forget(u64),
    /// `@auto_review re-review` (or `rereview`, `review-again`) —
    /// re-run the full review on the current head SHA, ignoring any
    /// recorded review history.
    ReReview,
    /// `@auto_review autofix` — ask the cheap-tier model to emit
    /// inline patch suggestions for the diff. Each patch is posted
    /// as a Forgejo review comment with a `\`\`\`suggestion` block
    /// that the author can apply with one click.
    Autofix,
    /// `@auto_review help` — print the supported commands.
    Help,
    /// `@auto_review <anything else>` — falls through to freeform
    /// chat. The handler will pass `text` to the LLM.
    Freeform(String),
    /// The comment doesn't mention the bot at all (or mentions a
    /// different name). Ignored.
    NotMentioned,
}

/// Parse a comment body looking for `@<bot_name> <command>` on any
/// line. The first matching line wins; subsequent lines are ignored.
pub fn parse_chat_command(body: &str, bot_name: &str) -> ChatCommand {
    let mention = format!("@{bot_name}");
    for raw_line in body.lines() {
        let line = raw_line.trim();
        let Some(after) = line.strip_prefix(&mention) else {
            continue;
        };
        // Require a separator after the mention (whitespace, punctuation,
        // or end-of-line). Avoids matching "@auto_reviewer" against
        // bot_name="auto_review".
        let next = after.chars().next();
        match next {
            Some(c) if !c.is_whitespace() && !is_separator(c) => continue,
            _ => {}
        }
        // Strip leading separators (`:` `,` `!` `?` `.`) and whitespace
        // before classification so "@auto_review: help" and "@auto_review,
        // help" route to Help, not Freeform(": help").
        let rest = after
            .trim_start_matches(|c: char| c.is_whitespace() || is_separator(c))
            .trim_end();
        return classify(rest);
    }
    ChatCommand::NotMentioned
}

fn is_separator(c: char) -> bool {
    matches!(c, ':' | ',' | '!' | '?' | '.')
}

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
                ChatCommand::Remember(tail.to_string())
            }
        }
        "forget" => match tail.parse::<u64>() {
            Ok(id) => ChatCommand::Forget(id),
            Err(_) => ChatCommand::Help,
        },
        "re-review" | "rereview" | "review-again" | "review_again" => ChatCommand::ReReview,
        "autofix" | "auto-fix" | "fix" => ChatCommand::Autofix,
        "help" | "?" | "--help" | "-h" => ChatCommand::Help,
        _ => ChatCommand::Freeform(rest.to_string()),
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
    fn custom_bot_name_is_supported() {
        let cmd = parse_chat_command("@reviewer help", "reviewer");
        assert_eq!(cmd, ChatCommand::Help);
    }
}
