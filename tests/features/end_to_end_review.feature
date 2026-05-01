Feature: End-to-end PR review on a real Forgejo instance
  As an operator self-hosting Forgejo with auto_review,
  I want auto_review to post LLM-generated review comments back to
  every opened pull request,
  so I get the same automated-reviewer experience as cloud users
  (CodeRabbit, Greptile) without sending source code to a SaaS.

  Background:
    Given a Forgejo instance running in a container
    And an admin user "admin" exists on it
    And a bot user "auto_review_bot" exists with a PAT
    And the auto_review gateway is running with that PAT and the
        webhook secret "shared-secret"
    And a stub LLM endpoint is reachable from the gateway

  Scenario: A pull_request:opened webhook produces a posted review
    Given a repository "alice/widgets" exists with a default branch
        and one commit
    And a webhook on that repo points at the gateway, signed with
        "shared-secret"
    And the stub LLM is configured to return one Warning finding on
        line 1 of "src/main.rs"
    When a pull request is opened against the default branch with
        one changed file "src/main.rs"
    Then within 60 seconds the gateway dispatches a review
    And the review is posted to Forgejo with one comment on
        "src/main.rs" line 1
    And the commit status on the head SHA is "success" with
        context "auto_review"

  Scenario: A webhook with the wrong HMAC secret is rejected
    Given a webhook on the repo signed with "wrong-secret"
    When that webhook fires
    Then the gateway responds with HTTP 401
    And no review is posted to Forgejo

  Scenario: The bot recovers from a transient LLM 5xx
    Given the stub LLM returns 503 on the first request and 200
        with one finding on the second
    When a pull request is opened
    Then the gateway eventually posts the review with the finding
        from the successful LLM call
    And the commit status reflects success
