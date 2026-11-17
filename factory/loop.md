# Running the factory

The factory runs as whoever starts it: the machine's git identity and `gh`
login. Every run first checks the halt switch in `factory/policy.md`.

## Local (current runner)

From a Claude Code session in this repository:

```text
/factory next        # take the next ready item
/factory <issue>     # run one issue
/factory status      # items by status, and recent metrics
/factory halt        # stop all runs
```

Unattended on this machine, while a session stays open:

```text
/loop 30m /factory next
```

Each pass reconciles open factory pull requests first, then starts at most
one item (`loops.deliver.max_concurrent` in `factory/factory.yaml`). To stop
the loop, open an issue labelled `factory:halt`, or create `factory/HALT` in
this checkout.

## What merges without a person

Every class but `factory` runs in `dark` mode, so an approved change
auto-merges once CI is green. The approve station sends a change to a person
(`needs-review`) only for a `factory`-class change, a spec decision marked
`needs_human` (a product, policy or security choice with no clearly
recommendable option), or review that used every allowed round. Other spec
decisions, such as an ADR number or reply wording, are listed in the pull
request body's `## Decisions` section and do not hold the merge.

## Pull requests waiting for a person

`/factory next` starts nothing while `loops.deliver.max_awaiting_human` (2)
or more open `factory`-labelled pull requests have no auto-merge scheduled.
It prints one notice naming the cap and the waiting pull requests and stops,
so a running loop does not pile up work on an old `main`. Explicit
`/factory <issue>`, `approve` and `rework` are not capped.

The `factory:approved` label must exist in the repository, since neither
`gh pr edit --add-label` nor the pull request sidebar creates a missing
label. Create it once:

```sh
gh label create factory:approved --description 'maintainer approval for the factory to merge' --color 0E8A16
```

Reconcile also creates it when it is missing; creating the label approves
nothing.

To let the factory land a `needs-review` pull request outside the `factory`
class, add the `factory:approved` label to it after reviewing its current
head, or approve that head with a review from a `## Roles` account in
`factory/policy.md` that did not author it. Factory agents run as your own
account, so you author every factory pull request and GitHub does not let
you approve it: the label is your practical approval. The same fact means
GitHub cannot tell whether you or an agent applied the label or review, so
the signal is a convenience for you, not a security boundary.

The next `/factory next` reconcile, when a deliver slot is free, checks the
change is not `factory` class (empty or missing approver classes, or any
changed path matching a `factory` pattern in `main`'s `factory/policy.md`,
count as `factory`), then, in a throwaway `git worktree` rather than your
checkout, merges current `main` into the branch with a merge
commit (no rebase, no force push), runs `nix develop .#ci --command just ci`,
pushes with `nix develop .#ci --command git push`, waits for the required
checks on that head (`gh pr checks <n> --required --watch`), and
squash-merges pinned to it (`gh pr merge <n> --squash --match-head-commit
<sha>`). A conflicting merge of `main` is aborted before anything is
committed; a conflict, a red gate or a failed required check removes the
label, comments on the pull request and requeues the item for rework, which
needs a fresh approval. Any push other than a clean merge of `main` removes
the label, so it never sits beside unreviewed commits.

The factory never merges a `factory` class pull request, approved or not:
you review and merge it yourself. `/factory status` only reports approved
pull requests; it never runs the gate, pushes or merges. The merge guard hook
(`.claude/hooks/factory-merge-guard.sh`) denies agent merges of pull
requests touching `factory` paths unless the command carries
`FACTORY_MERGE_APPROVED=<pr>`. It is a tripwire against the factory's own
scripted mistakes, not a security boundary: anything running as your
account can still merge. The enforceable boundary is a separate factory
identity, tracked in issue #223.

## Branches and bases

- Item branches are `factory/<item>`. Slice branches are
  `factory-slice/<item>/<slice-id>` and design option branches
  `factory-option/<item>/option-<n>`, outside the item branch's ref path, so
  they can coexist with it.
- Every build starts from current `origin/main`; a resumed or reworked branch
  merges current `main` first. Before any merge or auto-merge is scheduled,
  the factory merges current `main` into the branch again and reruns the gate.
- Item branches never carry `.claude/agent-memory/**`; a build or merge whose
  diff contains such a path is sent back for rework.
- Every push runs as `nix develop .#ci --command git push ...`, so the
  lefthook pre-push gate has its toolchain. Nothing skips that hook. The
  only push commands under `.claude` and `factory` without that prefix are
  the deny rules and the push guard in `.claude/settings.json`, which forbid
  force pushes and pushes to `main` rather than make a push.

## Checks that guard the factory

Two parts of `just ci` exist because factory runs broke the repository.
`just test` runs the suite with `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
`GIT_COMMON_DIR`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`
and `GIT_PREFIX` unset, so a test that spawns `git` under the pre-push hook
cannot reach this repository (a fixture's `git init` once set
`core.bare = true` here). `just conflict-markers` fails when a tracked file
holds a `<<<<<<< ` or `>>>>>>> ` conflict marker line.

## Cloud routine (later)

Not set up yet. Before adding `routine` to `runners` in
`factory/factory.yaml`, confirm a cloud session can run
`nix develop .#ci --command just ci`. The routine's prompt is `/factory next`;
its schedule and the `gh` credentials it uses are chosen when it is created.
