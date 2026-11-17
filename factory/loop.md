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

To let the factory land a `needs-review` pull request, add the
`factory:approved` label to it after reviewing its current head. Factory
agents run as your own account, so you author every factory pull request, and
GitHub does not let an author approve their own pull request: the label is
your approval. Only a `## Roles` account in `factory/policy.md` (as `main` has
it) counts, and the label counts only when you applied it after the branch's
newest own commit. An approving review works the same way, but only from
another `## Roles` account that did not author the pull request, and only
while GitHub keeps it (the ruleset dismisses stale reviews on push, which may
include the factory's merge of `main`), so prefer the label.

The next reconcile merges current `main` into the branch with a merge
commit, runs `nix develop .#ci --command just ci`, pushes with
`nix develop .#ci --command git push`, waits (bounded) for the required
checks on that head, and squash-merges pinned to it (`--match-head-commit`).
A conflict, a red gate or a check the change failed removes the label,
comments on the pull request and requeues the item for rework, which needs a
fresh approval; a transient check failure is rerun instead. The factory
removes the label before any push other than a clean merge of `main`
(rework, requeue or resume), so it never sits beside unreviewed commits. The
factory never adds `factory:approved` itself; the merge guard hook blocks
agents from adding it.

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
  lefthook pre-push gate has its toolchain. Nothing skips that hook. To
  audit this, `rg -n 'git push' .claude factory | rg -v 'nix develop .#ci --command'`
  lists only the deny rules and the push guard in `.claude/settings.json`,
  which forbid force pushes and pushes to `main` rather than make a push.

## Checks that guard the factory

`just ci` includes two checks added after factory runs broke the repository:
`just hermetic-git` fails when test code under `crates/` spawns `git` without
clearing `GIT_DIR`, `GIT_WORK_TREE` and `GIT_INDEX_FILE` (or using the shared
hermetic helper), and `just conflict-markers` fails when a tracked file holds
a `<<<<<<< ` or `>>>>>>> ` conflict marker line.

## Cloud routine (later)

Not set up yet. Before adding `routine` to `runners` in
`factory/factory.yaml`, confirm a cloud session can run
`nix develop .#ci --command just ci`. The routine's prompt is `/factory next`;
its schedule and the `gh` credentials it uses are chosen when it is created.
