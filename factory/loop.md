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

## Cloud routine (later)

Not set up yet. Before adding `routine` to `runners` in
`factory/factory.yaml`, confirm a cloud session can run
`nix develop .#ci --command just ci`. The routine's prompt is `/factory next`;
its schedule and the `gh` credentials it uses are chosen when it is created.
