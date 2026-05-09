# Forgejo

This repo uses Forgejo at `git.johnwilger.com`, not GitHub. Use `tea` for issues and pull requests. Do not introduce `gh` workflows.

Inline review feedback must be answered on the inline thread first. Start each reply by @-tagging the user whose comment is being answered, using the comment author's Forgejo login. For Forgejo API replies, copy the original comment `position` into the reply payload as `new_position` and set `old_position` to `0`.
