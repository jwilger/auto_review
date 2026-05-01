# Forgejo runner: persistent Nix store

`auto_review`'s CI is one nix-flake job (`nix flake check`) that
rebuilds the cargo dep tree from source on every cold run — about
12 minutes of wall time. The dev-machine equivalent finishes in
~30 seconds because the host's `/nix/store` carries the realised
derivations across runs. To get the same speedup in CI, mount
`/nix` as a persistent host volume on the Forgejo Actions runner.

This is a **one-time runner-host config change**. After it lands,
every `auto_review` job (and every other repo using the same
runner with the same volume) reuses the cached Nix store.

## Apply on the runner host

Edit the runner's `act_runner.yaml` (default: `/etc/forgejo-runner/
act_runner.yaml`) and update the `container:` section:

```yaml
container:
  # Allow the workflow's host volume mount.
  valid_volumes:
    - /var/cache/forgejo-runner-nix

  # Mount the volume on every container the runner spawns. The
  # workflow itself does NOT need to declare this volume — the
  # runner config injects it for all jobs.
  options: -v /var/cache/forgejo-runner-nix:/nix
```

Create the cache directory and restart the runner:

```bash
sudo install -d -m 0755 /var/cache/forgejo-runner-nix
sudo systemctl restart forgejo-runner   # or whatever your unit is
```

## What this gives you

- **First run on the new volume**: same speed as today (~12 min;
  cold cargo build, populates `/nix/store`).
- **Every run after**: ~30 seconds — `nix flake check` walks the
  flake outputs, finds every dep-layer derivation already
  realised in the store, and reports `all checks passed!` without
  rebuilding anything.
- **`Cargo.lock` / `flake.lock` change**: only the changed
  derivations rebuild; the rest of the store stays warm.

## Multi-tenant concerns

The `options:` field above applies to **every** container the
runner spawns, not just `auto_review` jobs. Other repos sharing
the runner will:

- See `/nix` in their containers. If they don't use Nix, this is
  harmless (it's just a directory).
- Be able to read the cached store. If you run untrusted PRs
  from other repos through the same runner, they could
  theoretically poison the cache. If that's a concern, dedicate
  a runner instance to `auto_review` and apply this config only
  there.

## Alternative: per-job volume via workflow

If you'd rather scope the volume per workflow rather than
runner-wide, drop the `options:` line above (keep
`valid_volumes:`) and add to `.forgejo/workflows/ci.yml`:

```yaml
jobs:
  flake-check:
    runs-on: docker
    container:
      image: <runner-default>   # the runner picks for `docker`
      volumes:
        - /var/cache/forgejo-runner-nix:/nix
```

This keeps other repos unaffected at the cost of having to name
a specific image in the workflow.

## Rollback

Revert the `act_runner.yaml` change and restart the runner. The
workflow's idempotent install step handles both states — when
the volume isn't there, it falls back to installing Nix fresh
each run (status quo).

## Why not bake an image instead

The trade-off was discussed in the dev thread: a `dockerTools.
streamLayeredImage` with cargoArtifacts pre-baked would also
work, but adds:

- A registry (Forgejo's container registry, docker.io, ghcr.io)
- A manual rebuild + push step on every `Cargo.lock` change
- ~1–2 GB image storage cost

The persistent-volume approach is one runner-host config line
and zero registry overhead. If you ever outgrow it (e.g.,
multiple runner hosts, or PR-from-fork CI where you can't
trust the cache), revisit the image approach.
